use crate::{
    btree::{DeleteOutcome, InsertOutcome},
    storage::{
        pager::Pager,
        pages::{
            Page,
            btree_page::BTreePage,
            spec::PageSpec,
            table::{
                interior::{TableInteriorCell, TableInteriorPage},
                leaf::{TableLeafCell, TableLeafPage},
            },
            types::{PageNumber, RowID},
        },
    },
};

pub struct TableCursor<'a> {
    pub pager: &'a mut Pager,
    root_pn: PageNumber,
}

impl<'a> TableCursor<'a> {
    pub fn new(pager: &'a mut Pager, root_pn: PageNumber) -> Self {
        Self { pager, root_pn }
    }

    pub fn seek(&mut self, row_id: RowID) -> anyhow::Result<Option<TableLeafCell>> {
        self.seek_from(self.root_pn, row_id)
    }

    pub fn scan(&mut self) -> anyhow::Result<Vec<TableLeafCell>> {
        let mut res = Vec::new();
        self.scan_from(self.root_pn, &mut res)?;
        Ok(res)
    }

    pub fn insert(&mut self, row_id: RowID, payload: &[u8]) -> anyhow::Result<()> {
        let res = self.insert_into(self.root_pn, row_id, payload)?;
        match res {
            InsertOutcome::Done => Ok(()),
            InsertOutcome::Split {
                new_child, divider, ..
            } => self.split_root(new_child, divider),
        }
    }

    pub fn delete(&mut self, row_id: RowID) -> anyhow::Result<()> {
        match self.delete_from(self.root_pn, row_id)? {
            DeleteOutcome::Done => self.shrink_root(),
            DeleteOutcome::DeletedChild => {
                anyhow::bail!(
                    "root reported its own death: shrink_root should have kept it non-empty"
                )
            }
        }
    }

    /// deletes all pages of a table btree by freeing all its pages and overwriting the root.
    pub fn delete_all(&mut self) -> anyhow::Result<()> {
        self.delete_all_from(self.root_pn)?;

        // overwrite root page with an empty leaf cell.
        self.pager.write_page(
            self.root_pn,
            Page::TableLeaf(TableLeafPage::new(vec![])).serialize(self.pager.spec(self.root_pn))?,
        )?;

        Ok(())
    }

    /// fetches the current maximum row_id in the tree, if empty it returns page number 1.
    pub fn get_max_row_id(&mut self) -> anyhow::Result<RowID> {
        self.get_max_row_id_from(self.root_pn)
    }

    fn fetch_page(&mut self, page_number: PageNumber) -> anyhow::Result<Page> {
        let ps = self.pager.spec(page_number);
        let page_bytes = match self.pager.get_page(page_number) {
            Ok(b) => b,
            Err(e) => anyhow::bail!("no page with page number {page_number}: {e}"),
        };

        Page::parse(page_bytes, ps)
    }

    fn seek_from(
        &mut self,
        page_number: PageNumber,
        row_id: RowID,
    ) -> anyhow::Result<Option<TableLeafCell>> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(lp) => {
                match lp.cells.binary_search_by(|cell| cell.row_id.cmp(&row_id)) {
                    Ok(idx) => {
                        let mut leaf = lp.cells[idx].clone();
                        leaf.local_payload = self.materialize(&leaf)?;
                        Ok(Some(leaf))
                    }
                    Err(_) => Ok(None),
                }
            }
            Page::TableInterior(ip) => {
                let next = ip.child_of(row_id);
                self.seek_from(next, row_id)
            }
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected a table page, found an index page"
                )
            }
        }
    }

    fn scan_from(
        &mut self,
        page_number: PageNumber,
        scanned: &mut Vec<TableLeafCell>,
    ) -> anyhow::Result<()> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(lp) => {
                for mut cell in lp.cells.into_iter() {
                    match cell.overflow_pn {
                        None => scanned.push(cell),
                        Some(_) => {
                            cell.local_payload = self.materialize(&cell)?;
                            scanned.push(cell);
                        }
                    }
                }
            }
            Page::TableInterior(ip) => {
                for cell in ip.cells.iter() {
                    self.scan_from(cell.left_child, scanned)?;
                }
                self.scan_from(ip.rightmost_child, scanned)?;
            }
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected a table page, found an index page"
                )
            }
        }
        Ok(())
    }

    fn insert_into(
        &mut self,
        page_number: PageNumber,
        row_id: RowID,
        payload: &[u8],
    ) -> anyhow::Result<InsertOutcome<RowID>> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(lp) => {
                if lp.contains(row_id) {
                    anyhow::bail!("row_id already exists")
                }
                let cap = self.pager.spec(page_number).max_local_payload_len()?;
                let new_cell = if payload.len() <= cap {
                    TableLeafCell::new(row_id, payload)
                } else {
                    let overflow_pn = self.pager.write_overflow(&payload[cap..])?;
                    TableLeafCell::with_overflow(
                        row_id,
                        payload[..cap].to_vec(),
                        payload.len(),
                        overflow_pn,
                    )
                };
                let overflow_pn = new_cell.overflow_pn;
                match self.insert_or_split(new_cell, lp, page_number) {
                    Ok(res) => Ok(res),
                    Err(e) => {
                        self.pager.free_overflow(overflow_pn)?;
                        Err(e)
                    }
                }
            }
            Page::TableInterior(mut ip) => {
                let next = ip.child_of(row_id);
                match self.insert_into(next, row_id, payload)? {
                    InsertOutcome::Done => Ok(InsertOutcome::Done),
                    InsertOutcome::Split {
                        old_child,
                        new_child,
                        divider,
                    } => {
                        repoint_child(&mut ip, old_child, new_child)?;
                        let new_cell = TableInteriorCell::new(old_child, divider);
                        self.insert_or_split(new_cell, ip, page_number)
                    }
                }
            }
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected a table page, found an index page"
                )
            }
        }
    }

    fn insert_or_split<P: BTreePage>(
        &mut self,
        new_cell: P::C,
        mut page: P,
        page_number: PageNumber,
    ) -> anyhow::Result<InsertOutcome<P::Key>> {
        let spec = self.pager.spec(page_number);

        if page.fits(&new_cell, self.pager.spec(page_number))? {
            page.insert(new_cell, self.pager.spec(page_number))?;
            self.pager
                .write_page(page_number, page.serialize(self.pager.spec(page_number))?)?;
            return Ok(InsertOutcome::Done);
        }

        // a new page is never page 1
        let right_spec = PageSpec::fresh(self.pager.page_size);
        let (left, right, divider) = page.split_with(new_cell, spec, right_spec)?;

        let left_bytes = left.serialize(spec)?;
        let right_bytes = right.serialize(right_spec)?;
        let new_pn = self.pager.allocate_page()?;

        self.pager.write_page(page_number, left_bytes)?;
        self.pager.write_page(new_pn, right_bytes)?;
        Ok(InsertOutcome::Split {
            old_child: page_number,
            new_child: new_pn,
            divider,
        })
    }

    fn split_root(&mut self, right_child: PageNumber, divider: RowID) -> anyhow::Result<()> {
        let root_pn = self.root_pn;
        let root_bytes = self.pager.get_page(root_pn)?.to_vec();
        // parse before allocating
        let left = Page::parse(&root_bytes, self.pager.spec(root_pn))?;

        let left_pn = self.pager.allocate_page()?;
        let left_bytes = left.serialize(self.pager.spec(left_pn))?;
        self.pager.write_page(left_pn, left_bytes)?;

        let new_root =
            TableInteriorPage::new(vec![TableInteriorCell::new(left_pn, divider)], right_child);
        let new_root_bytes = new_root.serialize(self.pager.spec(root_pn))?;
        // write_page stamps the header when root_pn == 1
        self.pager.write_page(root_pn, new_root_bytes)
    }

    fn delete_from(
        &mut self,
        page_number: PageNumber,
        row_id: RowID,
    ) -> anyhow::Result<DeleteOutcome> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(mut lp) => {
                let deleted = lp.delete(row_id)?;
                self.pager.free_overflow(deleted.overflow_pn)?;
                if lp.cells().is_empty() && page_number != self.root_pn {
                    return Ok(DeleteOutcome::DeletedChild);
                }
                self.pager
                    .write_page(page_number, lp.serialize(self.pager.spec(page_number))?)?;
                Ok(DeleteOutcome::Done)
            }
            Page::TableInterior(mut ip) => {
                let child = ip.child_of(row_id);
                match self.delete_from(child, row_id)? {
                    DeleteOutcome::Done => Ok(DeleteOutcome::Done),
                    DeleteOutcome::DeletedChild => {
                        if ip.cells().is_empty() {
                            self.pager.free_page(child)?;
                            return Ok(DeleteOutcome::DeletedChild);
                        }
                        ip.delete_child(child)?;
                        self.pager
                            .write_page(page_number, ip.serialize(self.pager.spec(page_number))?)?;
                        self.pager.free_page(child)?;
                        Ok(DeleteOutcome::Done)
                    }
                }
            }
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected a table page, found an index page"
                )
            }
        }
    }

    fn delete_all_from(&mut self, page_number: PageNumber) -> anyhow::Result<()> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(lp) => {
                for cell in lp.cells() {
                    self.pager.free_overflow(cell.overflow_pn)?;
                }
                if page_number == self.root_pn {
                    return Ok(());
                }
                self.pager.free_page(page_number)?;
                Ok(())
            }
            Page::TableInterior(ip) => {
                for cell in ip.cells() {
                    self.delete_all_from(cell.left_child)?;
                }
                self.delete_all_from(ip.rightmost_child)?;
                if page_number == self.root_pn {
                    return Ok(());
                }
                self.pager.free_page(page_number)
            }
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected a table page, found an index page"
                )
            }
        }
    }

    fn get_max_row_id_from(&mut self, page_number: PageNumber) -> anyhow::Result<RowID> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(lp) => {
                if let Some(last) = lp.cells().last() {
                    Ok(last.row_id)
                } else {
                    Ok(RowID::try_from(1).unwrap())
                }
            },
            Page::TableInterior(ip) => self.get_max_row_id_from(ip.rightmost_child),
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected a table page, found an index page"
                )
            }
        }
    }

    fn shrink_root(&mut self) -> anyhow::Result<()> {
        loop {
            let root = self.fetch_page(self.root_pn)?;
            match root {
                Page::TableLeaf(_) => return Ok(()),
                Page::TableInterior(ip) if !ip.cells().is_empty() => return Ok(()),
                Page::TableInterior(root_ip) => {
                    let right_child = root_ip.rightmost_child;
                    let root_right_child_bytes = self
                        .fetch_page(right_child)?
                        .serialize(self.pager.spec(self.root_pn))?;
                    self.pager
                        .write_page(self.root_pn, root_right_child_bytes)?;
                    self.pager.free_page(right_child)?;
                }
                Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                    anyhow::bail!(
                        "fetched page {}: expected a table page, found an index page",
                        self.root_pn
                    )
                }
            }
        }
    }

    fn materialize(&mut self, cell: &TableLeafCell) -> anyhow::Result<Vec<u8>> {
        if cell.overflow_pn.is_none() {
            return Ok(cell.local_payload.clone());
        }
        let tail_len = cell.total_payload_len - cell.local_payload.len();
        let mut full = cell.local_payload.clone();
        full.extend(self.pager.read_overflow(cell.overflow_pn, tail_len)?);
        Ok(full)
    }
}

fn repoint_child(
    ip: &mut TableInteriorPage,
    old_child: PageNumber,
    new_child: PageNumber,
) -> anyhow::Result<()> {
    if let Some(cell) = ip.cells.iter_mut().find(|c| c.left_child == old_child) {
        cell.left_child = new_child;
    } else if ip.rightmost_child == old_child {
        ip.rightmost_child = new_child;
    } else {
        anyhow::bail!(
            "parent interior page does not have a cell pointing to a child page with page number {old_child}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btree::check::check_table_tree as check_tree;
    use crate::storage::pages::spec::PageSpec;
    use crate::storage::pages::table::interior::{TableInteriorCell, TableInteriorPage};
    use crate::storage::pages::table::leaf::TableLeafPage;

    const PAGE_SIZE: usize = 512;

    fn pn(n: u32) -> PageNumber {
        PageNumber::try_from(n).unwrap()
    }

    fn rid(n: u64) -> RowID {
        RowID::try_from(n).unwrap()
    }

    fn temp_db_path(tag: &str) -> String {
        let mut path = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("maze_cursor_test_{tag}_{unique}.mz"));
        path.to_str().unwrap().to_string()
    }

    fn leaf(row_ids: &[RowID]) -> TableLeafPage {
        TableLeafPage {
            cells: row_ids
                .iter()
                .map(|&row_id| TableLeafCell {
                    row_id,
                    local_payload: row_id.get().to_be_bytes().to_vec(),
                    total_payload_len: 8,
                    overflow_pn: None,
                })
                .collect(),
        }
    }

    fn write_leaf(pager: &mut Pager, page_number: PageNumber, row_ids: &[RowID]) {
        let bytes = Page::TableLeaf(leaf(row_ids))
            .serialize(PageSpec::new(PAGE_SIZE as usize, page_number))
            .unwrap();
        pager.write_page(page_number, bytes).unwrap();
    }

    fn write_interior(pager: &mut Pager, page_number: PageNumber, page: TableInteriorPage) {
        let bytes = Page::TableInterior(page)
            .serialize(PageSpec::new(PAGE_SIZE as usize, page_number))
            .unwrap();
        pager.write_page(page_number, bytes).unwrap();
    }

    // Builds this 3-level tree, rooted at page 1 on a real file-backed
    // Pager -- deliberately exercising the page-1 file-header-offset path
    // (not sidestepped the way the old HashMap fixture did), since that's
    // exactly the code path the serialize/parse symmetry fix was about.
    //
    //                      root (page 1)
    //                 cell: (page 5, key=30)      rightmost: page 6
    //                /                                        \
    //         mid1 (page 5)                              mid2 (page 6)
    //    cell: (page 10, key=3)  rightmost: 11      cell: (page 12, key=150)  rightmost: 13
    //        /          \                                /            \
    //  leaf 10        leaf 11                        leaf 12        leaf 13
    //  [1,2,3]       [10,20,30]                      [100,150]    [500,600,700]
    fn build_tree(path: &str) -> Pager {
        let mut pager = Pager::new(path, PAGE_SIZE).unwrap();

        write_leaf(
            &mut pager,
            pn(10),
            &[
                RowID::try_from(1).unwrap(),
                RowID::try_from(2).unwrap(),
                RowID::try_from(3).unwrap(),
            ],
        );
        write_leaf(
            &mut pager,
            pn(11),
            &[
                RowID::try_from(10).unwrap(),
                RowID::try_from(20).unwrap(),
                RowID::try_from(30).unwrap(),
            ],
        );
        write_leaf(
            &mut pager,
            pn(12),
            &[RowID::try_from(100).unwrap(), RowID::try_from(150).unwrap()],
        );
        write_leaf(
            &mut pager,
            pn(13),
            &[
                RowID::try_from(500).unwrap(),
                RowID::try_from(600).unwrap(),
                RowID::try_from(700).unwrap(),
            ],
        );

        write_interior(
            &mut pager,
            pn(5),
            TableInteriorPage {
                cells: vec![TableInteriorCell {
                    left_child: pn(10),
                    row_id: RowID::try_from(3).unwrap(),
                }],
                rightmost_child: pn(11),
            },
        );
        write_interior(
            &mut pager,
            pn(6),
            TableInteriorPage {
                cells: vec![TableInteriorCell {
                    left_child: pn(12),
                    row_id: RowID::try_from(150).unwrap(),
                }],
                rightmost_child: pn(13),
            },
        );
        // overwrites the empty root leaf `Pager::new` creates at page 1.
        write_interior(
            &mut pager,
            pn(1),
            TableInteriorPage {
                cells: vec![TableInteriorCell {
                    left_child: pn(5),
                    row_id: RowID::try_from(30).unwrap(),
                }],
                rightmost_child: pn(6),
            },
        );

        pager
    }

    #[test]
    fn seek_finds_every_row_that_exists() {
        let path = temp_db_path("seek_finds");
        let mut pager = build_tree(&path);
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        for &row_id in &[1u64, 2, 3, 10, 20, 30, 100, 150, 500, 600, 700] {
            let found = cursor.seek(RowID::try_from(row_id).unwrap()).unwrap();
            assert_eq!(
                found.map(|c| c.row_id.get()),
                Some(row_id),
                "expected to find row {row_id}"
            );
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn seek_returns_none_for_rows_that_dont_exist() {
        let path = temp_db_path("seek_none");
        let mut pager = build_tree(&path);
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        // row id 0 can't even be constructed as a `RowID` (same rule as `PageNumber`),
        // so the "below everything" boundary case is rejected at construction, not at seek.
        assert!(RowID::try_from(0).is_err());

        // remaining cases: between leaves within one subtree, between the two
        // second-level subtrees, and above everything.
        for &row_id in &[4u64, 15, 50, 99, 151, 999] {
            assert_eq!(
                cursor.seek(rid(row_id)).unwrap(),
                None,
                "expected row {row_id} to be absent"
            );
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn insert_100_ascending_rows_all_remain_reachable() {
        let path = temp_db_path("insert_100_ascending");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        for i in 1u64..=100 {
            cursor.insert(rid(i), &i.to_be_bytes()).unwrap();
        }

        for i in 1u64..=100 {
            assert_eq!(
                cursor.seek(rid(i)).unwrap().map(|c| c.row_id.get()),
                Some(i),
                "row {i} should be findable via seek after inserting 100 ascending rows"
            );
        }

        let scanned: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect();
        assert_eq!(scanned, (1u64..=100).collect::<Vec<u64>>());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn scan_returns_every_row_in_ascending_order() {
        let path = temp_db_path("scan_order");
        let mut pager = build_tree(&path);
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        let got: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect();

        assert_eq!(got, vec![1, 2, 3, 10, 20, 30, 100, 150, 500, 600, 700]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn insert_survives_reopen() {
        let path = temp_db_path("reopen");
        {
            let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            for i in 1u64..=200 {
                cursor.insert(rid(i), &i.to_be_bytes()).unwrap();
            }
        }
        let mut pager = Pager::init(&path).unwrap();
        let rows = TableCursor::new(&mut pager, pn(1)).scan().unwrap();
        assert_eq!(rows.len(), 200);
        std::fs::remove_file(&path).ok();
    }

    fn shuffled(n: u64) -> Vec<u64> {
        // xorshift64, no rand crate
        let mut v: Vec<u64> = (1..=n).collect();
        let mut x = 0x2545F4914F6CDD1Du64;
        for i in (1..v.len()).rev() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.swap(i, (x % (i as u64 + 1)) as usize);
        }
        v
    }

    #[test]
    fn insert_5000_rows_all_remain_reachable() {
        let path = temp_db_path("insert_5000_ascending");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        let data = shuffled(5000);

        for elem in &data {
            cursor.insert(rid(*elem), &elem.to_be_bytes()).unwrap();
        }
        for elem in &data {
            let got = cursor.seek(rid(*elem)).unwrap().unwrap();
            assert_eq!(got.row_id.get(), *elem);
            assert_eq!(got.local_payload, elem.to_be_bytes()); // you currently only check row_id
        }

        let scanned: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect();
        assert_eq!(scanned.len(), 5000);
        assert_eq!(scanned, (1u64..=5000).collect::<Vec<u64>>());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn failed_inserts_do_not_grow_the_file() {
        let path = temp_db_path("no_leak");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        cursor.insert(rid(1), b"x").unwrap();
        let size = std::fs::metadata(&path).unwrap().len();

        assert!(cursor.insert(rid(1), b"dup").is_err()); // duplicate key
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        assert_eq!(cursor.scan().unwrap().len(), 1);

        // a payload bigger than a 512-byte page now succeeds via overflow pages: the local cap
        // is page_size / 4 = 128, so a 600-byte payload spills 472 bytes into exactly one
        // overflow page (its capacity is page_size - 4 = 508).
        let big = vec![0xABu8; 600];
        cursor.insert(rid(2), &big).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            size + PAGE_SIZE as u64
        );
        assert_eq!(cursor.seek(rid(2)).unwrap().unwrap().local_payload, big);

        std::fs::remove_file(&path).ok();
    }

    /// 30 small rows (row ids 2, 4, ..., 60, 10-byte payloads) fit in the root leaf with room
    /// to spare. Then one 300-byte row is inserted at `big_id`, which no longer fits and forces
    /// a split. Splitting the old cells 50/50 first and routing the big cell afterwards
    /// overflows whichever half receives it, so a correct split has to account for the big cell.
    fn insert_big_row_into_nearly_full_leaf(tag: &str, big_id: u64) {
        let path = temp_db_path(tag);
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();

        let mut expected: Vec<(u64, Vec<u8>)> =
            (1u64..=30).map(|i| (i * 2, vec![i as u8; 10])).collect();
        {
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            for (row_id, payload) in &expected {
                cursor.insert(rid(*row_id), payload).unwrap();
            }
        }
        // precondition: the 30 small rows still fit in the root leaf, so no split has happened
        assert_eq!(std::fs::metadata(&path).unwrap().len(), PAGE_SIZE as u64);

        let big = vec![0xABu8; 300];
        let mut cursor = TableCursor::new(&mut pager, pn(1));
        cursor.insert(rid(big_id), &big).unwrap();

        expected.push((big_id, big));
        expected.sort_by_key(|(id, _)| *id);

        // every row is reachable, with its exact payload
        for (id, payload) in &expected {
            let cell = cursor
                .seek(rid(*id))
                .unwrap()
                .unwrap_or_else(|| panic!("row {id} is missing after the split"));
            assert_eq!(
                &cell.local_payload, payload,
                "payload mismatch for row {id}"
            );
        }
        // and a scan returns all of them, in order
        let scanned: Vec<(u64, Vec<u8>)> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| (c.row_id.get(), c.local_payload))
            .collect();
        assert_eq!(scanned, expected);

        // the split really happened: page 1 is now an interior root, and the file grew by exactly
        // three pages (the new right half, the copy of the old root made by the root split, and
        // one overflow page for the 300-byte cell's tail beyond the 128-byte local cap)
        let root_bytes = pager.get_page(pn(1)).unwrap().to_vec();
        assert!(matches!(
            Page::parse(&root_bytes, PageSpec::new(PAGE_SIZE as usize, pn(1))).unwrap(),
            Page::TableInterior(_)
        ));
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            4 * PAGE_SIZE as u64
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn split_with_big_cell_near_start_of_full_leaf() {
        // after rows 2, 4, 6; the big cell lands in the left half of a 50/50 split
        insert_big_row_into_nearly_full_leaf("big_cell_start", 7);
    }

    #[test]
    fn split_with_big_cell_near_end_of_full_leaf() {
        // after row 52, before 54..60; the big cell lands in the right half of a 50/50 split
        insert_big_row_into_nearly_full_leaf("big_cell_end", 53);
    }

    #[test]
    fn big_cell_in_the_middle_of_a_full_leaf_now_succeeds_via_overflow() {
        // Before overflow pages existed, a 300-byte cell landing with 15 small cells on each
        // side needed a three-way split and was rejected. Overflow caps the cell's on-disk cost
        // to the local cap (128 bytes) + a 4-byte pointer, which a binary split can always
        // accommodate, so this now succeeds instead of erroring.
        let path = temp_db_path("big_cell_middle");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        let mut cursor = TableCursor::new(&mut pager, pn(1));
        for i in 1u64..=30 {
            cursor.insert(rid(i * 2), &[i as u8; 10]).unwrap();
        }

        let big = vec![0xABu8; 300];
        cursor.insert(rid(31), &big).unwrap();

        assert_eq!(cursor.seek(rid(31)).unwrap().unwrap().local_payload, big);
        assert_eq!(cursor.scan().unwrap().len(), 31);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tree_rooted_at_a_non_first_page_splits_and_stays_reachable() {
        let path = temp_db_path("non_first_root");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        write_leaf(&mut pager, pn(2), &[]); // an empty leaf root that is not page 1
        {
            let mut cursor = TableCursor::new(&mut pager, pn(2));
            for i in 1u64..=300 {
                cursor.insert(rid(i), &i.to_be_bytes()).unwrap();
            }
            for i in 1u64..=300 {
                assert_eq!(
                    cursor.seek(rid(i)).unwrap().map(|c| c.row_id.get()),
                    Some(i)
                );
            }
            let ids: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id.get())
                .collect();
            assert_eq!(ids, (1u64..=300).collect::<Vec<u64>>());
        }
        // the root's page number never moves, and it is now an interior page
        let root = pager.get_page(pn(2)).unwrap().to_vec();
        assert!(matches!(
            Page::parse(&root, PageSpec::new(PAGE_SIZE as usize, pn(2))).unwrap(),
            Page::TableInterior(_)
        ));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn ensure_5000_row_tree_stats_are_correct() {
        let path = temp_db_path("insert_5000_ascending");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        let data = shuffled(5000);

        for elem in &data {
            cursor.insert(rid(*elem), &elem.to_be_bytes()).unwrap();
        }

        let stats = check_tree(&mut pager, pn(1)).unwrap();
        assert_eq!(stats.rows, 5000);
        assert_eq!(
            // ( pages in tree        + pages leaked                     ) * size of a page = size of the database file.
            (stats.pages.len() as u64 + pager.count_fp().unwrap() as u64) * PAGE_SIZE as u64,
            std::fs::metadata(&path).unwrap().len(), // every page in the file belongs to the tree
        );
    }

    #[test]
    fn delete_removes_a_single_row_and_leaves_the_rest_reachable() {
        let path = temp_db_path("delete_single");
        let mut pager = build_tree(&path);
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        cursor.delete(rid(100)).unwrap();

        assert_eq!(cursor.seek(rid(100)).unwrap(), None);
        for &row_id in &[1u64, 2, 3, 10, 20, 30, 150, 500, 600, 700] {
            assert_eq!(
                cursor.seek(rid(row_id)).unwrap().map(|c| c.row_id.get()),
                Some(row_id),
                "expected row {row_id} to still be reachable"
            );
        }
        let scanned: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect();
        assert_eq!(scanned, vec![1, 2, 3, 10, 20, 30, 150, 500, 600, 700]);

        check_tree(&mut pager, pn(1)).unwrap();
        std::fs::remove_file(&path).ok();
    }

    // `build_tree`'s mid1 (page 5) has cell {left_child: 10, row_id: 3} plus
    // rightmost_child: 11 -- draining leaf 10 removes a *cell*, leaving
    // rightmost_child untouched. This is the code path the original bug's
    // `repoint_child(&mut grandparent, collapsed, survivor)` call handled
    // incorrectly when it fired at all; this test pins down that the
    // collapsed page now persists in place instead.
    #[test]
    fn draining_a_child_reached_via_a_cell_leaves_a_degenerate_sibling_with_rightmost_unchanged() {
        let path = temp_db_path("collapse_left_cell");
        let mut pager = build_tree(&path);
        {
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            for row_id in [1u64, 2, 3] {
                cursor.delete(rid(row_id)).unwrap();
            }

            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id.get())
                .collect();
            assert_eq!(scanned, vec![10, 20, 30, 100, 150, 500, 600, 700]);
        }

        let page5_bytes = pager.get_page(pn(5)).unwrap().to_vec();
        match Page::parse(&page5_bytes, pager.spec(pn(5))).unwrap() {
            Page::TableInterior(ip) => {
                assert!(ip.cells.is_empty(), "page 5 should now be degenerate");
                assert_eq!(
                    ip.rightmost_child,
                    pn(11),
                    "rightmost_child must be untouched by a cell-side collapse"
                );
            }
            Page::TableLeaf(_) => panic!("page 5 should still be an interior page"),
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                panic!("parsed page 5: expected a table page, found an index page")
            }
        }

        let stats = check_tree(&mut pager, pn(1)).unwrap();
        assert_eq!(stats.depth, 2, "tree must not have shrunk");
        std::fs::remove_file(&path).ok();
    }

    // Mirror of the test above, but draining leaf 11 -- reached via mid1's
    // `rightmost_child`, not a cell. `delete_child` must pop the one
    // remaining cell and promote its `left_child` into `rightmost_child`.
    #[test]
    fn draining_a_child_reached_via_rightmost_promotes_the_last_cell_into_rightmost() {
        let path = temp_db_path("collapse_rightmost");
        let mut pager = build_tree(&path);
        {
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            for row_id in [10u64, 20, 30] {
                cursor.delete(rid(row_id)).unwrap();
            }

            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id.get())
                .collect();
            assert_eq!(scanned, vec![1, 2, 3, 100, 150, 500, 600, 700]);
        }

        let page5_bytes = pager.get_page(pn(5)).unwrap().to_vec();
        match Page::parse(&page5_bytes, pager.spec(pn(5))).unwrap() {
            Page::TableInterior(ip) => {
                assert!(ip.cells.is_empty(), "page 5 should now be degenerate");
                assert_eq!(
                    ip.rightmost_child,
                    pn(10),
                    "rightmost_child must be promoted from the popped cell's left_child"
                );
            }
            Page::TableLeaf(_) => panic!("page 5 should still be an interior page"),
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                panic!("parsed page 5: expected a table page, found an index page")
            }
        }

        let stats = check_tree(&mut pager, pn(1)).unwrap();
        assert_eq!(stats.depth, 2, "tree must not have shrunk");
        std::fs::remove_file(&path).ok();
    }

    // Drains BOTH of mid1's children in sequence: the second drain hits
    // mid1 while it's *already* degenerate from the first drain, which is
    // the exact case that needs the "if ip.cells() is already empty, don't
    // call delete_child, just propagate my own death" branch -- without
    // it this either panics (popping an empty Vec) or errors on a guard.
    // The root then genuinely loses a level via `shrink_root`.
    #[test]
    fn draining_both_children_of_a_mid_node_shrinks_the_tree_by_one_level() {
        let path = temp_db_path("collapse_both_children");
        let mut pager = build_tree(&path);
        {
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            for row_id in [1u64, 2, 3, 10, 20, 30] {
                cursor.delete(rid(row_id)).unwrap();
            }

            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id.get())
                .collect();
            assert_eq!(scanned, vec![100, 150, 500, 600, 700]);
        }

        // page 1 (root) now holds what used to be mid2's (page 6) content.
        let root_bytes = pager.get_page(pn(1)).unwrap().to_vec();
        match Page::parse(&root_bytes, pager.spec(pn(1))).unwrap() {
            Page::TableInterior(ip) => {
                assert_eq!(ip.cells.len(), 1);
                assert_eq!(ip.cells[0].left_child, pn(12));
                assert_eq!(ip.cells[0].row_id, 150);
                assert_eq!(ip.rightmost_child, pn(13));
            }
            Page::TableLeaf(_) => panic!("root should still be an interior page"),
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                panic!("fetched root: expected a table page, found an index page")
            }
        }

        let stats = check_tree(&mut pager, pn(1)).unwrap();
        assert_eq!(stats.depth, 1, "tree must have shrunk by exactly one level");

        let leaked: std::collections::HashSet<PageNumber> =
            pager.list_fp().unwrap().into_iter().collect();
        let expected: std::collections::HashSet<PageNumber> =
            [pn(5), pn(6), pn(10), pn(11)].into_iter().collect();
        // Note: no live-pages + leaked-pages == file-size check here, unlike
        // `ensure_5000_row_tree_stats_are_correct` -- `build_tree` writes at
        // hand-picked page numbers (10-13) directly, never through
        // `Pager::allocate_page()`, so the file is sparse (pages 2-4 and
        // 7-9 were never allocated at all) and that byte-accounting
        // invariant doesn't apply to this fixture. The set equality below
        // is the real assertion.
        assert_eq!(leaked, expected);

        std::fs::remove_file(&path).ok();
    }

    // Hand-builds a chain of TWO stacked degenerate interior pages (3 -> 4,
    // both 0 cells) hanging off root's rightmost_child, with root's one
    // cell pointing at an unrelated small leaf. Draining that small leaf
    // makes root itself degenerate, forcing `shrink_root`'s loop to walk
    // through page 3, then page 4 (also degenerate), before finally
    // promoting leaf 7's actual rows into the root -- exercising the loop
    // as a loop, not just a single `if`.
    #[test]
    fn shrink_root_walks_through_a_stack_of_degenerate_interior_pages() {
        let path = temp_db_path("stacked_shrink");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();

        write_leaf(&mut pager, pn(2), &[rid(1), rid(2), rid(3), rid(4), rid(5)]);
        write_leaf(&mut pager, pn(7), &[rid(10), rid(20), rid(30)]);
        write_interior(&mut pager, pn(4), TableInteriorPage::new(Vec::new(), pn(7)));
        write_interior(&mut pager, pn(3), TableInteriorPage::new(Vec::new(), pn(4)));
        write_interior(
            &mut pager,
            pn(1),
            TableInteriorPage::new(
                vec![TableInteriorCell {
                    left_child: pn(2),
                    row_id: rid(5),
                }],
                pn(3),
            ),
        );

        {
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            for row_id in 1u64..=5 {
                cursor.delete(rid(row_id)).unwrap();
            }

            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id.get())
                .collect();
            assert_eq!(scanned, vec![10, 20, 30]);
        }

        // root collapsed all the way down to what used to be leaf 7.
        let root_bytes = pager.get_page(pn(1)).unwrap().to_vec();
        match Page::parse(&root_bytes, pager.spec(pn(1))).unwrap() {
            Page::TableLeaf(lp) => {
                let ids: Vec<u64> = lp.cells.iter().map(|c| c.row_id.get()).collect();
                assert_eq!(ids, vec![10, 20, 30]);
            }
            Page::TableInterior(_) => panic!("root should have collapsed all the way to a leaf"),
            Page::IndexLeaf(_) | Page::IndexInterior(_) => {
                panic!("parsed root: expected a table page, found an index page")
            }
        }

        let stats = check_tree(&mut pager, pn(1)).unwrap();
        assert_eq!(stats.depth, 0);
        assert_eq!(stats.rows, 3);

        let leaked: std::collections::HashSet<PageNumber> =
            pager.list_fp().unwrap().into_iter().collect();
        let expected: std::collections::HashSet<PageNumber> =
            [pn(2), pn(3), pn(4), pn(7)].into_iter().collect();
        assert_eq!(leaked, expected);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn delete_of_missing_row_errors_and_leaves_tree_unchanged() {
        let path = temp_db_path("delete_missing");
        let mut pager = build_tree(&path);
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        let size_before = std::fs::metadata(&path).unwrap().len();
        let scan_before: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect();

        assert!(cursor.delete(rid(999)).is_err());

        assert_eq!(std::fs::metadata(&path).unwrap().len(), size_before);
        let scan_after: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect();
        assert_eq!(scan_after, scan_before);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn insert_then_delete_shuffled_matches_reference_model() {
        let path = temp_db_path("insert_delete_shuffled");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();

        let n = 2000u64;
        let insert_order = shuffled(n);
        let mut delete_order = shuffled(n);
        delete_order.reverse(); // deterministic, but a different order than insertion

        let mut model: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
        {
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            for &k in &insert_order {
                cursor.insert(rid(k), &k.to_be_bytes()).unwrap();
                model.insert(k);
            }
            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id.get())
                .collect();
            assert_eq!(scanned, model.iter().copied().collect::<Vec<u64>>());
        }

        for &k in &delete_order {
            {
                let mut cursor = TableCursor::new(&mut pager, pn(1));
                cursor.delete(rid(k)).unwrap();
            }
            model.remove(&k);

            let scanned: Vec<u64> = {
                let mut cursor = TableCursor::new(&mut pager, pn(1));
                cursor
                    .scan()
                    .unwrap()
                    .into_iter()
                    .map(|c| c.row_id.get())
                    .collect()
            };
            assert_eq!(
                scanned,
                model.iter().copied().collect::<Vec<u64>>(),
                "mismatch after deleting {k}"
            );

            check_tree(&mut pager, pn(1)).unwrap();
        }

        assert!(model.is_empty());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn splits_reuse_freed_pages_without_growing_the_file() {
        let path = temp_db_path("split_reuse");
        let mut pager = build_tree(&path);
        // Collapse both of mid1's children: frees pages 5, 6, 10, 11
        // (same shape as `draining_both_children_of_a_mid_node_...`).
        {
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            for row_id in [1u64, 2, 3, 10, 20, 30] {
                cursor.delete(rid(row_id)).unwrap();
            }
        }
        let free_before: std::collections::HashSet<PageNumber> =
            pager.list_fp().unwrap().into_iter().collect();
        assert_eq!(free_before.len(), 4);
        let count_before = pager.header.free_page_count;
        let size_before = std::fs::metadata(&path).unwrap().len();

        // 400B payloads overflow a 512B leaf almost immediately, forcing
        // splits. Each split must pop the freelist, never extend the file.
        let mut inserted = Vec::new();
        let mut reused = false;
        for i in 0u64..12 {
            let row_id = 1000 + i;
            TableCursor::new(&mut pager, pn(1))
                .insert(rid(row_id), &vec![0xABu8; 400])
                .unwrap();
            inserted.push(row_id);
            assert_eq!(
                std::fs::metadata(&path).unwrap().len(),
                size_before,
                "split grew the file while the freelist was nonempty"
            );
            if pager.header.free_page_count < count_before {
                reused = true;
                break;
            }
        }
        assert!(reused, "expected a split to consume a freed page");
        assert_eq!(
            pager.header.free_page_count as usize,
            pager.count_fp().unwrap(),
            "header count must match the walked chain"
        );

        // Old rows plus new rows are all reachable, tree invariants hold.
        let scanned: Vec<u64> = TableCursor::new(&mut pager, pn(1))
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect();
        let mut expected = vec![100, 150, 500, 600, 700];
        expected.extend_from_slice(&inserted);
        assert_eq!(scanned, expected);
        check_tree(&mut pager, pn(1)).unwrap();

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn seek_and_scan_resolve_mixed_local_and_overflowing_payloads() {
        let path = temp_db_path("mixed_overflow");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        let mut cursor = TableCursor::new(&mut pager, pn(1));

        // local cap for a 512-byte page is 128 bytes.
        let rows: Vec<(u64, Vec<u8>)> = vec![
            (1, vec![0x11u8; 10]),             // well under the cap
            (2, vec![0x22u8; 128]),            // exactly at the cap: no overflow
            (3, vec![0x33u8; 129]),            // one byte over: needs one overflow page
            (4, vec![0x44u8; 508 + 508 + 37]), // spans several overflow pages
        ];
        for (id, payload) in &rows {
            cursor.insert(rid(*id), payload).unwrap();
        }

        for (id, payload) in &rows {
            let got = cursor
                .seek(rid(*id))
                .unwrap()
                .unwrap_or_else(|| panic!("row {id} missing"));
            assert_eq!(&got.local_payload, payload, "seek mismatch for row {id}");
        }

        let scanned: Vec<(u64, Vec<u8>)> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| (c.row_id.get(), c.local_payload))
            .collect();
        assert_eq!(scanned, rows);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn delete_frees_the_overflowing_row_s_overflow_chain() {
        let path = temp_db_path("delete_frees_overflow");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();

        // 300-byte payload: local cap is 128, so this spills into exactly one overflow page
        // (the tail is 172 bytes, under the 508-byte-per-page overflow capacity).
        {
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            cursor.insert(rid(1), &vec![0xABu8; 300]).unwrap();
        }
        assert_eq!(pager.count_fp().unwrap(), 0);
        let size_with_overflow = std::fs::metadata(&path).unwrap().len();

        {
            let mut cursor = TableCursor::new(&mut pager, pn(1));
            cursor.delete(rid(1)).unwrap();
        }

        // the overflow page is back on the freelist, not leaked.
        assert_eq!(pager.count_fp().unwrap(), 1);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            size_with_overflow,
            "freeing must not shrink or grow the file, just mark the page free"
        );

        // and it's genuinely reusable afterwards.
        let size_before_reuse = std::fs::metadata(&path).unwrap().len();
        pager.allocate_page().unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size_before_reuse);
        assert_eq!(pager.count_fp().unwrap(), 0);

        std::fs::remove_file(&path).ok();
    }
}

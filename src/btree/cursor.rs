use crate::file::{
    pager::Pager,
    pages::{
        Page,
        interior::{TableInteriorCell, TableInteriorPage},
        leaf::TableLeafCell,
        spec::PageSpec,
        table::TablePage,
        types::PageNumber,
    },
};

pub struct Cursor<'a> {
    pager: &'a mut Pager,
    root_pn: PageNumber,
}

impl<'a> Cursor<'a> {
    pub fn new(pager: &'a mut Pager, root_pn: PageNumber) -> Self {
        Self { pager, root_pn }
    }

    pub fn seek(&mut self, row_id: u64) -> anyhow::Result<Option<TableLeafCell>> {
        self.seek_from(self.root_pn, row_id)
    }

    pub fn scan(&mut self) -> anyhow::Result<Vec<TableLeafCell>> {
        let mut res = Vec::new();
        self.scan_from(self.root_pn, &mut res)?;
        Ok(res)
    }

    pub fn insert(&mut self, row_id: u64, payload: &[u8]) -> anyhow::Result<()> {
        let res = self.insert_into(self.root_pn, row_id, payload)?;
        match res {
            InsertOutcome::Done => Ok(()),
            InsertOutcome::Split {
                new_child, divider, ..
            } => self.split_root(new_child, divider),
        }
    }

    pub fn delete(&mut self, row_id: u64) -> anyhow::Result<()> {
        match self.delete_from(self.root_pn, row_id)? {
            DeleteOutcome::Done => self.shrink_root(),
            DeleteOutcome::DeletedChild => {
                anyhow::bail!(
                    "root reported its own death: shrink_root should have kept it non-empty"
                )
            }
        }
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
        row_id: u64,
    ) -> anyhow::Result<Option<TableLeafCell>> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(lp) => {
                let res = lp.cells.binary_search_by(|cell| cell.row_id.cmp(&row_id));
                Ok(res.ok().map(|idx| lp.cells[idx].clone()))
            }
            Page::TableInterior(ip) => {
                let next = ip.child_of(row_id);
                self.seek_from(next, row_id)
            }
        }
    }

    fn scan_from(
        &mut self,
        page_number: PageNumber,
        scanned: &mut Vec<TableLeafCell>,
    ) -> anyhow::Result<()> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(lp) => scanned.extend(lp.cells),
            Page::TableInterior(ip) => {
                for cell in ip.cells.iter() {
                    self.scan_from(cell.left_child, scanned)?;
                }
                self.scan_from(ip.rightmost_child, scanned)?;
            }
        }
        Ok(())
    }

    fn insert_into(
        &mut self,
        page_number: PageNumber,
        row_id: u64,
        payload: &[u8],
    ) -> anyhow::Result<InsertOutcome> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(lp) => {
                if lp.contains(row_id) {
                    anyhow::bail!("row_id already exists")
                }

                let new_cell = TableLeafCell::new(row_id, payload);
                self.insert_or_split(new_cell, lp, page_number)
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
        }
    }

    fn insert_or_split<P: TablePage>(
        &mut self,
        new_cell: P::C,
        mut page: P,
        page_number: PageNumber,
    ) -> anyhow::Result<InsertOutcome> {
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

    fn split_root(&mut self, right_child: PageNumber, divider: u64) -> anyhow::Result<()> {
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
        row_id: u64,
    ) -> anyhow::Result<DeleteOutcome> {
        match self.fetch_page(page_number)? {
            Page::TableLeaf(mut lp) => {
                lp.delete(row_id)?;
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
            }
        }
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

enum InsertOutcome {
    Done,
    Split {
        old_child: PageNumber,
        new_child: PageNumber,
        divider: u64,
    },
}

enum DeleteOutcome {
    Done,
    DeletedChild,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::btree::check::check_tree;
    use crate::file::pages::interior::{TableInteriorCell, TableInteriorPage};
    use crate::file::pages::leaf::TableLeafPage;
    use crate::file::pages::spec::PageSpec;

    const PAGE_SIZE: usize = 512;

    fn pn(n: u32) -> PageNumber {
        PageNumber::try_from(n).unwrap()
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

    fn leaf(row_ids: &[u64]) -> TableLeafPage {
        TableLeafPage {
            cells: row_ids
                .iter()
                .map(|&row_id| TableLeafCell {
                    row_id,
                    payload: row_id.to_be_bytes().to_vec(),
                })
                .collect(),
        }
    }

    fn write_leaf(pager: &mut Pager, page_number: PageNumber, row_ids: &[u64]) {
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

        write_leaf(&mut pager, pn(10), &[1, 2, 3]);
        write_leaf(&mut pager, pn(11), &[10, 20, 30]);
        write_leaf(&mut pager, pn(12), &[100, 150]);
        write_leaf(&mut pager, pn(13), &[500, 600, 700]);

        write_interior(
            &mut pager,
            pn(5),
            TableInteriorPage {
                cells: vec![TableInteriorCell {
                    left_child: pn(10),
                    row_id: 3,
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
                    row_id: 150,
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
                    row_id: 30,
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
        let mut cursor = Cursor::new(&mut pager, pn(1));

        for &row_id in &[1u64, 2, 3, 10, 20, 30, 100, 150, 500, 600, 700] {
            let found = cursor.seek(row_id).unwrap();
            assert_eq!(
                found.map(|c| c.row_id),
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
        let mut cursor = Cursor::new(&mut pager, pn(1));

        // includes: below everything, between leaves within one subtree,
        // between the two second-level subtrees, and above everything.
        for &row_id in &[0u64, 4, 15, 50, 99, 151, 999] {
            assert_eq!(
                cursor.seek(row_id).unwrap(),
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
        let mut cursor = Cursor::new(&mut pager, pn(1));

        for i in 1u64..=100 {
            cursor.insert(i, &i.to_be_bytes()).unwrap();
        }

        for i in 1u64..=100 {
            assert_eq!(
                cursor.seek(i).unwrap().map(|c| c.row_id),
                Some(i),
                "row {i} should be findable via seek after inserting 100 ascending rows"
            );
        }

        let scanned: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id)
            .collect();
        assert_eq!(scanned, (1u64..=100).collect::<Vec<u64>>());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn scan_returns_every_row_in_ascending_order() {
        let path = temp_db_path("scan_order");
        let mut pager = build_tree(&path);
        let mut cursor = Cursor::new(&mut pager, pn(1));

        let got: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id)
            .collect();

        assert_eq!(got, vec![1, 2, 3, 10, 20, 30, 100, 150, 500, 600, 700]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn insert_survives_reopen() {
        let path = temp_db_path("reopen");
        {
            let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
            let mut cursor = Cursor::new(&mut pager, pn(1));
            for i in 1u64..=200 {
                cursor.insert(i, &i.to_be_bytes()).unwrap();
            }
        }
        let mut pager = Pager::init(&path).unwrap();
        let rows = Cursor::new(&mut pager, pn(1)).scan().unwrap();
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
        let mut cursor = Cursor::new(&mut pager, pn(1));

        let data = shuffled(5000);

        for elem in &data {
            cursor.insert(*elem, &elem.to_be_bytes()).unwrap();
        }
        for elem in &data {
            let got = cursor.seek(*elem).unwrap().unwrap();
            assert_eq!(got.row_id, *elem);
            assert_eq!(got.payload, elem.to_be_bytes()); // you currently only check row_id
        }

        let scanned: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id)
            .collect();
        assert_eq!(scanned.len(), 5000);
        assert_eq!(scanned, (1u64..=5000).collect::<Vec<u64>>());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn failed_inserts_do_not_grow_the_file() {
        let path = temp_db_path("no_leak");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        let mut cursor = Cursor::new(&mut pager, pn(1));

        cursor.insert(1, b"x").unwrap();
        let size = std::fs::metadata(&path).unwrap().len();

        assert!(cursor.insert(1, b"dup").is_err()); // duplicate key
        assert!(cursor.insert(2, &vec![0u8; 600]).is_err()); // larger than a 512-byte page

        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        assert_eq!(cursor.scan().unwrap().len(), 1);
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
            let mut cursor = Cursor::new(&mut pager, pn(1));
            for (row_id, payload) in &expected {
                cursor.insert(*row_id, payload).unwrap();
            }
        }
        // precondition: the 30 small rows still fit in the root leaf, so no split has happened
        assert_eq!(std::fs::metadata(&path).unwrap().len(), PAGE_SIZE as u64);

        let big = vec![0xABu8; 300];
        let mut cursor = Cursor::new(&mut pager, pn(1));
        cursor.insert(big_id, &big).unwrap();

        expected.push((big_id, big));
        expected.sort_by_key(|(id, _)| *id);

        // every row is reachable, with its exact payload
        for (id, payload) in &expected {
            let cell = cursor
                .seek(*id)
                .unwrap()
                .unwrap_or_else(|| panic!("row {id} is missing after the split"));
            assert_eq!(&cell.payload, payload, "payload mismatch for row {id}");
        }
        // and a scan returns all of them, in order
        let scanned: Vec<(u64, Vec<u8>)> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| (c.row_id, c.payload))
            .collect();
        assert_eq!(scanned, expected);

        // the split really happened: page 1 is now an interior root, and the file grew by exactly
        // two pages (the new right half, plus the copy of the old root made by the root split)
        let root_bytes = pager.get_page(pn(1)).unwrap().to_vec();
        assert!(matches!(
            Page::parse(&root_bytes, PageSpec::new(PAGE_SIZE as usize, pn(1))).unwrap(),
            Page::TableInterior(_)
        ));
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            3 * PAGE_SIZE as u64
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
    fn big_cell_in_the_middle_of_a_full_leaf_is_rejected_cleanly() {
        // Needs a three-way split, which isn't supported: expect an error, no leak, no change.
        let path = temp_db_path("big_cell_middle");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        let mut cursor = Cursor::new(&mut pager, pn(1));
        for i in 1u64..=30 {
            cursor.insert(i * 2, &[i as u8; 10]).unwrap();
        }

        assert!(cursor.insert(31, &[0xABu8; 300]).is_err());

        assert_eq!(std::fs::metadata(&path).unwrap().len(), PAGE_SIZE as u64);
        assert_eq!(cursor.scan().unwrap().len(), 30);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tree_rooted_at_a_non_first_page_splits_and_stays_reachable() {
        let path = temp_db_path("non_first_root");
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        write_leaf(&mut pager, pn(2), &[]); // an empty leaf root that is not page 1
        {
            let mut cursor = Cursor::new(&mut pager, pn(2));
            for i in 1u64..=300 {
                cursor.insert(i, &i.to_be_bytes()).unwrap();
            }
            for i in 1u64..=300 {
                assert_eq!(cursor.seek(i).unwrap().map(|c| c.row_id), Some(i));
            }
            let ids: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id)
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
        let mut cursor = Cursor::new(&mut pager, pn(1));

        let data = shuffled(5000);

        for elem in &data {
            cursor.insert(*elem, &elem.to_be_bytes()).unwrap();
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
        let mut cursor = Cursor::new(&mut pager, pn(1));

        cursor.delete(100).unwrap();

        assert_eq!(cursor.seek(100).unwrap(), None);
        for &row_id in &[1u64, 2, 3, 10, 20, 30, 150, 500, 600, 700] {
            assert_eq!(
                cursor.seek(row_id).unwrap().map(|c| c.row_id),
                Some(row_id),
                "expected row {row_id} to still be reachable"
            );
        }
        let scanned: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id)
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
            let mut cursor = Cursor::new(&mut pager, pn(1));
            for row_id in [1u64, 2, 3] {
                cursor.delete(row_id).unwrap();
            }

            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id)
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
            let mut cursor = Cursor::new(&mut pager, pn(1));
            for row_id in [10u64, 20, 30] {
                cursor.delete(row_id).unwrap();
            }

            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id)
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
            let mut cursor = Cursor::new(&mut pager, pn(1));
            for row_id in [1u64, 2, 3, 10, 20, 30] {
                cursor.delete(row_id).unwrap();
            }

            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id)
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

        write_leaf(&mut pager, pn(2), &[1, 2, 3, 4, 5]);
        write_leaf(&mut pager, pn(7), &[10, 20, 30]);
        write_interior(&mut pager, pn(4), TableInteriorPage::new(Vec::new(), pn(7)));
        write_interior(&mut pager, pn(3), TableInteriorPage::new(Vec::new(), pn(4)));
        write_interior(
            &mut pager,
            pn(1),
            TableInteriorPage::new(
                vec![TableInteriorCell {
                    left_child: pn(2),
                    row_id: 5,
                }],
                pn(3),
            ),
        );

        {
            let mut cursor = Cursor::new(&mut pager, pn(1));
            for row_id in 1u64..=5 {
                cursor.delete(row_id).unwrap();
            }

            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id)
                .collect();
            assert_eq!(scanned, vec![10, 20, 30]);
        }

        // root collapsed all the way down to what used to be leaf 7.
        let root_bytes = pager.get_page(pn(1)).unwrap().to_vec();
        match Page::parse(&root_bytes, pager.spec(pn(1))).unwrap() {
            Page::TableLeaf(lp) => {
                let ids: Vec<u64> = lp.cells.iter().map(|c| c.row_id).collect();
                assert_eq!(ids, vec![10, 20, 30]);
            }
            Page::TableInterior(_) => panic!("root should have collapsed all the way to a leaf"),
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
        let mut cursor = Cursor::new(&mut pager, pn(1));

        let size_before = std::fs::metadata(&path).unwrap().len();
        let scan_before: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id)
            .collect();

        assert!(cursor.delete(999).is_err());

        assert_eq!(std::fs::metadata(&path).unwrap().len(), size_before);
        let scan_after: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id)
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
            let mut cursor = Cursor::new(&mut pager, pn(1));
            for &k in &insert_order {
                cursor.insert(k, &k.to_be_bytes()).unwrap();
                model.insert(k);
            }
            let scanned: Vec<u64> = cursor
                .scan()
                .unwrap()
                .into_iter()
                .map(|c| c.row_id)
                .collect();
            assert_eq!(scanned, model.iter().copied().collect::<Vec<u64>>());
        }

        for &k in &delete_order {
            {
                let mut cursor = Cursor::new(&mut pager, pn(1));
                cursor.delete(k).unwrap();
            }
            model.remove(&k);

            let scanned: Vec<u64> = {
                let mut cursor = Cursor::new(&mut pager, pn(1));
                cursor
                    .scan()
                    .unwrap()
                    .into_iter()
                    .map(|c| c.row_id)
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
            let mut cursor = Cursor::new(&mut pager, pn(1));
            for row_id in [1u64, 2, 3, 10, 20, 30] {
                cursor.delete(row_id).unwrap();
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
            Cursor::new(&mut pager, pn(1))
                .insert(row_id, &vec![0xABu8; 400])
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
        let scanned: Vec<u64> = Cursor::new(&mut pager, pn(1))
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id)
            .collect();
        let mut expected = vec![100, 150, 500, 600, 700];
        expected.extend_from_slice(&inserted);
        assert_eq!(scanned, expected);
        check_tree(&mut pager, pn(1)).unwrap();

        std::fs::remove_file(&path).ok();
    }
}

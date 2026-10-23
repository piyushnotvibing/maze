//! A cursor over an index b-tree, mirroring `TableCursor` with `RowID`
//! generalized to the composite key `(Vec<Value>, RowID)`.
//!
//! `seek`/`scan`/`insert`/`delete` are plain recursive descents, exactly
//! like `TableCursor` -- a single-key operation never needs to leave the
//! path from root to one leaf, so recursion is the simplest correct tool.
//!
//! `range_scan` is different: it has to return every entry whose indexed
//! `values` fall in `[lower, upper]` (compared as prefixes), and that run of
//! matching entries can legitimately span more than one leaf (duplicate
//! values, or a range wider than one leaf's capacity). There is no sibling
//! pointer to hop to "the next leaf," so the cursor instead keeps an
//! explicit path -- a stack of `(PageNumber, child_index)` frames from the
//! root down to the current leaf cell. Moving to the next entry means
//! bumping the leaf frame's index; once that runs past the leaf's last
//! cell, it pops back up to the parent frame, bumps *its* index to the next
//! child, and walks back down that child's leftmost spine to a fresh leaf.
//! `scan` is implemented as `range_scan(&[], &[])`, since an empty prefix
//! matches every key and never terminates the scan early -- this also
//! exercises the stack machinery for the common "scan everything" case.

use std::cmp::Ordering;

use crate::{btree::{DeleteOutcome, InsertOutcome}, storage::{
    pager::Pager, pages::{
        Page, btree_page::BTreePage, index::{
            interior::{IndexInteriorCell, IndexInteriorPage}, leaf::{IndexLeafCell, IndexLeafPage},
        }, spec::PageSpec, types::{PageNumber, RowID},
    }, record::values::Value,
}};

type Frame = (PageNumber, usize);

pub struct IndexCursor<'a> {
    pub pager: &'a mut Pager,
    root_pn: PageNumber,
    unique: bool,
}

impl<'a> IndexCursor<'a> {
    pub fn new(pager: &'a mut Pager, root_pn: PageNumber, unique: bool) -> Self {
        Self { pager, root_pn, unique }
    }

    /// Exact match on the full composite key `(values, row_id)`.
    pub fn seek(&mut self, values: &[Value], row_id: RowID) -> anyhow::Result<Option<IndexLeafCell>> {
        self.seek_from(self.root_pn, values, row_id)
    }

    /// Every entry in ascending key order.
    pub fn scan(&mut self) -> anyhow::Result<Vec<IndexLeafCell>> {
        self.range_scan(&[], &[])
    }

    /// Every entry whose `values` prefix is within `[lower, upper]` (both
    /// inclusive), compared only over the number of columns each bound
    /// supplies. Passing the same slice for both finds every entry sharing
    /// that exact prefix -- the "prefix scan" case -- even when the matching
    /// run of entries spans a leaf boundary.
    pub fn range_scan(
        &mut self,
        lower: &[Value],
        upper: &[Value],
    ) -> anyhow::Result<Vec<IndexLeafCell>> {
        let mut stack = self.descend_to_prefix(lower)?;
        self.climb(&mut stack)?;

        let mut out = Vec::new();
        while let Some(cell) = self.current(&stack)? {
            if cmp_prefix(&cell.values, upper) == Ordering::Greater {
                break;
            }
            out.push(cell);
            self.advance(&mut stack)?;
        }
        Ok(out)
    }

    pub fn insert(&mut self, values: Vec<Value>, row_id: RowID) -> anyhow::Result<()> {
        if self.unique && !self.range_scan(&values, &values)?.is_empty() {
            anyhow::bail!("values being inserted are not UNIQUE, although they were declared as such")
        }

        match self.insert_into(self.root_pn, values, row_id)? {
            InsertOutcome::Done => Ok(()),
            InsertOutcome::Split {
                new_child, divider, ..
            } => self.split_root(new_child, divider),
        }
    }

    pub fn delete(&mut self, values: &[Value], row_id: RowID) -> anyhow::Result<()> {
        match self.delete_from(self.root_pn, values, row_id)? {
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
            Page::IndexLeaf(IndexLeafPage::new(vec![])).serialize(self.pager.spec(self.root_pn))?,
        )?;

        Ok(())
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
        values: &[Value],
        row_id: RowID,
    ) -> anyhow::Result<Option<IndexLeafCell>> {
        match self.fetch_page(page_number)? {
            Page::IndexLeaf(lp) => {
                match lp
                    .cells
                    .binary_search_by(|c| c.values.as_slice().cmp(values).then(c.row_id.cmp(&row_id)))
                {
                    Ok(idx) => Ok(Some(lp.cells[idx].clone())),
                    Err(_) => Ok(None),
                }
            }
            Page::IndexInterior(ip) => {
                let next = ip.child_of(values, row_id);
                self.seek_from(next, values, row_id)
            }
            Page::TableLeaf(_) | Page::TableInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected an index page, found a table page"
                )
            }
        }
    }

    fn insert_into(
        &mut self,
        page_number: PageNumber,
        values: Vec<Value>,
        row_id: RowID,
    ) -> anyhow::Result<InsertOutcome<(Vec<Value>, RowID)>> {
        match self.fetch_page(page_number)? {
            Page::IndexLeaf(lp) => {
                if lp.contains(&values, row_id) {
                    anyhow::bail!("(values, row_id) already exists");
                }
                let new_cell = IndexLeafCell { values, row_id };
                self.insert_or_split(new_cell, lp, page_number)
            }
            Page::IndexInterior(mut ip) => {
                let next = ip.child_of(&values, row_id);
                match self.insert_into(next, values, row_id)? {
                    InsertOutcome::Done => Ok(InsertOutcome::Done),
                    InsertOutcome::Split {
                        old_child,
                        new_child,
                        divider: (d_values, d_row_id),
                    } => {
                        repoint_child(&mut ip, old_child, new_child)?;
                        let new_cell = IndexInteriorCell {
                            left_child: old_child,
                            values: d_values,
                            row_id: d_row_id,
                        };
                        self.insert_or_split(new_cell, ip, page_number)
                    }
                }
            }
            Page::TableLeaf(_) | Page::TableInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected an index page, found a table page"
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

        if page.fits(&new_cell, spec)? {
            page.insert(new_cell, spec)?;
            self.pager.write_page(page_number, page.serialize(spec)?)?;
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

    fn split_root(
        &mut self,
        right_child: PageNumber,
        divider: (Vec<Value>, RowID),
    ) -> anyhow::Result<()> {
        let root_pn = self.root_pn;
        let root_bytes = self.pager.get_page(root_pn)?.to_vec();
        // parse before allocating
        let left = Page::parse(&root_bytes, self.pager.spec(root_pn))?;

        let left_pn = self.pager.allocate_page()?;
        let left_bytes = left.serialize(self.pager.spec(left_pn))?;
        self.pager.write_page(left_pn, left_bytes)?;

        let (values, row_id) = divider;
        let new_root = IndexInteriorPage {
            cells: vec![IndexInteriorCell {
                left_child: left_pn,
                values,
                row_id,
            }],
            rightmost_child: right_child,
        };
        let new_root_bytes = new_root.serialize(self.pager.spec(root_pn))?;
        // write_page stamps the header when root_pn == 1
        self.pager.write_page(root_pn, new_root_bytes)
    }

    fn delete_from(
        &mut self,
        page_number: PageNumber,
        values: &[Value],
        row_id: RowID,
    ) -> anyhow::Result<DeleteOutcome> {
        match self.fetch_page(page_number)? {
            Page::IndexLeaf(mut lp) => {
                lp.delete(values, row_id)?;
                if lp.cells().is_empty() && page_number != self.root_pn {
                    return Ok(DeleteOutcome::DeletedChild);
                }
                self.pager
                    .write_page(page_number, lp.serialize(self.pager.spec(page_number))?)?;
                Ok(DeleteOutcome::Done)
            }
            Page::IndexInterior(mut ip) => {
                let child = ip.child_of(values, row_id);
                match self.delete_from(child, values, row_id)? {
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
            Page::TableLeaf(_) | Page::TableInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected an index page, found a table page"
                )
            }
        }
    }

    fn delete_all_from(&mut self, page_number: PageNumber) -> anyhow::Result<()> {
        match self.fetch_page(page_number)? {
            Page::IndexLeaf(_) => {
                if page_number == self.root_pn {
                    return Ok(());
                }
                self.pager.free_page(page_number)?;
                Ok(())
            }
            Page::IndexInterior(ip) => {
                for cell in ip.cells() {
                    self.delete_all_from(cell.left_child)?;
                }
                self.delete_all_from(ip.rightmost_child)?;
                if page_number == self.root_pn {
                    return Ok(());
                }
                self.pager.free_page(page_number)
            }
            Page::TableLeaf(_) | Page::TableInterior(_) => {
                anyhow::bail!(
                    "fetched page {page_number}: expected an index page, found a table page"
                )
            }
        }
    }

    fn shrink_root(&mut self) -> anyhow::Result<()> {
        loop {
            let root = self.fetch_page(self.root_pn)?;
            match root {
                Page::IndexLeaf(_) => return Ok(()),
                Page::IndexInterior(ip) if !ip.cells().is_empty() => return Ok(()),
                Page::IndexInterior(root_ip) => {
                    let right_child = root_ip.rightmost_child;
                    let root_right_child_bytes = self
                        .fetch_page(right_child)?
                        .serialize(self.pager.spec(self.root_pn))?;
                    self.pager
                        .write_page(self.root_pn, root_right_child_bytes)?;
                    self.pager.free_page(right_child)?;
                }
                Page::TableLeaf(_) | Page::TableInterior(_) => {
                    anyhow::bail!(
                        "fetched page {}: expected an index page, found a table page",
                        self.root_pn
                    )
                }
            }
        }
    }

    /*====================== stack-based traversal ======================*/

    /// Builds the path from the root down to the leaf cell that would be the
    /// first entry whose `values` prefix is `>= prefix` -- or, if every
    /// entry in the reached leaf is `< prefix`, down to that leaf anyway
    /// (with its index pointing past its last cell). `climb` resolves that
    /// into a genuinely valid position (or "no more entries").
    fn descend_to_prefix(&mut self, prefix: &[Value]) -> anyhow::Result<Vec<Frame>> {
        let mut stack = Vec::new();
        let mut pn = self.root_pn;
        loop {
            match self.fetch_page(pn)? {
                Page::IndexInterior(ip) => {
                    let idx = ip
                        .cells
                        .partition_point(|c| cmp_prefix(&c.values, prefix) == Ordering::Less);
                    let child = match ip.cells.get(idx) {
                        Some(cell) => cell.left_child,
                        None => ip.rightmost_child,
                    };
                    stack.push((pn, idx));
                    pn = child;
                }
                Page::IndexLeaf(lp) => {
                    let idx = lp
                        .cells
                        .partition_point(|c| cmp_prefix(&c.values, prefix) == Ordering::Less);
                    stack.push((pn, idx));
                    return Ok(stack);
                }
                Page::TableLeaf(_) | Page::TableInterior(_) => {
                    anyhow::bail!("fetched page {pn}: expected an index page, found a table page")
                }
            }
        }
    }

    /// Pushes frames down the leftmost spine of the subtree rooted at `pn`,
    /// ending on its first leaf cell.
    fn push_leftmost(&mut self, mut pn: PageNumber, stack: &mut Vec<Frame>) -> anyhow::Result<()> {
        loop {
            match self.fetch_page(pn)? {
                Page::IndexInterior(ip) => {
                    stack.push((pn, 0));
                    pn = match ip.cells().first() {
                        Some(cell) => cell.left_child,
                        None => ip.rightmost_child,
                    };
                }
                Page::IndexLeaf(_) => {
                    stack.push((pn, 0));
                    return Ok(());
                }
                Page::TableLeaf(_) | Page::TableInterior(_) => {
                    anyhow::bail!("fetched page {pn}: expected an index page, found a table page")
                }
            }
        }
    }

    /// Ensures the stack's top frame points at a real leaf cell, climbing
    /// (popping a frame and bumping its parent's index) and re-descending as
    /// many times as needed. Leaves `stack` empty once the tree is
    /// exhausted. Idempotent: calling it when the top frame already points
    /// at a valid cell is a no-op.
    fn climb(&mut self, stack: &mut Vec<Frame>) -> anyhow::Result<()> {
        loop {
            let Some(&(pn, idx)) = stack.last() else {
                return Ok(());
            };
            match self.fetch_page(pn)? {
                Page::IndexLeaf(lp) => {
                    if idx < lp.cells.len() {
                        return Ok(());
                    }
                    stack.pop();
                    if let Some(parent) = stack.last_mut() {
                        parent.1 += 1;
                    }
                }
                Page::IndexInterior(ip) => {
                    let n = ip.cells.len();
                    if idx > n {
                        stack.pop();
                        if let Some(parent) = stack.last_mut() {
                            parent.1 += 1;
                        }
                        continue;
                    }
                    let child = if idx < n {
                        ip.cells[idx].left_child
                    } else {
                        ip.rightmost_child
                    };
                    // Keep this interior frame in place (its idx already
                    // points at `child`) and descend below it -- popping it
                    // here would discard the path back up through this
                    // level, leaving `climb` with no parent frame to bump
                    // once the fresh subtree's leaves run out.
                    self.push_leftmost(child, stack)?;
                    return Ok(());
                }
                Page::TableLeaf(_) | Page::TableInterior(_) => {
                    anyhow::bail!("fetched page {pn}: expected an index page, found a table page")
                }
            }
        }
    }

    /// Moves the cursor one entry forward. `stack` must point at a valid
    /// leaf cell on entry (as `climb` leaves it); on return it either points
    /// at the next valid cell, or is empty (no more entries).
    fn advance(&mut self, stack: &mut Vec<Frame>) -> anyhow::Result<()> {
        if let Some(top) = stack.last_mut() {
            top.1 += 1;
        }
        self.climb(stack)
    }

    /// The cell `stack`'s top frame currently points at, assuming `climb`
    /// has already been applied (so the frame is either absent or valid).
    fn current(&mut self, stack: &[Frame]) -> anyhow::Result<Option<IndexLeafCell>> {
        let Some(&(pn, idx)) = stack.last() else {
            return Ok(None);
        };
        match self.fetch_page(pn)? {
            Page::IndexLeaf(lp) => Ok(lp.cells.get(idx).cloned()),
            _ => anyhow::bail!("expected the top of the path to be a leaf page"),
        }
    }
}

/// Compares `values`' first `prefix.len()` columns against `prefix`.
/// Lexicographic order on the full column vector is consistent with order
/// on any of its prefixes, so routing decisions made this way at interior
/// pages are sound: a subtree whose divider's prefix is `< prefix` can only
/// contain entries whose prefix is also `< prefix`.
fn cmp_prefix(values: &[Value], prefix: &[Value]) -> Ordering {
    values[..prefix.len()].cmp(prefix)
}

fn repoint_child(
    ip: &mut IndexInteriorPage,
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
    use crate::btree::check::check_index_tree;
    use crate::storage::pages::index::leaf::IndexLeafPage;

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
        path.push(format!("maze_index_cursor_test_{tag}_{unique}.mz"));
        path.to_str().unwrap().to_string()
    }

    /// Single-column `Value::Int` keys, row_id chosen equal to the int /10 so
    /// assertions can read as plain integers.
    fn entry(key: i64, row_id: u64) -> (Vec<Value>, RowID) {
        (vec![Value::Int(key)], rid(row_id))
    }

    fn leaf(entries: &[(Vec<Value>, RowID)]) -> IndexLeafPage {
        IndexLeafPage {
            cells: entries
                .iter()
                .map(|(values, row_id)| IndexLeafCell {
                    values: values.clone(),
                    row_id: *row_id,
                })
                .collect(),
        }
    }

    fn write_leaf(pager: &mut Pager, page_number: PageNumber, entries: &[(Vec<Value>, RowID)]) {
        let bytes = Page::IndexLeaf(leaf(entries))
            .serialize(PageSpec::fresh(PAGE_SIZE))
            .unwrap();
        pager.write_page(page_number, bytes).unwrap();
    }

    fn write_interior(pager: &mut Pager, page_number: PageNumber, page: IndexInteriorPage) {
        let bytes = Page::IndexInterior(page)
            .serialize(PageSpec::new(PAGE_SIZE, page_number))
            .unwrap();
        pager.write_page(page_number, bytes).unwrap();
    }

    /// `Pager::new` always creates page 1 as an empty *table* leaf (page 1 is
    /// reserved for the schema table; the file-header/page-1 rules in
    /// `Page::serialize`/`parse` outright reject an index page there). So
    /// every index tree in these tests is rooted at page 2 instead, mirroring
    /// `table_cursor`'s `tree_rooted_at_a_non_first_page_splits_and_stays_reachable`.
    fn new_index_tree(path: &str) -> Pager {
        let mut pager = Pager::new(path, PAGE_SIZE).unwrap();
        write_leaf(&mut pager, pn(2), &[]);
        pager
    }

    /// A 2-level tree rooted at page 2:
    ///
    ///                  root (page 1, interior)
    ///            cell: (page 10, key=(30,3))   rightmost: page 11
    ///                /                                      \
    ///           leaf 10                                  leaf 11
    ///         [(10,1),(20,2),(30,3)]                  [(40,4),(50,5),(60,6)]
    fn build_tree(path: &str) -> Pager {
        let mut pager = Pager::new(path, PAGE_SIZE).unwrap();

        write_leaf(
            &mut pager,
            pn(10),
            &[entry(10, 1), entry(20, 2), entry(30, 3)],
        );
        write_leaf(
            &mut pager,
            pn(11),
            &[entry(40, 4), entry(50, 5), entry(60, 6)],
        );

        write_interior(
            &mut pager,
            pn(2),
            IndexInteriorPage {
                cells: vec![IndexInteriorCell {
                    left_child: pn(10),
                    values: vec![Value::Int(30)],
                    row_id: rid(3),
                }],
                rightmost_child: pn(11),
            },
        );

        pager
    }

    #[test]
    fn seek_finds_every_entry_that_exists() {
        let path = temp_db_path("seek_finds");
        let mut pager = build_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        for &(key, row) in &[(10, 1u64), (20, 2), (30, 3), (40, 4), (50, 5), (60, 6)] {
            let found = cursor.seek(&[Value::Int(key)], rid(row)).unwrap();
            assert_eq!(
                found.map(|c| (c.values, c.row_id.get())),
                Some((vec![Value::Int(key)], row)),
                "expected to find key {key} / row {row}"
            );
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn seek_returns_none_for_entries_that_dont_exist() {
        let path = temp_db_path("seek_none");
        let mut pager = build_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        // right values, wrong row_id, and values that don't exist at all.
        assert_eq!(cursor.seek(&[Value::Int(10)], rid(999)).unwrap(), None);
        assert_eq!(cursor.seek(&[Value::Int(15)], rid(1)).unwrap(), None);
        assert_eq!(cursor.seek(&[Value::Int(999)], rid(1)).unwrap(), None);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn insert_100_ascending_rows_all_remain_reachable() {
        let path = temp_db_path("insert_100_ascending");
        let mut pager = new_index_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        for i in 1i64..=100 {
            cursor.insert(vec![Value::Int(i)], rid(i as u64)).unwrap();
        }

        for i in 1i64..=100 {
            assert_eq!(
                cursor
                    .seek(&[Value::Int(i)], rid(i as u64))
                    .unwrap()
                    .map(|c| c.row_id.get()),
                Some(i as u64),
                "row with key {i} should be findable via seek"
            );
        }

        let scanned: Vec<i64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| match c.values.as_slice() {
                [Value::Int(k)] => *k,
                other => panic!("unexpected key shape: {other:?}"),
            })
            .collect();
        assert_eq!(scanned, (1i64..=100).collect::<Vec<i64>>());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn scan_returns_every_entry_in_ascending_key_order() {
        let path = temp_db_path("scan_order");
        let mut pager = build_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        let got: Vec<i64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| match c.values.as_slice() {
                [Value::Int(k)] => *k,
                other => panic!("unexpected key shape: {other:?}"),
            })
            .collect();

        assert_eq!(got, vec![10, 20, 30, 40, 50, 60]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn insert_survives_reopen() {
        let path = temp_db_path("reopen");
        {
            let mut pager = new_index_tree(&path);
            let mut cursor = IndexCursor::new(&mut pager, pn(2), false);
            for i in 1i64..=200 {
                cursor.insert(vec![Value::Int(i)], rid(i as u64)).unwrap();
            }
        }
        let mut pager = Pager::init(&path).unwrap();
        let rows = IndexCursor::new(&mut pager, pn(2), false).scan().unwrap();
        assert_eq!(rows.len(), 200);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn delete_removes_a_single_entry_and_leaves_the_rest_reachable() {
        let path = temp_db_path("delete_single");
        let mut pager = build_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        cursor.delete(&[Value::Int(20)], rid(2)).unwrap();

        assert_eq!(cursor.seek(&[Value::Int(20)], rid(2)).unwrap(), None);
        for &(key, row) in &[(10, 1u64), (30, 3), (40, 4), (50, 5), (60, 6)] {
            assert!(
                cursor.seek(&[Value::Int(key)], rid(row)).unwrap().is_some(),
                "key {key} / row {row} should still be reachable"
            );
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn delete_of_missing_entry_errors_and_leaves_tree_unchanged() {
        let path = temp_db_path("delete_missing");
        let mut pager = build_tree(&path);

        {
            let mut cursor = IndexCursor::new(&mut pager, pn(2), false);
            assert!(cursor.delete(&[Value::Int(999)], rid(1)).is_err());
        }

        let stats = check_index_tree(&mut pager, pn(2)).unwrap();
        assert_eq!(stats.rows, 6);

        std::fs::remove_file(&path).ok();
    }

    /// Two rows can legitimately share the same indexed value — the composite
    /// key `(values, row_id)` is what disambiguates them, with row_id breaking
    /// the tie. Both must coexist and be independently seekable/deletable.
    #[test]
    fn duplicate_values_are_disambiguated_by_row_id() {
        let path = temp_db_path("duplicate_values");
        let mut pager = new_index_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        cursor.insert(vec![Value::Int(7)], rid(1)).unwrap();
        cursor.insert(vec![Value::Int(7)], rid(2)).unwrap();

        assert!(cursor.seek(&[Value::Int(7)], rid(1)).unwrap().is_some());
        assert!(cursor.seek(&[Value::Int(7)], rid(2)).unwrap().is_some());

        cursor.delete(&[Value::Int(7)], rid(1)).unwrap();
        assert!(cursor.seek(&[Value::Int(7)], rid(1)).unwrap().is_none());
        assert!(cursor.seek(&[Value::Int(7)], rid(2)).unwrap().is_some());

        std::fs::remove_file(&path).ok();
    }

    fn shuffled(n: u64) -> Vec<u64> {
        // xorshift64, no rand crate — mirrors table_cursor.rs's helper.
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
    fn insert_then_delete_shuffled_matches_reference_model() {
        let path = temp_db_path("shuffled_model");
        let mut pager = new_index_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        let n = 500u64;
        for i in shuffled(n) {
            cursor.insert(vec![Value::Int(i as i64)], rid(i)).unwrap();
        }

        let to_delete = shuffled(n)
            .into_iter()
            .filter(|i| i % 3 == 0)
            .collect::<Vec<_>>();
        for &i in &to_delete {
            cursor.delete(&[Value::Int(i as i64)], rid(i)).unwrap();
        }

        let mut want: Vec<u64> = (1..=n).filter(|i| i % 3 != 0).collect();
        want.sort();
        let got: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect();
        assert_eq!(got, want);

        let stats = check_index_tree(&mut pager, pn(2)).unwrap();
        assert_eq!(stats.rows, want.len());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn seek_and_scan_through_an_interior_page() {
        let path = temp_db_path("through_interior");
        let mut pager = new_index_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        for i in 1i64..=1000 {
            cursor.insert(vec![Value::Int(i)], rid(i as u64)).unwrap();
        }

        for i in [1i64, 500, 1000] {
            assert!(
                cursor
                    .seek(&[Value::Int(i)], rid(i as u64))
                    .unwrap()
                    .is_some()
            );
        }

        let stats = check_index_tree(&mut pager, pn(2)).unwrap();
        assert!(stats.interiors >= 1, "tree should have grown an interior level");
        assert_eq!(stats.rows, 1000);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn range_scan_returns_an_inclusive_bound() {
        let path = temp_db_path("range_inclusive");
        let mut pager = build_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        let got: Vec<i64> = cursor
            .range_scan(&[Value::Int(20)], &[Value::Int(50)])
            .unwrap()
            .into_iter()
            .map(|c| match c.values.as_slice() {
                [Value::Int(k)] => *k,
                other => panic!("unexpected key shape: {other:?}"),
            })
            .collect();

        assert_eq!(got, vec![20, 30, 40, 50]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn range_scan_returns_nothing_outside_the_bounds() {
        let path = temp_db_path("range_empty");
        let mut pager = build_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        assert_eq!(
            cursor
                .range_scan(&[Value::Int(1000)], &[Value::Int(2000)])
                .unwrap(),
            vec![]
        );
        assert_eq!(
            cursor
                .range_scan(&[Value::Int(-100)], &[Value::Int(5)])
                .unwrap(),
            vec![]
        );

        std::fs::remove_file(&path).ok();
    }

    /// The whole point of a stack-based cursor: a run of entries sharing the
    /// same indexed value must be found in full even when it straddles a
    /// leaf boundary, with no sibling pointers to hop across. 300 rows, all
    /// with the same indexed value, disambiguated only by row_id, forces
    /// several splits, so the matching run ends up spread across multiple
    /// leaves by construction.
    #[test]
    fn range_scan_finds_duplicate_values_spanning_a_leaf_boundary() {
        let path = temp_db_path("range_spans_leaves");
        let mut pager = new_index_tree(&path);

        let n = 300u64;
        {
            let mut cursor = IndexCursor::new(&mut pager, pn(2), false);
            for i in 1..=n {
                cursor.insert(vec![Value::Int(7)], rid(i)).unwrap();
            }
            // interleave a different value so it isn't the *only* thing in the tree.
            cursor.insert(vec![Value::Int(8)], rid(n + 1)).unwrap();
        }

        let stats = check_index_tree(&mut pager, pn(2)).unwrap();
        assert!(
            stats.leaves > 1,
            "the duplicate run should have forced more than one leaf"
        );

        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);
        let got: Vec<u64> = cursor
            .range_scan(&[Value::Int(7)], &[Value::Int(7)])
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect();

        let mut want: Vec<u64> = (1..=n).collect();
        want.sort();
        assert_eq!(got, want);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn range_scan_through_several_interior_levels_matches_a_filtered_full_scan() {
        let path = temp_db_path("range_deep");
        let mut pager = new_index_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), false);

        let n = 2000i64;
        for i in shuffled(n as u64) {
            cursor.insert(vec![Value::Int(i as i64)], rid(i)).unwrap();
        }

        let (lo, hi) = (600i64, 900i64);
        let got: Vec<i64> = cursor
            .range_scan(&[Value::Int(lo)], &[Value::Int(hi)])
            .unwrap()
            .into_iter()
            .map(|c| match c.values.as_slice() {
                [Value::Int(k)] => *k,
                other => panic!("unexpected key shape: {other:?}"),
            })
            .collect();

        assert_eq!(got, (lo..=hi).collect::<Vec<i64>>());

        let stats = check_index_tree(&mut pager, pn(2)).unwrap();
        assert!(stats.interiors >= 1);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unique_index_rejects_a_duplicate_value() {
        let path = temp_db_path("unique_reject");
        let mut pager = new_index_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), true);

        cursor.insert(vec![Value::Int(7)], rid(1)).unwrap();
        // same indexed value, different row_id -- still a UNIQUE violation.
        assert!(cursor.insert(vec![Value::Int(7)], rid(2)).is_err());

        assert!(cursor.seek(&[Value::Int(7)], rid(1)).unwrap().is_some());
        assert!(cursor.seek(&[Value::Int(7)], rid(2)).unwrap().is_none());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unique_index_accepts_distinct_values() {
        let path = temp_db_path("unique_accept");
        let mut pager = new_index_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), true);

        for i in 1i64..=50 {
            cursor.insert(vec![Value::Int(i)], rid(i as u64)).unwrap();
        }

        let got: Vec<i64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| match c.values.as_slice() {
                [Value::Int(k)] => *k,
                other => panic!("unexpected key shape: {other:?}"),
            })
            .collect();
        assert_eq!(got, (1i64..=50).collect::<Vec<i64>>());

        std::fs::remove_file(&path).ok();
    }

    /// A rejected UNIQUE insert must bail out before touching any page: the
    /// check runs as a `range_scan` ahead of `insert_into`, so a violation
    /// never allocates a page or mutates the tree.
    #[test]
    fn unique_violation_leaves_the_tree_unchanged() {
        let path = temp_db_path("unique_leaves_unchanged");
        let mut pager = new_index_tree(&path);
        {
            let mut cursor = IndexCursor::new(&mut pager, pn(2), true);
            cursor.insert(vec![Value::Int(7)], rid(1)).unwrap();
        }

        let size_before = std::fs::metadata(&path).unwrap().len();
        let stats_before = check_index_tree(&mut pager, pn(2)).unwrap();

        {
            let mut cursor = IndexCursor::new(&mut pager, pn(2), true);
            assert!(cursor.insert(vec![Value::Int(7)], rid(2)).is_err());
        }

        assert_eq!(std::fs::metadata(&path).unwrap().len(), size_before);
        let stats_after = check_index_tree(&mut pager, pn(2)).unwrap();
        assert_eq!(stats_after.rows, stats_before.rows);

        std::fs::remove_file(&path).ok();
    }

    /// The UNIQUE check compares the whole composite value, not just its
    /// first column: a shared first column with a differing second column
    /// is not a violation.
    #[test]
    fn unique_check_considers_the_full_composite_value_not_just_a_prefix() {
        let path = temp_db_path("unique_full_value");
        let mut pager = new_index_tree(&path);
        let mut cursor = IndexCursor::new(&mut pager, pn(2), true);

        let smith_jane = vec![Value::Text("Smith".into()), Value::Text("Jane".into())];
        let smith_john = vec![Value::Text("Smith".into()), Value::Text("John".into())];

        cursor.insert(smith_jane.clone(), rid(1)).unwrap();
        // shares the first column, differs in the second -- not a duplicate.
        cursor.insert(smith_john.clone(), rid(2)).unwrap();
        // exact same composite value, different row_id -- a real violation.
        assert!(cursor.insert(smith_jane.clone(), rid(3)).is_err());

        let got: Vec<(Vec<Value>, u64)> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| (c.values, c.row_id.get()))
            .collect();
        assert_eq!(got, vec![(smith_jane, 1), (smith_john, 2)]);

        std::fs::remove_file(&path).ok();
    }
}

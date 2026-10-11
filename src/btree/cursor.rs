use crate::file::{
    pager::Pager,
    pages::{Page, leaf::TableLeafCell, types::PageNumber},
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

    fn fetch_page(&mut self, page_number: PageNumber) -> anyhow::Result<Page> {
        let page_bytes = match self.pager.get_page(page_number) {
            Ok(b) => b,
            Err(e) => anyhow::bail!("no page with page number {page_number}: {e}"),
        };

        Page::parse(page_bytes, page_number)
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
                let next_cell_idx = ip.cells.partition_point(|cell| cell.row_id < row_id);
                let next = match ip.cells.get(next_cell_idx) {
                    Some(cell) => cell.left_child,
                    None => ip.rightmost_child,
                };

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::pages::interior::{TableInteriorCell, TableInteriorPage};
    use crate::file::pages::leaf::TableLeafPage;

    const PAGE_SIZE: u32 = 512;

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
            .serialize(page_number, PAGE_SIZE as usize)
            .unwrap();
        pager.write_page(page_number, bytes).unwrap();
    }

    fn write_interior(pager: &mut Pager, page_number: PageNumber, page: TableInteriorPage) {
        let bytes = Page::TableInterior(page)
            .serialize(page_number, PAGE_SIZE as usize)
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
}

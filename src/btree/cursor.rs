use std::collections::HashMap;

use crate::file::pages::{Page, leaf::TableLeafCell, types::PageNumber};

pub struct Cursor<'a> {
    pages: &'a HashMap<PageNumber, Vec<u8>>,
    root_pn: PageNumber,
}

impl<'a> Cursor<'a> {
    pub fn new(pages: &'a HashMap<PageNumber, Vec<u8>>, root_pn: PageNumber) -> Self {
        Self { pages, root_pn }
    }

    pub fn seek(&self, row_id: u64) -> anyhow::Result<Option<TableLeafCell>> {
        self.seek_from(self.root_pn, row_id)
    }

    pub fn scan(&self) -> anyhow::Result<Vec<TableLeafCell>> {
        let mut res = Vec::new();
        self.scan_from(self.root_pn, &mut res)?;
        Ok(res)
    }

    fn fetch_page(&self, page_number: PageNumber) -> anyhow::Result<Page> {
        let page_bytes = match self.pages.get(&page_number) {
            Some(b) => b,
            None => anyhow::bail!("no page with page number {page_number}"),
        };

        Page::parse(page_bytes, page_number)
    }

    fn seek_from(
        &self,
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
        &self,
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

    const PAGE_SIZE: usize = 512;

    fn pn(n: u32) -> PageNumber {
        PageNumber::try_from(n).unwrap()
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

    // Builds this 3-level tree (page numbers deliberately avoid 1, which
    // carries the file-header offset — irrelevant to what Cursor is tested
    // on here):
    //
    //                      root (page 2)
    //                 cell: (page 5, key=30)      rightmost: page 6
    //                /                                        \
    //         mid1 (page 5)                              mid2 (page 6)
    //    cell: (page 10, key=3)  rightmost: 11      cell: (page 12, key=150)  rightmost: 13
    //        /          \                                /            \
    //  leaf 10        leaf 11                        leaf 12        leaf 13
    //  [1,2,3]       [10,20,30]                      [100,150]    [500,600,700]
    fn build_tree() -> HashMap<PageNumber, Vec<u8>> {
        let mid1 = TableInteriorPage {
            cells: vec![TableInteriorCell {
                left_child: pn(10),
                row_id: 3,
            }],
            rightmost_child: pn(11),
        };
        let mid2 = TableInteriorPage {
            cells: vec![TableInteriorCell {
                left_child: pn(12),
                row_id: 150,
            }],
            rightmost_child: pn(13),
        };
        let root = TableInteriorPage {
            cells: vec![TableInteriorCell {
                left_child: pn(5),
                row_id: 30,
            }],
            rightmost_child: pn(6),
        };

        let mut pages = HashMap::new();
        pages.insert(
            pn(10),
            Page::TableLeaf(leaf(&[1, 2, 3]))
                .serialize(pn(10), PAGE_SIZE)
                .unwrap(),
        );
        pages.insert(
            pn(11),
            Page::TableLeaf(leaf(&[10, 20, 30]))
                .serialize(pn(11), PAGE_SIZE)
                .unwrap(),
        );
        pages.insert(
            pn(12),
            Page::TableLeaf(leaf(&[100, 150]))
                .serialize(pn(12), PAGE_SIZE)
                .unwrap(),
        );
        pages.insert(
            pn(13),
            Page::TableLeaf(leaf(&[500, 600, 700]))
                .serialize(pn(13), PAGE_SIZE)
                .unwrap(),
        );
        pages.insert(
            pn(5),
            Page::TableInterior(mid1).serialize(pn(5), PAGE_SIZE).unwrap(),
        );
        pages.insert(
            pn(6),
            Page::TableInterior(mid2).serialize(pn(6), PAGE_SIZE).unwrap(),
        );
        pages.insert(
            pn(2),
            Page::TableInterior(root).serialize(pn(7), PAGE_SIZE).unwrap(),
        );

        pages
    }

    #[test]
    fn seek_finds_every_row_that_exists() {
        let pages = build_tree();
        let cursor = Cursor::new(&pages, pn(2));

        for &row_id in &[1u64, 2, 3, 10, 20, 30, 100, 150, 500, 600, 700] {
            let found = cursor.seek(row_id).unwrap();
            assert_eq!(
                found.map(|c| c.row_id),
                Some(row_id),
                "expected to find row {row_id}"
            );
        }
    }

    #[test]
    fn seek_returns_none_for_rows_that_dont_exist() {
        let pages = build_tree();
        let cursor = Cursor::new(&pages, pn(2));

        // includes: below everything, between leaves within one subtree,
        // between the two second-level subtrees, and above everything.
        for &row_id in &[0u64, 4, 15, 50, 99, 151, 999] {
            assert_eq!(
                cursor.seek(row_id).unwrap(),
                None,
                "expected row {row_id} to be absent"
            );
        }
    }

    #[test]
    fn scan_returns_every_row_in_ascending_order() {
        let pages = build_tree();
        let cursor = Cursor::new(&pages, pn(2));

        let got: Vec<u64> = cursor
            .scan()
            .unwrap()
            .into_iter()
            .map(|c| c.row_id)
            .collect();

        assert_eq!(got, vec![1, 2, 3, 10, 20, 30, 100, 150, 500, 600, 700]);
    }
}

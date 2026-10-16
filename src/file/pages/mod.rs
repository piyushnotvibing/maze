pub mod cell;
pub mod header;
pub mod interior;
pub mod leaf;
pub mod spec;
pub mod table;
pub mod trunk;
pub mod types;

mod bytes;

use crate::file::pages::{
    cell::Cell, interior::TableInteriorPage, leaf::TableLeafPage, spec::PageSpec, table::TablePage,
    types::PageNumber,
};

// Each cell pointer in a page is 2 bytes long (they are u16s).
pub const CELL_PTR_SIZE: usize = 2;

#[derive(Debug, Clone)]
pub enum Page {
    TableLeaf(TableLeafPage),
    TableInterior(TableInteriorPage),
}

impl Page {
    pub fn parse(mut buf: &[u8], ps: PageSpec) -> anyhow::Result<Page> {
        buf = buf
            .get(ps.base_offset()..)
            .ok_or_else(|| anyhow::anyhow!("page smaller than page header"))?;

        let page_type = match PageType::try_from(
            *buf.first()
                .ok_or_else(|| anyhow::anyhow!("page has nothing past its header"))?,
        ) {
            Ok(pt) => pt,
            Err(e) => anyhow::bail!("{e}"),
        };

        match page_type {
            PageType::TableLeaf => Ok(Page::TableLeaf(TableLeafPage::parse(buf, ps)?)),
            PageType::TableInterior => Ok(Page::TableInterior(TableInteriorPage::parse(buf, ps)?)),
        }
    }

    pub fn serialize(&self, ps: PageSpec) -> anyhow::Result<Vec<u8>> {
        match self {
            Page::TableLeaf(lp) => lp.serialize(ps),
            Page::TableInterior(ip) => ip.serialize(ps),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) enum PageType {
    TableLeaf,
    TableInterior,
}

impl PageType {
    pub(crate) fn serialize<C: Cell>(
        &self,
        cells: &[C],
        rightmost_child: Option<PageNumber>,
        ps: PageSpec,
    ) -> anyhow::Result<Vec<u8>> {
        let cells_bytes = cells
            .iter()
            .map(|c| c.serialize())
            .collect::<Vec<Vec<u8>>>();

        self.pack(&cells_bytes, rightmost_child, ps)
    }

    fn pack(
        self,
        cells: &[Vec<u8>],
        rightmost_child: Option<PageNumber>,
        ps: PageSpec,
    ) -> anyhow::Result<Vec<u8>> {
        match self {
            PageType::TableInterior if rightmost_child.is_none() => {
                anyhow::bail!("interior page has no rightmost child")
            }
            PageType::TableLeaf if let Some(_) = rightmost_child => {
                anyhow::bail!("how tf does leaf page have a rightmost child bro?")
            }
            _ => {}
        }
        let page_offset = ps.base_offset();

        let mut p_buf = vec![0u8; ps.page_size()];

        /*
            A page's header contains:
                1. page type
                2. cell count,
                3. cell content offset and
                4. rightmost child page number
        */
        let header_size = self.header_size();
        let all_cells_len: usize = cells.iter().map(Vec::len).sum();
        let cell_count = cells.len();
        let cell_ptrs_region_end = header_size + 2 * cell_count;
        let cell_content_offset = ps
            .page_size()
            .checked_sub(page_offset)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "this error does not even make sense, if it happens i'll shave my moustache"
                )
            })?
            .checked_sub(all_cells_len)
            .filter(|&offset| offset >= cell_ptrs_region_end) // only get the calculated offset if it is greater than the last cell pointer's address
            .ok_or_else(|| anyhow::anyhow!("cells do not fit in page"))?;
        // an empty page can only hit exactly page_size here when page_offset == 0 (i.e. not page 1) — page 1's relative offset is always < page_size
        let stored_cell_content_offset = if cell_content_offset == 65536 {
            0
        } else {
            cell_content_offset as u16
        };

        // Writing the page header.
        p_buf[page_offset] = self.into();
        p_buf[(page_offset + 1)..(page_offset + 3)]
            .copy_from_slice(&u16::try_from(cell_count)?.to_be_bytes());
        p_buf[(page_offset + 3)..(page_offset + 5)]
            .copy_from_slice(&stored_cell_content_offset.to_be_bytes());
        if let Some(rc) = rightmost_child {
            p_buf[(page_offset + 5)..(page_offset + 9)].copy_from_slice(&rc.get().to_be_bytes());
        }

        let mut cell_ptr = cell_content_offset;
        for (i, cell) in cells.iter().enumerate() {
            // Copy the cell, at its real (absolute) position in the buffer.
            let abs_cell_start = page_offset + cell_ptr;
            p_buf[abs_cell_start..abs_cell_start + cell.len()].copy_from_slice(cell);

            let cell_ptr_addr = page_offset + header_size + 2 * i;
            p_buf[cell_ptr_addr..cell_ptr_addr + 2]
                .copy_from_slice(&u16::try_from(cell_ptr)?.to_be_bytes());

            cell_ptr += cell.len()
        }

        Ok(p_buf)
    }

    pub fn header_size(&self) -> usize {
        match self {
            PageType::TableLeaf => 5,
            PageType::TableInterior => 9,
        }
    }
}

impl From<PageType> for u8 {
    fn from(value: PageType) -> Self {
        match value {
            PageType::TableLeaf => 13,
            PageType::TableInterior => 5,
        }
    }
}

impl TryFrom<u8> for PageType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> anyhow::Result<Self> {
        match value {
            13 => Ok(PageType::TableLeaf),
            5 => Ok(PageType::TableInterior),
            _ => anyhow::bail!("invalid page type"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{
        header::MAZE_HEADER_SIZE,
        pages::{
            interior::{TableInteriorCell, TableInteriorPage},
            leaf::TableLeafCell,
        },
        varint,
    };

    // Build one cell: varint(payload len) | varint(row_id) | payload
    fn cell_bytes(row_id: u64, payload: &[u8]) -> Vec<u8> {
        let mut c = varint::encode(payload.len() as u64);
        c.extend(varint::encode(row_id));
        c.extend_from_slice(payload);
        c
    }

    #[test]
    fn page_parses_correctly() {
        const PAGE_SIZE: usize = 512;
        // Page 1: the leaf page starts right after the file header, and (as in your
        // original test) pointers and the content offset are relative to that start.
        let base = MAZE_HEADER_SIZE;
        let leaf_len = PAGE_SIZE - base;

        // Mix of 1-byte and multi-byte varints: len 3 / id 1, and len 200 / id 300
        // (200 and 300 both need 2 bytes each).
        let rows: Vec<(u64, Vec<u8>)> = vec![(1, b"abc".to_vec()), (300, vec![7u8; 200])];
        let cells: Vec<Vec<u8>> = rows.iter().map(|(id, p)| cell_bytes(*id, p)).collect();

        // Pack cells contiguously at the end of the leaf; offsets come from real lengths.
        let total: usize = cells.iter().map(Vec::len).sum();
        let content_offset = leaf_len - total;

        let mut buf = vec![0u8; PAGE_SIZE];
        let mut ptrs: Vec<u16> = Vec::new();
        let mut at = content_offset;
        for c in &cells {
            ptrs.push(at as u16);
            buf[base + at..base + at + c.len()].copy_from_slice(c);
            at += c.len();
        }

        // Leaf header: type(1) | cell count(2) | content offset(2), then the pointer array.
        buf[base] = PageType::TableLeaf.into();
        buf[base + 1..base + 3].copy_from_slice(&(cells.len() as u16).to_be_bytes());
        buf[base + 3..base + 5].copy_from_slice(&(content_offset as u16).to_be_bytes());
        for (i, p) in ptrs.iter().enumerate() {
            let o = base + 5 + 2 * i;
            buf[o..o + 2].copy_from_slice(&p.to_be_bytes());
        }

        let ps = PageSpec::new(PAGE_SIZE, PageNumber::try_from(1).unwrap());
        let page = Page::parse(&buf, ps).unwrap();
        if let Page::TableLeaf(leaf) = page {
            assert_eq!(leaf.cells.len(), rows.len());
            for (cell, (id, payload)) in leaf.cells.iter().zip(&rows) {
                assert_eq!(cell.payload.len(), payload.len());
                assert_eq!(cell.row_id, *id);
                assert_eq!(&cell.payload, payload);
            }
        } else {
            panic!("invalid page type")
        }
    }

    // Regression test for the bug where `pack`'s cell-pointer-array write
    // forgot to add `page_offset`: for page 1, pointers landed in the
    // reserved file-header region instead of after the page's own header,
    // so a leaf/interior page with real cells failed to round-trip through
    // serialize -> parse.
    #[test]
    fn leaf_page_round_trips_at_page_number_one() {
        const PAGE_SIZE: usize = 512;
        let pn1 = PageNumber::try_from(1).unwrap();
        let ps = PageSpec::new(PAGE_SIZE, pn1);

        let original = TableLeafPage {
            cells: vec![
                TableLeafCell {
                    row_id: 1,
                    payload: b"abc".to_vec(),
                },
                TableLeafCell {
                    row_id: 300,
                    payload: vec![7u8; 50],
                },
            ],
        };

        let bytes = Page::TableLeaf(original.clone()).serialize(ps).unwrap();

        // The file-header region must stay untouched by page-serialization
        // code — only the Pager/MazeHeader should ever write there.
        assert_eq!(
            &bytes[0..MAZE_HEADER_SIZE],
            vec![0u8; MAZE_HEADER_SIZE].as_slice()
        );

        match Page::parse(&bytes, ps).unwrap() {
            Page::TableLeaf(parsed) => assert_eq!(parsed, original),
            Page::TableInterior(_) => panic!("expected a leaf page back"),
        }
    }

    #[test]
    fn interior_page_round_trips_at_page_number_one() {
        const PAGE_SIZE: usize = 512;
        let pn1 = PageNumber::try_from(1).unwrap();
        let ps = PageSpec::new(PAGE_SIZE, pn1);

        let original = TableInteriorPage {
            cells: vec![TableInteriorCell {
                left_child: PageNumber::try_from(5).unwrap(),
                row_id: 100,
            }],
            rightmost_child: PageNumber::try_from(6).unwrap(),
        };

        let bytes = Page::TableInterior(original.clone()).serialize(ps).unwrap();

        assert_eq!(
            &bytes[0..MAZE_HEADER_SIZE],
            vec![0u8; MAZE_HEADER_SIZE].as_slice()
        );

        match Page::parse(&bytes, ps).unwrap() {
            Page::TableInterior(parsed) => assert_eq!(parsed, original),
            Page::TableLeaf(_) => panic!("expected an interior page back"),
        }
    }
}

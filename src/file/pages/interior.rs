/*
    Interior b-tree page structure:

    ┌───────────────────────────────────────────────┐
    │ type (1) | cell count (2) | content offset (2)│  header, 9 bytes in your format
    │ rightmost child page number (4)               │
    ├───────────────────────────────────────────────┤
    │ cell pointer array: u16 × cell_count          │
    ├───────────────────────────────────────────────┤
    │ free space                                    │
    ├───────────────────────────────────────────────┤
    │ cells, filled from the end of the page        │
    └───────────────────────────────────────────────┘

    interior cell = u32 left_child (4 bytes, big-endian) | varint row_id
*/

use crate::{
    file::{
        pages::{PageNumber, PageType, pack_page, parse_cell_ptrs, parse_header},
        varint,
    },
    utils::read_word_u32_be,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInteriorPage {
    pub cells: Vec<TableInteriorCell>,
    pub rightmost_child: PageNumber,
}

impl TableInteriorPage {
    pub fn serialize(&self, usable_page_len: usize) -> anyhow::Result<Vec<u8>> {
        let mut prev_row_id: Option<u64> = None;
        for cell in self.cells.iter() {
            if let Some(prev_row_id) = prev_row_id {
                if cell.row_id <= prev_row_id {
                    anyhow::bail!("interior page cells not in sorted order")
                }
            }
            prev_row_id = Some(cell.row_id);
        }

        let cells: Vec<Vec<u8>> = self
            .cells
            .iter()
            .map(|cell| cell.serialize())
            .collect::<Vec<Vec<u8>>>();

        pack_page(
            PageType::TableInterior,
            Some(self.rightmost_child),
            &cells,
            usable_page_len,
        )
    }
}

impl TryFrom<&[u8]> for TableInteriorPage {
    type Error = anyhow::Error;

    fn try_from(buf: &[u8]) -> anyhow::Result<Self> {
        let header = parse_header(buf, PageType::TableInterior)?;
        if header.cell_count == 0 {
            anyhow::bail!("interior page has no cells")
        }
        if header.rightmost_child.is_none() {
            anyhow::bail!("interior page has no rightmost child")
        }

        let cell_content_region_start = match header.cell_content_offset {
            0 => 65536usize,
            n => n as usize,
        };
        let cell_ptr_region_end =
            PageType::TableInterior.header_size() + 2 * header.cell_count as usize;

        if cell_ptr_region_end > cell_content_region_start || cell_content_region_start > buf.len()
        {
            anyhow::bail!("invalid cell content offset")
        }

        let content_buffer = buf
            .get(PageType::TableInterior.header_size()..)
            .ok_or_else(|| anyhow::anyhow!("interior page smaller than interior page header"))?;
        let cell_ptrs = parse_cell_ptrs(content_buffer, header.cell_count as usize)?;

        let mut prev_row_id: Option<u64> = None;
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(cell_ptrs.len());
        let cells = cell_ptrs
            .iter()
            .map(|&ptr| {
                // If a cell pointer points anywhere other than the cell area of the page, then error out.
                if (ptr as usize) < cell_content_region_start {
                    anyhow::bail!(
                        "invalid cell pointer {ptr}: points outside the designated cell content region"
                    )
                }

                let ptr_usize = ptr as usize;
                let res = TableInteriorCell::try_from(
                    buf.get(ptr_usize..)
                        .ok_or_else(|| anyhow::anyhow!("cell out of bounds"))?,
                )?;
                let end = ptr_usize
                    .checked_add(res.encoded_len())
                    .ok_or_else(|| anyhow::anyhow!("cell end overflows"))?;
                if end > buf.len() {
                    anyhow::bail!("cell extends past end of page");
                }
                spans.push((ptr_usize, end));

                if let Some(prev) = prev_row_id {
                    if res.row_id <= prev {
                        anyhow::bail!("cell pointers not sorted by row_id")
                    }
                }
                prev_row_id = Some(res.row_id);

                Ok(res)
            })
            .collect::<anyhow::Result<Vec<TableInteriorCell>>>()?;

        // Pointer order is key order, not physical order — sort by offset to
        // detect overlaps/aliasing.
        spans.sort_by_key(|&(start, _)| start);
        for w in spans.windows(2) {
            if w[1].0 < w[0].1 {
                anyhow::bail!("corrupted buffer: overlapping cells");
            }
        }

        Ok(Self {
            cells,
            rightmost_child: header.rightmost_child.unwrap(),
        })
    }
}

// Min size = 4 + 1 = 5B; Max size = 4 + 9 = 13B.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInteriorCell {
    // Page number (essentially a pointer) to another TableInteriorPage or TableLeafPage
    pub left_child: PageNumber,
    // Varint encoded on disk.
    pub row_id: u64,
}

impl TableInteriorCell {
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = self.left_child.get().to_be_bytes().to_vec();
        bytes.extend_from_slice(&varint::encode(self.row_id));

        bytes
    }

    pub fn encoded_len(&self) -> usize {
        std::mem::size_of::<u32>() + varint::encode(self.row_id).len()
    }
}

impl TryFrom<&[u8]> for TableInteriorCell {
    type Error = anyhow::Error;

    fn try_from(buf: &[u8]) -> anyhow::Result<Self> {
        let left_child = PageNumber::try_from(read_word_u32_be(buf, 0)?)?;
        // &buf[4..] won't panic here because above line would've errored if buffer wasn't at least 4 bytes long
        let (row_id, _) =
            varint::decode(&buf[4..]).ok_or_else(|| anyhow::anyhow!("invalid row_id varint"))?;

        Ok(Self { left_child, row_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{header::HEADER_SIZE, pages::Page};

    const PAGE_SIZE: usize = 512;

    fn pn(n: u32) -> PageNumber {
        PageNumber::try_from(n).unwrap()
    }

    fn interior_cell_bytes(left_child: u32, row_id: u64) -> Vec<u8> {
        let mut c = left_child.to_be_bytes().to_vec();
        c.extend(varint::encode(row_id));
        c
    }

    /// Builds a full page buffer with cells packed at the end of the page.
    /// `base` is 0 for an ordinary page and HEADER_SIZE for page 1.
    /// Returns (buffer, pointers as stored on disk, content offset).
    fn build_interior(
        base: usize,
        cells: &[(u32, u64)],
        rightmost: u32,
    ) -> (Vec<u8>, Vec<u16>, usize) {
        let region_len = PAGE_SIZE - base;
        let encoded: Vec<Vec<u8>> = cells
            .iter()
            .map(|&(c, k)| interior_cell_bytes(c, k))
            .collect();
        let total: usize = encoded.iter().map(Vec::len).sum();
        let content_offset = region_len - total;
        assert!(
            content_offset >= PageType::TableInterior.header_size() + 2 * cells.len(),
            "cells don't fit"
        );

        let mut buf = vec![0u8; PAGE_SIZE];
        let mut ptrs = Vec::new();
        let mut at = content_offset;
        for c in &encoded {
            ptrs.push(at as u16);
            buf[base + at..base + at + c.len()].copy_from_slice(c);
            at += c.len();
        }

        buf[base] = PageType::TableInterior.into();
        buf[base + 1..base + 3].copy_from_slice(&(cells.len() as u16).to_be_bytes());
        buf[base + 3..base + 5].copy_from_slice(&(content_offset as u16).to_be_bytes());
        buf[base + 5..base + 9].copy_from_slice(&rightmost.to_be_bytes());
        for (i, p) in ptrs.iter().enumerate() {
            let o = base + PageType::TableInterior.header_size() + 2 * i;
            buf[o..o + 2].copy_from_slice(&p.to_be_bytes());
        }
        (buf, ptrs, content_offset)
    }

    fn assert_rejects(buf: &[u8]) {
        assert!(TableInteriorPage::try_from(buf).is_err());
    }

    // ---------- valid pages ----------

    #[test]
    fn parses_valid_interior_page() {
        // key 300 needs a 2-byte varint, key 100 needs 1
        let cells = [(3, 100), (4, 300)];
        let (buf, _ptrs, _content_offset) = build_interior(0, &cells, 5);

        let page = TableInteriorPage::try_from(buf.as_slice()).unwrap();

        assert_eq!(page.rightmost_child, pn(5));
        assert_eq!(page.cells.len(), cells.len());
        for (cell, &(child, key)) in page.cells.iter().zip(&cells) {
            assert_eq!(cell.left_child, pn(child));
            assert_eq!(cell.row_id, key);
        }
    }

    #[test]
    fn pointer_order_is_key_order_not_physical_order() {
        // Two equal-length cells (5 bytes each), then swap their bytes and pointers,
        // so the pointer array stays sorted by key but cell positions are reversed.
        let (mut buf, ptrs, _) = build_interior(0, &[(3, 10), (4, 20)], 5);
        let (a, b) = (ptrs[0] as usize, ptrs[1] as usize);
        assert_eq!(b, a + 5);

        let first = buf[a..a + 5].to_vec();
        let second = buf[b..b + 5].to_vec();
        buf[a..a + 5].copy_from_slice(&second);
        buf[b..b + 5].copy_from_slice(&first);
        buf[9..11].copy_from_slice(&(b as u16).to_be_bytes());
        buf[11..13].copy_from_slice(&(a as u16).to_be_bytes());

        let page = TableInteriorPage::try_from(buf.as_slice()).unwrap();
        assert_eq!(page.cells.len(), 2);
        assert_eq!(
            (page.cells[0].left_child, page.cells[0].row_id),
            (pn(3), 10)
        );
        assert_eq!(
            (page.cells[1].left_child, page.cells[1].row_id),
            (pn(4), 20)
        );
    }

    #[test]
    fn page_enum_dispatches_to_interior() {
        // (page number, base offset): page 1 sits after the file header
        for (number, base) in [(1u32, HEADER_SIZE), (2, 0)] {
            let (buf, _, _) = build_interior(base, &[(3, 100), (4, 300)], 5);
            match Page::parse(&buf, pn(number)).unwrap() {
                Page::TableInterior(p) => {
                    assert_eq!(p.cells.len(), 2);
                    assert_eq!(p.rightmost_child, pn(5));
                }
                _ => panic!("expected interior page for page {number}"),
            }
        }
    }

    // ---------- invalid pages: must be Err, never a panic ----------

    #[test]
    fn rejects_empty_buffer() {
        assert_rejects(&[]);
    }

    #[test]
    fn rejects_truncated_cell() {
        // the last cell ends in a 2-byte varint (300); drop its final byte
        let (buf, _, _) = build_interior(0, &[(3, 300)], 5);
        assert_rejects(&buf[..PAGE_SIZE - 1]);
    }

    #[test]
    fn rejects_wrong_page_type_byte() {
        let (mut buf, _, _) = build_interior(0, &[(3, 10)], 5);
        buf[0] = PageType::TableLeaf.into();
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_zero_cells() {
        let (buf, _, _) = build_interior(0, &[], 5);
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_zero_rightmost_child() {
        let (buf, _, _) = build_interior(0, &[(3, 10)], 0);
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_zero_left_child() {
        let (buf, _, _) = build_interior(0, &[(0, 10)], 5);
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_duplicate_keys() {
        let (buf, _, _) = build_interior(0, &[(3, 10), (4, 10)], 5);
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_descending_keys() {
        let (buf, _, _) = build_interior(0, &[(3, 20), (4, 10)], 5);
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_pointer_into_header() {
        let (mut buf, _, _) = build_interior(0, &[(3, 10)], 5);
        buf[9..11].copy_from_slice(&2u16.to_be_bytes());
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_pointer_into_free_space() {
        let (mut buf, _, content_offset) = build_interior(0, &[(3, 10)], 5);
        buf[9..11].copy_from_slice(&((content_offset - 1) as u16).to_be_bytes());
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_pointer_past_end_of_page() {
        let (mut buf, _, _) = build_interior(0, &[(3, 10)], 5);
        buf[9..11].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_pointer_array_overlapping_cells() {
        // claim 1000 cells: the pointer array would run into the cell area
        let (mut buf, _, _) = build_interior(0, &[(3, 10)], 5);
        buf[1..3].copy_from_slice(&1000u16.to_be_bytes());
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_content_offset_past_buffer() {
        let (mut buf, _, _) = build_interior(0, &[(3, 10)], 5);
        buf[3..5].copy_from_slice(&600u16.to_be_bytes());
        assert_rejects(&buf);
    }

    #[test]
    fn rejects_duplicate_pointers() {
        // Both pointers at the first cell: fires the duplicate-key check and
        // the overlap check. (A pure sum-of-lengths check would pass here —
        // two 5-byte cells fit easily — so this also guards the interval logic.)
        let (mut buf, ptrs, _) = build_interior(0, &[(3, 10), (4, 20)], 5);
        buf[11..13].copy_from_slice(&ptrs[0].to_be_bytes());
        assert_rejects(&buf);
    }
}

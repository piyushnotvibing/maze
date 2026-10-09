use crate::file::{
    pages::{PageType, pack_page, parse_cell_ptrs, parse_header},
    record::{Record, Value},
    varint,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableLeafPage {
    pub cells: Vec<TableLeafCell>,
}

impl TableLeafPage {
    pub fn serialize(&self, usable_page_len: usize) -> anyhow::Result<Vec<u8>> {
        let cells: Vec<Vec<u8>> = self
            .cells
            .iter()
            .map(|cell| cell.serialize())
            .collect::<Vec<Vec<u8>>>();
        let no_rightmost_child = None;
        pack_page(
            PageType::TableLeaf,
            no_rightmost_child,
            &cells,
            usable_page_len,
        )
    }
}

impl TryFrom<&[u8]> for TableLeafPage {
    type Error = anyhow::Error;

    fn try_from(buf: &[u8]) -> anyhow::Result<Self> {
        let header = parse_header(buf, PageType::TableLeaf)?;

        let cell_content_region_start = match header.cell_content_offset {
            0 => 65536usize,
            n => n as usize,
        };
        let cell_ptr_region_end =
            PageType::TableLeaf.header_size() + 2 * header.cell_count as usize;

        if cell_ptr_region_end > cell_content_region_start || cell_content_region_start > buf.len()
        {
            anyhow::bail!("invalid cell content offset")
        }

        let content_buffer = buf
            .get(PageType::TableLeaf.header_size()..)
            .ok_or_else(|| anyhow::anyhow!("leaf page smaller than leaf page header"))?;
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
                let res = TableLeafCell::try_from(
                    buf.get(ptr_usize..)
                        .ok_or_else(|| anyhow::anyhow!("cell out of bounds"))?,
                )?;
                let len = res.encoded_len();
                let end = ptr_usize
                    .checked_add(len)
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
            .collect::<anyhow::Result<Vec<TableLeafCell>>>()?;

        // Cells may be in key order, not physical order — sort by offset to
        // detect overlaps/aliasing (e.g. a pointer into another cell's payload).
        spans.sort_by_key(|&(start, _)| start);
        for w in spans.windows(2) {
            if w[1].0 < w[0].1 {
                anyhow::bail!("corrupted buffer: overlapping cells");
            }
        }

        Ok(Self { cells })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableLeafCell {
    pub row_id: u64, // varint encoded on disk
    pub payload: Vec<u8>,
}

impl TableLeafCell {
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = varint::encode(self.payload.len() as u64);
        bytes.extend(varint::encode(self.row_id));
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    pub fn decode_payload(&self) -> anyhow::Result<Vec<Value>> {
        let record = Record::build(&self.payload)?;
        Ok(record.body)
    }

    pub fn encoded_len(&self) -> usize {
        varint::encode(self.payload.len() as u64).len()
            + varint::encode(self.row_id).len()
            + self.payload.len()
    }
}

impl TryFrom<&[u8]> for TableLeafCell {
    type Error = anyhow::Error;

    fn try_from(buf: &[u8]) -> anyhow::Result<Self> {
        let (size, size_bytes_read) =
            varint::decode(buf).ok_or(anyhow::anyhow!("invalid cell size varint"))?;
        let (row_id, rowid_bytes_read) = varint::decode(&buf[size_bytes_read..])
            .ok_or(anyhow::anyhow!("invalid row_id varint"))?;

        let start = size_bytes_read + rowid_bytes_read;
        let end = usize::try_from(size)
            .ok()
            .and_then(|s| start.checked_add(s))
            .ok_or_else(|| anyhow::anyhow!("payload size out of range"))?;
        let payload = buf
            .get(start..end)
            .ok_or_else(|| anyhow::anyhow!("payload extends past buffer"))?
            .to_vec();

        Ok(Self { row_id, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{
        header::HEADER_SIZE,
        pages::{Page, PageNumber, leaf::TableLeafCell},
        record::{Record, Value},
    };

    // Build one cell: varint(payload len) | varint(row_id) | payload
    fn cell_bytes(row_id: u64, payload: &[u8]) -> Vec<u8> {
        let mut c = varint::encode(payload.len() as u64);
        c.extend(varint::encode(row_id));
        c.extend_from_slice(payload);
        c
    }

    #[test]
    fn table_leaf_cell_roundtrip() {
        let cell = TableLeafCell {
            row_id: 300,
            payload: b"abc".to_vec(),
        };
        let parsed = TableLeafCell::try_from(cell.serialize().as_slice()).unwrap();
        assert_eq!((parsed.row_id, parsed.payload), (300, b"abc".to_vec()));
    }

    #[test]
    fn payload_decodes_into_a_value_array_correctly() {
        const PAGE_SIZE: usize = 512;
        let base = HEADER_SIZE; // page 1: leaf starts right after the file header
        let leaf_len = PAGE_SIZE - base;

        // Each row: (row_id, column values). Row 2 has a 200-byte blob and rowid 300,
        // so its size varint, rowid varint, and blob serial type are all 2 bytes.
        let rows: Vec<(u64, Vec<Value>)> = vec![
            (
                1,
                vec![Value::Int(-5), Value::Text("Ada".into()), Value::Null],
            ),
            (
                300,
                vec![
                    Value::Blob(vec![7u8; 200]),
                    Value::Float(1.5),
                    Value::Text("x".into()),
                ],
            ),
        ];

        // payload = encoded record; cell = varint(payload len) | varint(row_id) | payload
        let cells: Vec<Vec<u8>> = rows
            .iter()
            .map(|(id, values)| {
                let payload = Record::new(values.clone()).unwrap().encode();
                cell_bytes(*id, &payload)
            })
            .collect();

        // Pack cells contiguously at the end of the leaf; offsets come from real lengths.
        let total: usize = cells.iter().map(Vec::len).sum();
        let content_offset = leaf_len - total;
        let ptrs_end = 5 + 2 * cells.len();
        assert!(
            content_offset >= ptrs_end,
            "test cells don't fit in the page"
        );

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

        let page_number = PageNumber(1);
        let page = Page::parse(&buf, page_number).unwrap();
        if let Page::TableLeaf(leaf) = page {
            assert_eq!(leaf.cells.len(), rows.len());
            for (cell, (row_id, expected)) in leaf.cells.iter().zip(&rows) {
                assert_eq!(cell.row_id, *row_id);
                assert_eq!(&cell.decode_payload().unwrap(), expected);
            }
        } else {
            panic!("invalid page type")
        }
    }

    #[test]
    fn empty_leaf_page_is_valid() {
        let mut buf = vec![0u8; 512];
        buf[0] = PageType::TableLeaf.into();
        buf[3..5].copy_from_slice(&512u16.to_be_bytes()); // cell area starts at page end
        let leaf = TableLeafPage::try_from(buf.as_slice()).unwrap();
        assert!(leaf.cells.is_empty());
    }

    #[test]
    fn rejects_pointer_into_sibling_payload() {
        // Aliasing attack: cell B's exact bytes are embedded at the start of
        // cell A's payload. Pointer 1 targets B's bytes inside A, so a naive
        // parser sees two sorted, in-bounds cells (row_ids 1 then 2).
        const PAGE_SIZE: usize = 512;
        const LEAF_HEADER: usize = 5;
        const PTR_END: usize = LEAF_HEADER + 2 * 2; // 9

        // Cell B (row_id 2, small payload).
        let b_payload = b"XYZ".to_vec();
        let mut b_bytes = varint::encode(b_payload.len() as u64);
        b_bytes.extend(varint::encode(2));
        b_bytes.extend_from_slice(&b_payload);

        // Cell A (row_id 1) fills everything from PTR_END to end of page,
        // with B's bytes at the start of its payload.
        let a_start = PTR_END;
        let a_len = PAGE_SIZE - a_start;
        // 500-byte payload needs a 2-byte size varint + 1-byte row_id varint.
        let payload_len = a_len - 3;
        let size_varint = varint::encode(payload_len as u64);
        let rowid_varint = varint::encode(1);
        assert_eq!(size_varint.len() + rowid_varint.len(), 3);
        let mut payload = b_bytes.clone();
        payload.resize(payload_len, 0xAA);
        let mut a_bytes = size_varint;
        a_bytes.extend(rowid_varint);
        a_bytes.extend_from_slice(&payload);
        assert_eq!(a_bytes.len(), a_len);

        let mut buf = vec![0u8; PAGE_SIZE];
        buf[0] = PageType::TableLeaf.into();
        buf[1..3].copy_from_slice(&2u16.to_be_bytes());
        buf[3..5].copy_from_slice(&(a_start as u16).to_be_bytes());
        buf[5..7].copy_from_slice(&(a_start as u16).to_be_bytes());
        let b_ptr = (a_start + 3) as u16; // A's start + A's two varints
        buf[7..9].copy_from_slice(&b_ptr.to_be_bytes());
        buf[a_start..a_start + a_len].copy_from_slice(&a_bytes);

        // Sanity: pointer 1 really does decode as a valid row_id-2 cell.
        let inner = TableLeafCell::try_from(&buf[b_ptr as usize..]).unwrap();
        assert_eq!(inner.row_id, 2);
        assert_eq!(inner.payload, b_payload);

        assert!(TableLeafPage::try_from(buf.as_slice()).is_err());
    }
}

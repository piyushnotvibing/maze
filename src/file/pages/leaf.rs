use crate::file::{
    header::MAZE_HEADER_SIZE,
    pages::{
        PageHeader, PageType,
        cell::{Cell, parse_cells},
        types::PageNumber,
    },
    varint,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableLeafPage {
    pub cells: Vec<TableLeafCell>,
}

impl TableLeafPage {
    pub fn new(cells: Vec<TableLeafCell>) -> Self {
        Self { cells }
    }

    pub fn cell_len(&self) -> usize {
        self.cells
            .iter()
            .map(|cell| cell.encoded_len())
            .sum::<usize>()
    }

    pub fn serialize(&self, page_number: PageNumber, page_size: usize) -> anyhow::Result<Vec<u8>> {
        let no_rightmost_child = None;
        PageType::TableLeaf.serialize(page_number, no_rightmost_child, &self.cells, page_size)
    }

    /// Assumes that the caller (Cursor) has already checked if the cell fits. If it
    /// doesn't it also assumes the insertion is being done in the right leaf page formed
    /// after the split.
    pub fn insert(
        &mut self,
        new_cell: TableLeafCell,
        page_number: PageNumber,
        page_size: usize,
    ) -> anyhow::Result<()> {
        let new_row_id = new_cell.row_id;
        let insert_idx = self.cells.partition_point(|cell| cell.row_id < new_row_id);
        if self
            .cells
            .get(insert_idx)
            .is_some_and(|c| c.row_id == new_row_id)
        {
            anyhow::bail!("row_id {new_row_id} already exists")
        }

        if !self.fits(&new_cell, page_size, page_number) {
            anyhow::bail!("new cell does not fit in this leaf page")
        }

        self.cells.insert(insert_idx, new_cell);

        Ok(())
    }

    /// Caller (Cursor) should call this before insertion.
    pub fn fits(
        &self,
        new_cell: &TableLeafCell,
        page_size: usize,
        page_number: PageNumber,
    ) -> bool {
        /*
            let's make some things clear: cell_count = self.cells.len(), total_cell_len = the length of all the cells combined

            if page_number == 1: checks if page_size >= (MAZE_HEADER_SIZE + leaf page header size + (2*(cell_count + 1)) + (total_cell_len + length of the new cell))
            else: checks if page_size >= PageType::TableLeaf.header_size() + (2*(cell_count + 1)) + (total_cell_len + new_cell.serialize().len()))
        */
        let (cell_count, total_cell_len) = (self.cells.len(), self.cell_len());

        match page_number.get() {
            1 => {
                page_size
                    >= MAZE_HEADER_SIZE
                        + PageType::TableLeaf.header_size()
                        + 2 * (cell_count + 1)
                        + (total_cell_len + new_cell.encoded_len())
            }
            _ => {
                page_size
                    >= PageType::TableLeaf.header_size()
                        + 2 * (cell_count + 1)
                        + (total_cell_len + new_cell.encoded_len())
            }
        }
    }

    /// Caller should call this if a new cell does not fit into the
    /// leaf page it's supposed to be inserted in. Pseudocode:
    /// ```pseudocode
    /// if lp.fits(new_cell) then
    ///     lp.insert(new_cell)
    /// else
    ///     new_pn = cursor.pager.allocate_page()
    ///     new_lp, divider = lp.split() {where divider = max(left_lp), new_lp = right_lp}
    ///     
    ///     if new_cell.row_id <= new_split_row_id then lp.insert(new_cell)
    ///     else new_lp.insert(new_cell), cursor.pager.write_page(new_pn, new_lp.serialize())
    ///     
    ///     insert new interior cell into new_lp's & lp's parent and handle its split (if it splits)    
    ///     
    ///     cursor.pager.write_page(pn, lp.serialize())
    /// ```
    pub fn split(
        &mut self,
        self_pn: PageNumber,
        right_pn: PageNumber,
        page_size: usize,
    ) -> anyhow::Result<(Self, u64)> {
        if self.cells.len() < 2 {
            anyhow::bail!("cannot split a page with less than 2 cells")
        }

        let total_cell_len = self.cell_len();
        let target = total_cell_len / 2;
        let (mut running, mut split_idx) = (0, 0);
        for (i, cell) in self.cells.iter().enumerate() {
            running += cell.encoded_len();
            if running >= target {
                split_idx = i + 1;
                break;
            }
        }
        let mut split_idx = split_idx.clamp(1, self.cells.len() - 1);

        let mut right = self.cells.split_off(split_idx);
        let mut divider = match self.cells.last() {
            Some(cell) => cell.row_id,
            None => anyhow::bail!("split didn't leave left child with any cells"),
        };
        if divider >= right[0].row_id {
            anyhow::bail!(
                "wtf the last row_id of left leaf page is greater than first row_id of right leaf page"
            )
        }

        let usable = |pn: PageNumber| -> usize {
            page_size
                .checked_sub(if pn == 1 { MAZE_HEADER_SIZE } else { 0 })
                .ok_or_else(|| anyhow::anyhow!("what the actual fuck"))
                .unwrap()
        };
        let lp_size = |cells: &[TableLeafCell]| -> usize {
            PageType::TableLeaf.header_size()
                + 2 * cells.len()
                + cells.iter().map(|cell| cell.encoded_len()).sum::<usize>()
        };

        let total_len = self.cells.len() + right.len();
        for _ in 0..total_len {
            let left_overflowed = usable(self_pn) < lp_size(&self.cells);
            let right_overflowed = usable(right_pn) < lp_size(&right);

            if !left_overflowed && !right_overflowed {
                break;
            } else if left_overflowed {
                if split_idx == 1 {
                    anyhow::bail!("overflow support needed")
                }
                split_idx = split_idx
                    .checked_sub(1)
                    .ok_or_else(|| anyhow::anyhow!("split_idx went negative bro pack it up"))?
            } else if right_overflowed {
                split_idx += 1;
                if split_idx == total_len {
                    anyhow::bail!("overflow support needed")
                }
            }

            self.cells.extend(right);
            split_idx = split_idx.clamp(1, self.cells.len() - 1);
            right = self.cells.split_off(split_idx);
            divider = match self.cells.last() {
                Some(cell) => cell.row_id,
                None => anyhow::bail!("split didn't leave left child with any cells"),
            };
            if divider >= right[0].row_id {
                anyhow::bail!(
                    "wtf the last row_id of left leaf page is greater than first row_id of right leaf page"
                )
            }
        }

        let left_overflowed = usable(self_pn) < lp_size(&self.cells);
        let right_overflowed = usable(right_pn) < lp_size(&right);
        if left_overflowed || right_overflowed {
            anyhow::bail!("overflow support needed")
        }

        Ok((TableLeafPage::new(right), divider))
    }
}

impl TryFrom<&[u8]> for TableLeafPage {
    type Error = anyhow::Error;

    fn try_from(buf: &[u8]) -> anyhow::Result<Self> {
        let header = PageHeader::parse(buf, PageType::TableLeaf)?;
        let cells = parse_cells(buf, header, PageType::TableLeaf)?;

        Ok(Self { cells })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableLeafCell {
    pub row_id: u64, // varint encoded on disk
    pub payload: Vec<u8>,
}

impl TableLeafCell {
    pub fn new(row_id: u64, payload: &[u8]) -> Self {
        Self {
            row_id,
            payload: payload.to_vec(),
        }
    }
}

impl Cell for TableLeafCell {
    fn serialize(&self) -> Vec<u8> {
        let mut bytes = varint::encode(self.payload.len() as u64);
        bytes.extend(varint::encode(self.row_id));
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    fn encoded_len(&self) -> usize {
        varint::encode(self.payload.len() as u64).len()
            + varint::encode(self.row_id).len()
            + self.payload.len()
    }

    fn sort_key(&self) -> u64 {
        self.row_id
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
        header::MAZE_HEADER_SIZE,
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
        let base = MAZE_HEADER_SIZE; // page 1: leaf starts right after the file header
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

        let page_number = PageNumber::try_from(1).unwrap();
        let page = Page::parse(&buf, page_number).unwrap();
        if let Page::TableLeaf(leaf) = page {
            assert_eq!(leaf.cells.len(), rows.len());
            for (cell, (row_id, expected)) in leaf.cells.iter().zip(&rows) {
                assert_eq!(cell.row_id, *row_id);
                assert_eq!(&Record::decode(&cell.payload).unwrap(), expected);
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

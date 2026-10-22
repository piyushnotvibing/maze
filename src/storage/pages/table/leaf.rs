use crate::storage::{
    pages::{
        PageType,
        btree_page::{BTreePage, SplitKind, best_split},
        bytes::read_word_u32_be,
        cell::Cell,
        spec::PageSpec,
        types::{PageNumber, RowID},
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

    pub fn contains(&self, row_id: RowID) -> bool {
        self.cells
            .binary_search_by_key(&row_id, |cell| cell.row_id)
            .is_ok()
    }

    pub fn delete(&mut self, row_id: RowID) -> anyhow::Result<TableLeafCell> {
        let delete_idx = self.cells.binary_search_by_key(&row_id, |cell| cell.row_id);
        if delete_idx.is_err() {
            anyhow::bail!("cannot delete row_id {row_id} that does not exist in the tree")
        }
        let delete_idx = delete_idx.unwrap();
        let deleted = self.cells.remove(delete_idx);

        Ok(deleted)
    }
}

impl BTreePage for TableLeafPage {
    type C = TableLeafCell;
    type Key = <Self::C as Cell>::Key;

    const PAGE_TYPE: PageType = PageType::TableLeaf;

    fn from_parts(
        cells: Vec<Self::C>,
        rightmost_child: Option<PageNumber>,
    ) -> anyhow::Result<Self> {
        if rightmost_child.is_some() {
            anyhow::bail!("table leaf page cannot have a rightmost child")
        }

        Ok(Self { cells })
    }

    fn cells(&self) -> &[TableLeafCell] {
        &self.cells
    }

    fn cells_mut(&mut self) -> &mut Vec<Self::C> {
        &mut self.cells
    }

    /// Consumes the page, returns the left page, right page and divider `row_id`.
    fn split_with(
        mut self,
        new_cell: Self::C,
        left: PageSpec,
        right: PageSpec,
    ) -> anyhow::Result<(Self, Self, RowID)> {
        self.insert_unchecked(new_cell)?;

        let split_idx = best_split(
            &self.cell_costs()?,
            SplitKind::LeafSplit,
            Self::PAGE_TYPE.header_size(),
            left.usable()?,
            right.usable()?,
        )
        .ok_or_else(|| {
            anyhow::anyhow!("no two-way split can hold this cell: need overflow support")
        })?;

        let right_lp_cells = self.cells.split_off(split_idx);
        let divider = self.cells[split_idx - 1].row_id;

        Ok((
            self,
            Self {
                cells: right_lp_cells,
            },
            divider,
        ))
    }

    fn rightmost_child(&self) -> Option<PageNumber> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableLeafCell {
    pub row_id: RowID, // varint encoded on disk
    pub local_payload: Vec<u8>,
    pub total_payload_len: usize, // full logical length
    pub overflow_pn: Option<PageNumber>,
}

impl TableLeafCell {
    /// used when there's no overflow
    pub fn new(row_id: RowID, payload: &[u8]) -> Self {
        Self {
            row_id,
            local_payload: payload.to_vec(),
            total_payload_len: payload.len(),
            overflow_pn: None,
        }
    }

    pub fn with_overflow(
        row_id: RowID,
        local_payload: Vec<u8>,
        total_payload_len: usize,
        overflow_pn: Option<PageNumber>,
    ) -> Self {
        Self {
            row_id,
            local_payload,
            total_payload_len,
            overflow_pn,
        }
    }
}

impl Cell for TableLeafCell {
    type Key = RowID;

    // only reason its anyhow::Result<Vec<u8>> is because indexes need it, no need here, always returns Ok().
    fn serialize(&self) -> anyhow::Result<Vec<u8>> {
        let mut bytes = varint::encode(self.total_payload_len as u64);
        bytes.extend(varint::encode(self.row_id.get()));
        bytes.extend_from_slice(&self.local_payload);
        if let Some(n) = self.overflow_pn {
            bytes.extend(n.get().to_be_bytes());
        }
        Ok(bytes)
    }

    fn encoded_len(&self) -> anyhow::Result<usize> {
        Ok(varint::encode(self.total_payload_len as u64).len()
            + varint::encode(self.row_id.get()).len()
            + self.local_payload.len()
            + if self.overflow_pn.is_some() { 4 } else { 0 }) // overflow page number = u32 = 4B, only when present
    }

    fn sort_key(&self) -> RowID {
        self.row_id
    }

    /// ps to be used when overflow pages will be supported.
    fn parse(buf: &[u8], ps: PageSpec) -> anyhow::Result<Self> {
        let cap = ps.max_local_payload_len()?;
        let (total_payload_len, len_bytes_read) =
            varint::decode(buf).ok_or(anyhow::anyhow!("invalid cell size varint"))?;
        let (row_id, rowid_bytes_read) = varint::decode(&buf[len_bytes_read..])
            .ok_or_else(|| anyhow::anyhow!("invalid row_id varint"))?;

        let local_len = total_payload_len.min(cap as u64);

        let start = len_bytes_read + rowid_bytes_read;
        let end = usize::try_from(local_len)
            .ok()
            .and_then(|s| start.checked_add(s))
            .ok_or_else(|| anyhow::anyhow!("payload size out of range"))?;
        let local_payload = buf
            .get(start..end)
            .ok_or_else(|| anyhow::anyhow!("payload extends past buffer"))?
            .to_vec();

        let overflow_pn = if local_len < total_payload_len {
            Some(PageNumber::try_from(read_word_u32_be(
                buf,
                start + (local_len as usize),
            )?)?)
        } else {
            None
        };

        let total_payload_len = total_payload_len as usize;
        let row_id = RowID::try_from(row_id)?;

        Ok(Self {
            row_id,
            local_payload,
            total_payload_len,
            overflow_pn,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{
        header::MAZE_HEADER_SIZE,
        pages::{Page, PageNumber, types::RowID},
        record::{Record, values::Value},
    };

    // Build one cell: varint(payload len) | varint(row_id) | payload
    fn cell_bytes(row_id: RowID, payload: &[u8]) -> Vec<u8> {
        let mut c = varint::encode(payload.len() as u64);
        c.extend(varint::encode(row_id.get()));
        c.extend_from_slice(payload);
        c
    }

    #[test]
    fn table_leaf_cell_roundtrip() {
        let row_id = RowID::try_from(300).unwrap();
        let cell = TableLeafCell {
            row_id,
            local_payload: b"abc".to_vec(),
            total_payload_len: 3,
            overflow_pn: None,
        };
        let ps = PageSpec::fresh(512);
        let parsed = TableLeafCell::parse(cell.serialize().unwrap().as_slice(), ps).unwrap();
        assert_eq!(
            (parsed.row_id.get(), parsed.local_payload),
            (300, b"abc".to_vec())
        );
    }

    #[test]
    fn payload_decodes_into_a_value_array_correctly() {
        // big enough that row 300's ~212-byte encoded record stays under the local-payload
        // cap (page_size / 4), so this hand-rolled, overflow-unaware page layout stays valid.
        const PAGE_SIZE: usize = 1024;
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
                cell_bytes(RowID::try_from(*id).unwrap(), &payload)
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
        let ps = PageSpec::new(PAGE_SIZE, page_number);
        let page = Page::parse(&buf, ps).unwrap();
        if let Page::TableLeaf(leaf) = page {
            assert_eq!(leaf.cells.len(), rows.len());
            for (cell, (row_id, expected)) in leaf.cells.iter().zip(&rows) {
                assert_eq!(cell.row_id, *row_id);
                let (got, _) = Record::decode(&cell.local_payload).unwrap();
                assert_eq!(&got, expected);
            }
        } else {
            panic!("invalid page type")
        }
    }

    #[test]
    fn empty_leaf_page_is_valid() {
        let ps = PageSpec::fresh(512);
        let mut buf = vec![0u8; 512];
        buf[0] = PageType::TableLeaf.into();
        buf[3..5].copy_from_slice(&512u16.to_be_bytes()); // cell area starts at page end
        let leaf = TableLeafPage::parse(buf.as_slice(), ps).unwrap();
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

        let ps = PageSpec::fresh(PAGE_SIZE);
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
        let inner = TableLeafCell::parse(&buf[b_ptr as usize..], ps).unwrap();
        assert_eq!(inner.row_id, 2);
        assert_eq!(inner.local_payload, b_payload);

        assert!(TableLeafPage::parse(buf.as_slice(), ps).is_err());
    }

    #[test]
    fn split_with_a_big_cell_at_every_position_always_succeeds_via_overflow() {
        // Before overflow pages existed, big_id 29/31 (14 or 15 small cells forced onto the
        // big cell's side) made every two-way split infeasible. Overflow caps the big cell's
        // on-disk cost to the local cap (512 / 4 = 128 bytes) + a 4-byte pointer, which a binary
        // split can always accommodate regardless of position, so every position now succeeds.
        let left_spec = PageSpec::new(512, PageNumber::try_from(1).unwrap());
        let right_spec = PageSpec::fresh(512);
        let cap = left_spec.max_local_payload_len().unwrap();
        assert_eq!(cap, right_spec.max_local_payload_len().unwrap());

        for big_id in (1u64..=61).step_by(2) {
            // odd ids: before, between and after the 30 small rows
            let page = TableLeafPage::new(
                (1u64..=30)
                    .map(|i| TableLeafCell::new(RowID::try_from(i * 2).unwrap(), &[i as u8; 10]))
                    .collect(),
            );
            let full_payload = vec![0xABu8; 300];
            let big = TableLeafCell::with_overflow(
                RowID::try_from(big_id).unwrap(),
                full_payload[..cap].to_vec(),
                full_payload.len(),
                Some(PageNumber::try_from(999).unwrap()),
            );
            let result = page.clone().split_with(big.clone(), left_spec, right_spec);

            let (left, right, divider) = result.unwrap_or_else(|e| {
                panic!("big_id {big_id} should always find a two-way split now: {e}")
            });
            left.serialize(left_spec).unwrap(); // both halves must really fit
            right.serialize(right_spec).unwrap();
            assert_eq!(left.cells.last().unwrap().row_id, divider);
            assert!(right.cells[0].row_id > divider);

            let mut want = page.cells.clone();
            want.push(big);
            want.sort_by_key(|c| c.row_id);
            let got: Vec<_> = left.cells.iter().chain(&right.cells).cloned().collect();
            assert_eq!(got, want); // nothing lost, nothing duplicated
        }
    }
}

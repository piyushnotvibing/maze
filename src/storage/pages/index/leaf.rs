use crate::storage::{
    pages::{
        PageType,
        btree_page::{BTreePage, SplitKind, best_split},
        cell::Cell,
        spec::PageSpec,
        types::{PageNumber, RowID},
    },
    record::{Record, values::Value},
    varint,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexLeafPage {
    pub cells: Vec<IndexLeafCell>,
}

impl IndexLeafPage {
    pub fn new(cells: Vec<IndexLeafCell>) -> Self {
        Self { cells }
    }

    /// Mirrors `TableLeafPage::contains`.
    pub fn contains(&self, values: &[Value], row_id: RowID) -> bool {
        self.cells
            .binary_search_by(|c| c.values.as_slice().cmp(values).then(c.row_id.cmp(&row_id)))
            .is_ok()
    }

    /// Mirrors `TableLeafPage::delete`.
    pub fn delete(&mut self, values: &[Value], row_id: RowID) -> anyhow::Result<IndexLeafCell> {
        let idx = self
            .cells
            .binary_search_by(|c| c.values.as_slice().cmp(values).then(c.row_id.cmp(&row_id)))
            .map_err(|_| {
                anyhow::anyhow!("cannot delete (values, row_id) that does not exist in the tree")
            })?;
        Ok(self.cells.remove(idx))
    }
}

impl BTreePage for IndexLeafPage {
    type C = IndexLeafCell;

    type Key = <Self::C as Cell>::Key;

    const PAGE_TYPE: PageType = PageType::IndexLeaf;

    fn from_parts(
        cells: Vec<Self::C>,
        rightmost_child: Option<PageNumber>,
    ) -> anyhow::Result<Self> {
        if rightmost_child.is_some() {
            anyhow::bail!("index leaf page cannot have a rightmost child")
        }

        Ok(Self { cells })
    }

    fn cells(&self) -> &[Self::C] {
        &self.cells
    }

    fn cells_mut(&mut self) -> &mut Vec<Self::C> {
        &mut self.cells
    }

    fn split_with(
        mut self,
        new_cell: Self::C,
        left: PageSpec,
        right: PageSpec,
    ) -> anyhow::Result<(Self, Self, Self::Key)> {
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
        let divider = (
            self.cells[split_idx - 1].values.clone(),
            self.cells[split_idx - 1].row_id,
        );

        Ok((
            self,
            Self {
                cells: right_lp_cells,
            },
            divider,
        ))
    }

    fn rightmost_child(&self) -> Option<crate::storage::pages::types::PageNumber> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexLeafCell {
    pub values: Vec<Value>,
    // stored on disk varint encoded
    pub row_id: RowID,
}

impl Cell for IndexLeafCell {
    type Key = (Vec<Value>, RowID);

    fn serialize(&self) -> anyhow::Result<Vec<u8>> {
        let mut bytes = Record::new(self.values.clone())?.encode();
        bytes.extend(varint::encode(self.row_id.get()));

        Ok(bytes)
    }

    fn encoded_len(&self) -> anyhow::Result<usize> {
        Ok(Record::new(self.values.clone())?.encode().len()
            + varint::encode(self.row_id.get()).len())
    }

    fn sort_key(&self) -> Self::Key {
        (self.values.clone(), self.row_id)
    }

    fn parse(buf: &[u8], ps: PageSpec) -> anyhow::Result<Self> {
        if ps.first {
            anyhow::bail!("first page of database cannot be an index leaf page")
        }

        let (values, bytes_read) = Record::decode(buf)?;
        if bytes_read >= buf.len() {
            anyhow::bail!("corrupted index leaf page buffer")
        }

        let row_id = varint::decode(
            buf.get(bytes_read..)
                .ok_or_else(|| anyhow::anyhow!("this error should never occur"))?,
        );
        if row_id.is_none() {
            anyhow::bail!("corrupted index leaf page buffer, no row_id")
        }
        let row_id = RowID::try_from(row_id.unwrap().0)?;
        Ok(Self { values, row_id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::pages::{CELL_PTR_SIZE, Page, types::PageNumber};

    fn rid(n: u64) -> RowID {
        RowID::try_from(n).unwrap()
    }

    fn cell(values: Vec<Value>, row_id: u64) -> IndexLeafCell {
        IndexLeafCell {
            values,
            row_id: rid(row_id),
        }
    }

    #[test]
    fn roundtrips_a_single_column_key() {
        let c = cell(vec![Value::Int(42)], 7);
        let ps = PageSpec::fresh(512);
        let parsed = IndexLeafCell::parse(&c.serialize().unwrap(), ps).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn roundtrips_a_composite_key() {
        let c = cell(
            vec![Value::Text("Smith".into()), Value::Text("Jane".into())],
            12,
        );
        let ps = PageSpec::fresh(512);
        let parsed = IndexLeafCell::parse(&c.serialize().unwrap(), ps).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn roundtrips_null_and_blob_columns() {
        let c = cell(vec![Value::Null, Value::Blob(vec![1, 2, 3])], 5);
        let ps = PageSpec::fresh(512);
        let parsed = IndexLeafCell::parse(&c.serialize().unwrap(), ps).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn rejects_cell_at_page_one() {
        let c = cell(vec![Value::Int(1)], 1);
        let ps = PageSpec::new(512, PageNumber::try_from(1).unwrap());
        assert!(IndexLeafCell::parse(&c.serialize().unwrap(), ps).is_err());
    }

    #[test]
    fn empty_leaf_page_at_page_one_is_rejected() {
        let ps = PageSpec::new(512, PageNumber::try_from(1).unwrap());
        assert!(
            Page::IndexLeaf(IndexLeafPage { cells: vec![] })
                .serialize(ps)
                .is_err(),
            "an empty index leaf page must still be rejected at page 1"
        );
    }

    #[test]
    fn page_round_trips_through_serialize_and_parse() {
        let page = IndexLeafPage {
            cells: vec![
                cell(vec![Value::Int(1)], 1),
                cell(vec![Value::Int(2)], 2),
                cell(vec![Value::Text("mid".into())], 3),
            ],
        };
        let ps = PageSpec::fresh(512);
        let bytes = Page::IndexLeaf(page.clone()).serialize(ps).unwrap();
        match Page::parse(&bytes, ps).unwrap() {
            Page::IndexLeaf(parsed) => assert_eq!(parsed, page),
            _ => panic!("expected an index leaf page back"),
        }
    }

    #[test]
    fn split_with_divides_cells_and_promotes_correct_divider() {
        let page = IndexLeafPage {
            cells: (1..=4)
                .map(|i| cell(vec![Value::Int(i * 10)], i as u64))
                .collect(),
        };
        let new_cell = cell(vec![Value::Int(25)], 99);

        // Size left/right specs from the real encoded costs so the split is
        // forced regardless of how big a serialized cell happens to be.
        let header = PageType::IndexLeaf.header_size();
        let mut costs = page.cell_costs().unwrap();
        costs.push(CELL_PTR_SIZE + new_cell.encoded_len().unwrap());
        let total: usize = costs.iter().sum();
        let cap = header + total / 2 + costs.iter().max().copied().unwrap();
        let left_spec = PageSpec::fresh(cap);
        let right_spec = PageSpec::fresh(cap);

        let (left, right, divider) = page
            .clone()
            .split_with(new_cell.clone(), left_spec, right_spec)
            .unwrap();
        left.serialize(left_spec).unwrap();
        right.serialize(right_spec).unwrap();

        let mut want = page.cells.clone();
        want.push(new_cell);
        want.sort_by_key(|c| c.sort_key());
        let got: Vec<_> = left.cells.iter().chain(&right.cells).cloned().collect();
        assert_eq!(got, want);
        assert_eq!(left.cells.last().unwrap().sort_key(), divider);
        assert!(right.cells[0].sort_key() > divider);
    }
}

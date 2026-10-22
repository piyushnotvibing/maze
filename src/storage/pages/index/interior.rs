use std::cmp::Ordering;

use crate::storage::{
    pages::{
        PageType,
        btree_page::{BTreePage, SplitKind, best_split},
        bytes::read_word_u32_be,
        cell::{Cell, InteriorCell},
        spec::PageSpec,
        types::{PageNumber, RowID},
    },
    record::{Record, values::Value},
    varint,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInteriorPage {
    pub cells: Vec<IndexInteriorCell>,
    pub rightmost_child: PageNumber,
}

impl IndexInteriorPage {
    /// Mirrors `TableInteriorPage::child_of`: the child whose subtree may
    /// contain `(values, row_id)`, comparing the full composite key.
    pub fn child_of(&self, values: &[Value], row_id: RowID) -> PageNumber {
        let next_cell_idx = self.cells.partition_point(|cell| {
            cell.values
                .as_slice()
                .cmp(values)
                .then(cell.row_id.cmp(&row_id))
                == Ordering::Less
        });
        match self.cells.get(next_cell_idx) {
            Some(cell) => cell.left_child,
            None => self.rightmost_child,
        }
    }

    /// Mirrors `TableInteriorPage::delete_child`.
    pub fn delete_child(&mut self, child: PageNumber) -> anyhow::Result<()> {
        let child_idx = self.cells.iter().position(|cell| cell.left_child == child);
        match child_idx {
            Some(idx) => {
                self.cells.remove(idx);
            }
            None if self.rightmost_child == child => {
                let last_cell = self.cells.pop().unwrap();
                self.rightmost_child = last_cell.left_child;
            }
            None => anyhow::bail!("interior page does not contain child page {child}"),
        }

        Ok(())
    }
}

impl BTreePage for IndexInteriorPage {
    type C = IndexInteriorCell;
    type Key = <Self::C as Cell>::Key;
    const PAGE_TYPE: PageType = PageType::IndexInterior;

    fn from_parts(
        cells: Vec<Self::C>,
        rightmost_child: Option<PageNumber>,
    ) -> anyhow::Result<Self> {
        let rightmost_child = rightmost_child
            .ok_or_else(|| anyhow::anyhow!("index interior page has no rightmost child"))?;

        Ok(Self {
            cells,
            rightmost_child,
        })
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

        let s = best_split(
            &self.cell_costs()?,
            SplitKind::InteriorSplit,
            Self::PAGE_TYPE.header_size(),
            left.usable()?,
            right.usable()?,
        )
        .ok_or_else(|| anyhow::anyhow!("interior page too small to split"))?;

        let mut right_cells = self.cells.split_off(s);
        let promoted = right_cells.remove(0); // leaves both halves
        let right_page = Self {
            cells: right_cells,
            rightmost_child: self.rightmost_child,
        };
        self.rightmost_child = promoted.left_child; // promoted cell's subtree ends the left half
        Ok((self, right_page, (promoted.values, promoted.row_id)))
    }

    fn rightmost_child(&self) -> Option<PageNumber> {
        Some(self.rightmost_child)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInteriorCell {
    pub left_child: PageNumber,
    pub values: Vec<Value>,
    // stored on disk varint encoded
    pub row_id: RowID,
}

impl Cell for IndexInteriorCell {
    type Key = (Vec<Value>, RowID);

    fn serialize(&self) -> anyhow::Result<Vec<u8>> {
        let mut bytes = self.left_child.get().to_be_bytes().to_vec();
        bytes.extend(Record::new(self.values.clone())?.encode());
        bytes.extend(varint::encode(self.row_id.get()));

        Ok(bytes)
    }

    fn encoded_len(&self) -> anyhow::Result<usize> {
        Ok(std::mem::size_of::<u32>()
            + Record::new(self.values.clone())?.encode().len()
            + varint::encode(self.row_id.get()).len())
    }

    fn sort_key(&self) -> Self::Key {
        (self.values.clone(), self.row_id)
    }

    fn parse(buf: &[u8], ps: PageSpec) -> anyhow::Result<Self> {
        if ps.first {
            anyhow::bail!("first page of database cannot be an index interior page")
        }

        let left_child = PageNumber::try_from(read_word_u32_be(buf, 0)?)?;

        let (values, bytes_read) = Record::decode(&buf[4..])?;
        if 4 + bytes_read >= buf.len() {
            anyhow::bail!("corrupted index interior page buffer")
        }

        let row_id = varint::decode(
            buf.get(4 + bytes_read..)
                .ok_or_else(|| anyhow::anyhow!("this error should never occur"))?,
        );
        if row_id.is_none() {
            anyhow::bail!("corrupted index interior page buffer, no row_id")
        }
        let row_id = RowID::try_from(row_id.unwrap().0)?;
        Ok(Self {
            left_child,
            values,
            row_id,
        })
    }
}

impl InteriorCell for IndexInteriorCell {
    fn left_child(&self) -> PageNumber {
        self.left_child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::pages::{CELL_PTR_SIZE, Page};

    fn rid(n: u64) -> RowID {
        RowID::try_from(n).unwrap()
    }

    fn cell(left_child: u32, values: Vec<Value>, row_id: u64) -> IndexInteriorCell {
        IndexInteriorCell {
            left_child: PageNumber::try_from(left_child).unwrap(),
            values,
            row_id: rid(row_id),
        }
    }

    #[test]
    fn roundtrips_a_single_column_key() {
        let c = cell(3, vec![Value::Int(42)], 7);
        let ps = PageSpec::fresh(512);
        let parsed = IndexInteriorCell::parse(&c.serialize().unwrap(), ps).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn roundtrips_a_composite_key() {
        let c = cell(
            4,
            vec![Value::Text("Smith".into()), Value::Text("Jane".into())],
            12,
        );
        let ps = PageSpec::fresh(512);
        let parsed = IndexInteriorCell::parse(&c.serialize().unwrap(), ps).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn rejects_cell_at_page_one() {
        let c = cell(3, vec![Value::Int(1)], 1);
        let ps = PageSpec::new(512, PageNumber::try_from(1).unwrap());
        assert!(IndexInteriorCell::parse(&c.serialize().unwrap(), ps).is_err());
    }

    #[test]
    fn empty_interior_page_at_page_one_is_rejected() {
        let ps = PageSpec::new(512, PageNumber::try_from(1).unwrap());
        assert!(
            Page::IndexInterior(IndexInteriorPage {
                cells: vec![],
                rightmost_child: PageNumber::try_from(5).unwrap(),
            })
            .serialize(ps)
            .is_err(),
            "an empty index interior page must still be rejected at page 1"
        );
    }

    // Not parse-dependent: split_with only rearranges already-in-memory
    // cells, so it's unaffected by the parse bug above.
    #[test]
    fn split_with_divides_cells_and_promotes_correct_divider() {
        let page = IndexInteriorPage {
            cells: (1..=4)
                .map(|i| cell(i as u32, vec![Value::Int(i * 10)], i as u64))
                .collect(),
            rightmost_child: PageNumber::try_from(5).unwrap(),
        };
        let new_cell = cell(9, vec![Value::Int(25)], 99);

        let header = PageType::IndexInterior.header_size();
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

        // The cell whose key equals the divider is "promoted": removed from both
        // halves entirely, its left_child becomes left's new rightmost_child, and
        // its key is what's returned as the divider (mirrors TableInteriorPage).
        let promoted_idx = want.iter().position(|c| c.sort_key() == divider).unwrap();
        let promoted = want.remove(promoted_idx);

        let got: Vec<_> = left.cells.iter().chain(&right.cells).cloned().collect();
        assert_eq!(got, want);
        assert_eq!(left.rightmost_child, promoted.left_child);
    }
}

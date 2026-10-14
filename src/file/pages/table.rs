use crate::file::pages::{
    CELL_PTR_SIZE, PageType,
    cell::{Cell, parse_cells},
    header::PageHeader,
    spec::PageSpec,
    types::PageNumber,
};

pub(crate) trait TablePage: Sized {
    type C: Cell;
    const PAGE_TYPE: PageType;

    fn from_parts(cells: Vec<Self::C>, rightmost_child: Option<PageNumber>)
    -> anyhow::Result<Self>;

    fn cells(&self) -> &[Self::C];

    fn cells_mut(&mut self) -> &mut Vec<Self::C>;

    /// Consumes the page (`self`) returns (left page, right page, divider)
    fn split_with(
        self,
        new_cell: Self::C,
        left: PageSpec,
        right: PageSpec,
    ) -> anyhow::Result<(Self, Self, u64)>;

    fn cell_costs(&self) -> Vec<usize> {
        self.cells()
            .iter()
            .map(|cell| CELL_PTR_SIZE + cell.encoded_len())
            .collect::<Vec<usize>>()
    }

    fn rightmost_child(&self) -> Option<PageNumber>;

    fn used_bytes(&self) -> usize {
        Self::PAGE_TYPE.header_size() + self.cell_costs().iter().sum::<usize>()
    }

    fn fits(&self, new_cell: &Self::C, ps: PageSpec) -> anyhow::Result<bool> {
        Ok(self.used_bytes() + CELL_PTR_SIZE + new_cell.encoded_len() <= ps.usable()?)
    }

    /// Sorted insert with the duplicate check, but no size check (splits may overflow transiently)
    fn insert_unchecked(&mut self, cell: Self::C) -> anyhow::Result<()> {
        let key = cell.sort_key();
        let idx = self.cells().partition_point(|c| c.sort_key() < key);
        if self.cells().get(idx).is_some_and(|c| c.sort_key() == key) {
            anyhow::bail!("key {key} already exists");
        }
        self.cells_mut().insert(idx, cell);
        Ok(())
    }

    fn insert(&mut self, cell: Self::C, spec: PageSpec) -> anyhow::Result<()> {
        if !self.fits(&cell, spec)? {
            anyhow::bail!("cell does not fit in this page");
        }
        self.insert_unchecked(cell)
    }

    fn serialize(&self, spec: PageSpec) -> anyhow::Result<Vec<u8>> {
        if self
            .cells()
            .windows(2)
            .any(|w| w[0].sort_key() >= w[1].sort_key())
        {
            anyhow::bail!("cells not in strictly increasing key order");
        }
        Self::PAGE_TYPE.serialize(self.cells(), self.rightmost_child(), spec)
    }

    fn parse(buf: &[u8], ps: PageSpec) -> anyhow::Result<Self> {
        let header = PageHeader::parse(buf, Self::PAGE_TYPE)?;
        let cells = parse_cells::<Self::C>(buf, header, Self::PAGE_TYPE, ps)?;
        Self::from_parts(cells, header.rightmost_child)
    }
}

/// Costs are pointer + cell per cell. Returns the split index `s` that fits both halves with
/// the least imbalance. promote_median: cell `s` leaves both halves (interior); otherwise it
/// starts the right half (leaf).
pub(crate) fn best_split(
    costs: &[usize],
    // Only true for interior table pages.
    split_for: SplitKind,
    header: usize,
    left_cap: usize,
    right_cap: usize,
) -> Option<usize> {
    let (n, total) = (costs.len(), costs.iter().sum::<usize>());
    let mut left = 0usize;
    let mut best: Option<(usize, usize)> = None;
    for split_idx in 1..n {
        left += costs[split_idx - 1];

        // right half would be empty
        if split_for == SplitKind::TableInteriorSplit && split_idx + 1 >= n {
            break;
        }

        let right = total
            - left
            - if split_for == SplitKind::TableInteriorSplit {
                costs[split_idx]
            } else {
                0
            };
        if header + left <= left_cap && header + right <= right_cap {
            let imbalance = left.abs_diff(right);
            if best.is_none_or(|(b, _)| imbalance < b) {
                best = Some((imbalance, split_idx));
            }
        }
    }
    best.map(|(_, split_idx)| split_idx)
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SplitKind {
    TableLeafSplit,
    TableInteriorSplit,
}

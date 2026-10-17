use crate::file::{header::MAZE_HEADER_SIZE, pages::types::PageNumber};

const LOCAL_PAYLOAD_THRESHOLD_RATIO: usize = 4;

#[derive(Copy, Clone)]
pub struct PageSpec {
    pub page_size: usize,
    pub first: bool,
}

impl PageSpec {
    pub fn new(page_size: usize, pn: PageNumber) -> Self {
        Self {
            page_size,
            first: pn == 1,
        }
    }

    pub fn fresh(page_size: usize) -> Self {
        Self {
            page_size,
            first: false,
        }
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn base_offset(&self) -> usize {
        if self.first { MAZE_HEADER_SIZE } else { 0 }
    }

    pub fn usable(&self) -> anyhow::Result<usize> {
        self.page_size.checked_sub(self.base_offset()).ok_or_else(|| anyhow::anyhow!("wow this is a crazy error at this stage, how is page size less than maze header size"))
    }

    /// the reason this exists is because when a leaf page overflows, we do a strict binary split
    /// sqlite does not do that, it takes 3 sibling leaf pages into account and shuffles the cells
    /// among them to fit everything. that's why it can afford to put max_local_payload_len as
    /// page_size - <some number>. we can't do this because if we did, the binary split would fail
    /// on a single cell leaf page as neither page post-split would be able to hold the cell.
    /// therefore we have to set a max local payload cap for a cell so that binary split works.
    ///
    /// deliberately based on `page_size`, not `usable()`: a cell decides local-vs-overflow once,
    /// at insert time, but a later split can move it from page 1 onto a brand-new (always
    /// non-first) page. if the cap depended on `first`, a cell built under page 1's smaller cap
    /// could be re-parsed later under a non-first page's larger cap, misreading part of the
    /// overflow pointer as payload bytes. `page_size` is the same on every page, so the cap a
    /// cell was written with always matches the cap it's read back with, no matter which page it
    /// ends up on.
    pub fn max_local_payload_len(&self) -> anyhow::Result<usize> {
        Ok(self.page_size / LOCAL_PAYLOAD_THRESHOLD_RATIO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_local_payload_len_is_the_same_on_page_one_and_everywhere_else() {
        // Regression: this cap used to be `usable() / 4`, which differs between page 1
        // (page_size - MAZE_HEADER_SIZE) and every other page (page_size). A cell's
        // local/overflow split is decided once at insert time but can later be relocated by a
        // split onto a different page, so the cap must be identical everywhere a cell could end
        // up, or a payload length in the old gap between the two caps would be written under
        // one cap and misparsed under the other.
        for page_size in [512usize, 1024, 4096, 65536] {
            let first = PageSpec::new(page_size, PageNumber::try_from(1).unwrap());
            let non_first = PageSpec::fresh(page_size);
            assert_eq!(
                first.max_local_payload_len().unwrap(),
                non_first.max_local_payload_len().unwrap(),
                "page_size {page_size}"
            );
        }
    }
}

use crate::file::{header::MAZE_HEADER_SIZE, pages::types::PageNumber};

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
}

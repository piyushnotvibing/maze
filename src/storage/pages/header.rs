use crate::storage::pages::{
    PageType,
    bytes::{read_word_u16_be, read_word_u32_be},
    types::PageNumber,
};

const PAGE_CELL_COUNT_OFFSET: usize = 1;
const PAGE_CELL_CONTENT_OFFSET: usize = 3;
const PAGE_RIGHTMOST_CHILD_OFFSET: usize = 5;

/*
    PageHeader size:
        Leaf Node = 1 + 2 + 2 + 0 = 5
        Interior = 1 + 2 + 2 + 4 = 9
*/
// Internal struct, only holds metadata for as long as i need it.
#[derive(Debug, Copy, Clone)]
pub(crate) struct PageHeader {
    pub(crate) page_type: PageType,
    pub(crate) cell_count: u16,
    pub(crate) cell_content_offset: u16, // 0 => 65536, else same
    pub(crate) rightmost_child: Option<PageNumber>, // Only interior page has this field set to Some().
}

impl PageHeader {
    pub(crate) fn parse(buf: &[u8], page_type: PageType) -> anyhow::Result<Self> {
        if buf.is_empty() {
            anyhow::bail!("buffer is empty")
        }

        if PageType::try_from(buf[0])? != page_type {
            anyhow::bail!("invalid page_type args")
        }

        let cell_count = read_word_u16_be(buf, PAGE_CELL_COUNT_OFFSET)?;
        let cell_content_offset = read_word_u16_be(buf, PAGE_CELL_CONTENT_OFFSET)?;
        let rightmost_child = match page_type {
            PageType::TableInterior => Some(PageNumber::try_from(read_word_u32_be(
                buf,
                PAGE_RIGHTMOST_CHILD_OFFSET,
            )?)?),
            PageType::IndexInterior => Some(PageNumber::try_from(read_word_u32_be(
                buf,
                PAGE_RIGHTMOST_CHILD_OFFSET,
            )?)?),
            PageType::TableLeaf => None,
            PageType::IndexLeaf => None,
        };

        Ok(Self {
            page_type,
            cell_count,
            cell_content_offset,
            rightmost_child,
        })
    }
}

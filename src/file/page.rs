use crate::{
    file::header::HEADER_SIZE,
    utils::{read_word_u16_be, read_word_u64_be},
};

#[derive(Debug, Clone)]
pub enum Page {
    TableLeaf(TableLeafPage),
}

#[derive(Debug, Clone)]
pub struct TableLeafPage {
    pub header: PageHeader,
    pub cell_ptrs: Vec<u16>,
    pub cells: Vec<TableLeafCell>,
}

/*
    PageHeader size:
        Leaf Node = 1 + 2 + 2 + 0 = 5
        Interior = 1 + 2 + 2 + 4 = 9
*/
#[derive(Debug, Copy, Clone)]
pub struct PageHeader {
    pub page_type: PageType,
    pub cell_count: u16,
    pub cell_content_offset: u16,     // 0 => 65536, else same
    pub rightmost_child: Option<u32>, // Only interior page has this field set to Some().
}

#[derive(Debug, Clone)]
pub struct TableLeafCell {
    pub size: u64,
    pub row_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Copy, Clone)]
pub enum PageType {
    TableLeaf,
    TableInterior,
}

impl Into<u8> for PageType {
    fn into(self) -> u8 {
        match self {
            PageType::TableLeaf =>  10,
            PageType::TableInterior => 5,
        }
    }
}

impl TryFrom<u8> for PageType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> anyhow::Result<Self> {
        match value {
            10 => Ok(PageType::TableLeaf),
            5 => Ok(PageType::TableInterior),
            _ => anyhow::bail!("invalid page type"),
        }
    }
}

const LEAF_PAGE_HEADER_SIZE: usize = 5;
const PAGE_CELL_COUNT_OFFSET: usize = 1;
const PAGE_CELL_CONTENT_OFFSET: usize = 3;
// const PAGE_RIGHTMOST_CHILD_PTR_OFFSET: usize = 5;

pub fn parse_page(buf: &[u8], page_number: usize) -> anyhow::Result<Page> {
    let is_first_page = page_number == 1;

    let page_type = match PageType::try_from(buf[0]) {
        Ok(pt) => pt,
        Err(e) => anyhow::bail!("{e}"),
    };

    match page_type {
        PageType::TableLeaf => parse_table_leaf_page(buf, is_first_page),
        PageType::TableInterior => todo!(),
    }
}

fn parse_table_leaf_page(buf: &[u8], is_first_page: bool) -> anyhow::Result<Page> {
    let leaf_data_offset = if is_first_page { HEADER_SIZE as usize } else { 0 };
    
    let header = parse_page_header(&buf[leaf_data_offset..])?;

    let content_buffer = &buf[LEAF_PAGE_HEADER_SIZE..];
    let cell_ptrs = parse_cell_ptrs(content_buffer, header.cell_count as usize, leaf_data_offset as u16)?;

    let cells = cell_ptrs
        .iter()
        .map(|&ptr| parse_table_leaf_cell(&buf[ptr as usize..]))
        .collect::<anyhow::Result<Vec<TableLeafCell>>>()?;

    Ok(Page::TableLeaf(TableLeafPage {
        header,
        cell_ptrs,
        cells,
    }))
}

fn parse_page_header(buf: &[u8]) -> anyhow::Result<PageHeader> {
    let page_type = match PageType::try_from(buf[0]) {
        Ok(pt) => pt,
        Err(e) => anyhow::bail!("{e}"),
    };

    let cell_count = read_word_u16_be(buf, PAGE_CELL_COUNT_OFFSET)?;
    let cell_content_offset = read_word_u16_be(buf, PAGE_CELL_CONTENT_OFFSET)?;
    let rightmost_child = None; // for now.

    Ok(PageHeader {
        page_type,
        cell_count,
        cell_content_offset,
        rightmost_child,
    })
}

fn parse_cell_ptrs(buf: &[u8], cell_count: usize, leaf_data_offset: u16) -> anyhow::Result<Vec<u16>> {
    let mut pointers = Vec::with_capacity(cell_count);

    for i in 0..cell_count {
        let bytes: u16 = read_word_u16_be(buf, 2 * i)
            .map_err(|_| anyhow::anyhow!("failed to parse cell pointers at index {i}"))?;
        let ptr = bytes.checked_sub(leaf_data_offset).ok_or_else(|| anyhow::anyhow!("leaf_data_offset > cell_count bytes"))?;
        pointers.push(ptr);
    }

    Ok(pointers)
}

fn parse_table_leaf_cell(buf: &[u8]) -> anyhow::Result<TableLeafCell> {
    let size = read_word_u64_be(buf, 0)?;
    let row_id = read_word_u64_be(buf, 8)?;
    let end = 16u64.checked_add(size).ok_or_else(|| anyhow::anyhow!("16 + size > u64 god bless this number"))?;
    let payload = buf[16..end as usize].to_vec();

    Ok(TableLeafCell {
        size,
        row_id,
        payload,
    })
}

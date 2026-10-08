use crate::{
    file::{
        header::HEADER_SIZE,
        record::{Record, Value},
        varint,
    },
    utils::{read_word_u16_be, read_word_u64_be},
};

#[derive(Debug, Clone)]
pub enum Page {
    TableLeaf(TableLeafPage),
}

impl Page {
    fn from(mut buf: &[u8], page_number: usize) -> anyhow::Result<Page> {
        let leaf_data_offset = if page_number == 1 { HEADER_SIZE } else { 0 };

        let page_type = match PageType::try_from(buf[leaf_data_offset]) {
            Ok(pt) => pt,
            Err(e) => anyhow::bail!("{e}"),
        };
        buf = &buf[leaf_data_offset..];

        match page_type {
            PageType::TableLeaf => parse_table_leaf_page(buf),
            PageType::TableInterior => todo!(),
        }
    }
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

impl TableLeafCell {
    fn from(buf: &[u8]) -> anyhow::Result<Self> {
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

        Ok(TableLeafCell {
            size,
            row_id,
            payload,
        })
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = varint::encode(self.size);
        bytes.extend(varint::encode(self.row_id));
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    fn decode_payload(&self) -> anyhow::Result<Vec<Value>> {
        let record = Record::build(&self.payload)?;
        Ok(record.body)
    }
}

#[derive(Debug, Copy, Clone)]
pub enum PageType {
    TableLeaf,
    TableInterior,
}

impl From<PageType> for u8 {
    fn from(value: PageType) -> Self {
        match value {
            PageType::TableLeaf => 10,
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

fn parse_table_leaf_page(buf: &[u8]) -> anyhow::Result<Page> {
    let header = parse_leaf_page_header(buf)?;

    let content_buffer = &buf[LEAF_PAGE_HEADER_SIZE..];
    let cell_ptrs = parse_cell_ptrs(content_buffer, header.cell_count as usize)?;

    let cells = cell_ptrs
        .iter()
        .map(|&ptr| TableLeafCell::from(&buf[ptr as usize..]))
        .collect::<anyhow::Result<Vec<TableLeafCell>>>()?;

    Ok(Page::TableLeaf(TableLeafPage {
        header,
        cell_ptrs,
        cells,
    }))
}

fn parse_leaf_page_header(buf: &[u8]) -> anyhow::Result<PageHeader> {
    let cell_count = read_word_u16_be(buf, PAGE_CELL_COUNT_OFFSET)?;
    let cell_content_offset = read_word_u16_be(buf, PAGE_CELL_CONTENT_OFFSET)?;
    let rightmost_child = None;

    Ok(PageHeader {
        page_type: PageType::TableLeaf,
        cell_count,
        cell_content_offset,
        rightmost_child,
    })
}

fn parse_cell_ptrs(buf: &[u8], cell_count: usize) -> anyhow::Result<Vec<u16>> {
    let mut pointers = Vec::with_capacity(cell_count);

    for i in 0..cell_count {
        let bytes = read_word_u16_be(buf, 2 * i)
            .map_err(|_| anyhow::anyhow!("failed to parse cell pointers at index {i}"))?;
        pointers.push(bytes);
    }

    Ok(pointers)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build one cell: varint(payload len) | varint(row_id) | payload
    fn cell_bytes(row_id: u64, payload: &[u8]) -> Vec<u8> {
        let mut c = varint::encode(payload.len() as u64);
        c.extend(varint::encode(row_id));
        c.extend_from_slice(payload);
        c
    }

    #[test]
    fn page_parses_correctly() {
        const PAGE_SIZE: usize = 512;
        // Page 1: the leaf page starts right after the file header, and (as in your
        // original test) pointers and the content offset are relative to that start.
        let base = HEADER_SIZE;
        let leaf_len = PAGE_SIZE - base;

        // Mix of 1-byte and multi-byte varints: len 3 / id 1, and len 200 / id 300
        // (200 and 300 both need 2 bytes each).
        let rows: Vec<(u64, Vec<u8>)> = vec![(1, b"abc".to_vec()), (300, vec![7u8; 200])];
        let cells: Vec<Vec<u8>> = rows.iter().map(|(id, p)| cell_bytes(*id, p)).collect();

        // Pack cells contiguously at the end of the leaf; offsets come from real lengths.
        let total: usize = cells.iter().map(Vec::len).sum();
        let content_offset = leaf_len - total;

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

        let page = Page::from(&buf, 1).unwrap();
        let Page::TableLeaf(leaf) = page;

        assert_eq!(leaf.header.cell_count as usize, rows.len());
        assert_eq!(leaf.header.cell_content_offset as usize, content_offset);
        assert_eq!(leaf.cell_ptrs, ptrs);
        assert_eq!(leaf.cells.len(), rows.len());
        for (cell, (id, payload)) in leaf.cells.iter().zip(&rows) {
            assert_eq!(cell.size, payload.len() as u64);
            assert_eq!(cell.row_id, *id);
            assert_eq!(&cell.payload, payload);
        }
    }

    #[test]
    fn table_leaf_cell_roundtrip() {
        let cell = TableLeafCell {
            size: 3,
            row_id: 300,
            payload: b"abc".to_vec(),
        };
        let parsed = TableLeafCell::from(&cell.to_bytes()).unwrap();
        assert_eq!(
            (parsed.size, parsed.row_id, parsed.payload),
            (3, 300, b"abc".to_vec())
        );
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

        let page = Page::from(&buf, 1).unwrap();
        let Page::TableLeaf(leaf) = page;

        assert_eq!(leaf.cells.len(), rows.len());
        for (cell, (row_id, expected)) in leaf.cells.iter().zip(&rows) {
            assert_eq!(cell.row_id, *row_id);
            assert_eq!(&cell.decode_payload().unwrap(), expected);
        }
    }
}

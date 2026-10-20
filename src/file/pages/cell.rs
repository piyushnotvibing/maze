use std::fmt::Display;

use crate::file::pages::{PageType, bytes::read_word_u16_be, header::PageHeader, spec::PageSpec};

pub trait Cell: Sized {
    type Key: Ord + Display + Copy + Clone;

    fn serialize(&self) -> Vec<u8>;
    fn encoded_len(&self) -> usize;
    fn sort_key(&self) -> Self::Key;
    fn parse(buf: &[u8], ps: PageSpec) -> anyhow::Result<Self>;
}

pub(crate) fn parse_cells<C: Cell>(
    buf: &[u8],
    header: PageHeader,
    page_type: PageType,
    ps: PageSpec,
) -> anyhow::Result<Vec<C>> {
    let cell_content_region_start = match header.cell_content_offset {
        0 => 65536usize,
        n => n as usize,
    };
    let cell_ptr_region_end = page_type.header_size() + 2 * header.cell_count as usize;
    if cell_ptr_region_end > cell_content_region_start || cell_content_region_start > buf.len() {
        anyhow::bail!("invalid cell content offset")
    }

    let content_buffer = buf
        .get(page_type.header_size()..)
        .ok_or_else(|| anyhow::anyhow!("page smaller than page header"))?;
    let cell_ptrs = parse_cell_ptrs(content_buffer, header.cell_count as usize)?;

    let mut prev_row_id: Option<C::Key> = None;
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(cell_ptrs.len());
    let cells = cell_ptrs
        .iter()
        .map(|&ptr| {
            let ptr = ptr as usize;
            // If a cell pointer points anywhere other than the cell area of the page, then error out.
            if ptr < cell_content_region_start {
                anyhow::bail!(
                    "invalid cell pointer {ptr}: points outside the designated cell content region"
                )
            }

            let res = C::parse(
                buf.get(ptr..)
                    .ok_or_else(|| anyhow::anyhow!("cell out of bounds"))?,
                ps,
            )?;
            let end = ptr
                .checked_add(res.encoded_len())
                .ok_or_else(|| anyhow::anyhow!("cell end overflows"))?;
            if end > buf.len() {
                anyhow::bail!("cell extends past end of page");
            }
            spans.push((ptr, end));

            if let Some(prev) = prev_row_id.clone()
                && res.sort_key() <= (prev as <C as Cell>::Key)
            {
                anyhow::bail!("cell pointers not sorted by row_id")
            }
            prev_row_id = Some(res.sort_key());

            Ok(res)
        })
        .collect::<anyhow::Result<Vec<C>>>()?;

    // Pointer order is key order, not physical order — sort by offset to
    // detect overlaps/aliasing.
    spans.sort_by_key(|&(start, _)| start);
    for w in spans.windows(2) {
        if w[1].0 < w[0].1 {
            anyhow::bail!("corrupted buffer: overlapping cells");
        }
    }

    Ok(cells)
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

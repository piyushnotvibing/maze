use crate::file::pages::{PageType, bytes::read_word_u16_be, header::PageHeader};
pub trait Cell {
    fn serialize(&self) -> Vec<u8>;
    fn encoded_len(&self) -> usize;
    fn sort_key(&self) -> u64;
}

pub(crate) fn parse_cells<'a, C: Cell + TryFrom<&'a [u8], Error = anyhow::Error>>(
    buf: &'a [u8],
    header: PageHeader,
    page_type: PageType,
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

    let mut prev_row_id: Option<u64> = None;
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(cell_ptrs.len());
    let cells = cell_ptrs
        .iter()
        .map(|&ptr| {
            // If a cell pointer points anywhere other than the cell area of the page, then error out.
            if (ptr as usize) < cell_content_region_start {
                anyhow::bail!(
                    "invalid cell pointer {ptr}: points outside the designated cell content region"
                )
            }

            let ptr_usize = ptr as usize;
            let res = C::try_from(
                buf.get(ptr_usize..)
                    .ok_or_else(|| anyhow::anyhow!("cell out of bounds"))?,
            )?;
            let end = ptr_usize
                .checked_add(res.encoded_len())
                .ok_or_else(|| anyhow::anyhow!("cell end overflows"))?;
            if end > buf.len() {
                anyhow::bail!("cell extends past end of page");
            }
            spans.push((ptr_usize, end));

            if let Some(prev) = prev_row_id
                && res.sort_key() <= prev
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

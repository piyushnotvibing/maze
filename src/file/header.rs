pub const HEADER_SIZE: usize = 23;
const MAGIC_STR: &[u8] = b"Maze Format 1\0";
const HEADER_PAGE_SIZE_OFFSET: usize = 16;
const MAX_PAGE_SIZE: u32 = (u16::MAX as u32) + 1; // 65536

#[derive(Debug, Clone, Copy)]
pub struct HeaderInfo {
    // (1 << page_size_exp) gives page size.
    pub page_size_exp: PageSizeExp,
}

impl HeaderInfo {
    fn from(buf: &[u8]) -> anyhow::Result<Self> {
        if !buf.starts_with(MAGIC_STR) {
            let prefix = String::from_utf8_lossy(&buf[..MAGIC_STR.len()]);
            anyhow::bail!("invalid magic string: {prefix}");
        }

        let page_size_exp = u8::from_be_bytes(
            match buf[HEADER_PAGE_SIZE_OFFSET..HEADER_PAGE_SIZE_OFFSET + 1].try_into() {
                Ok(b) => b,
                Err(e) => anyhow::bail!("failed to parse page size exp: {e}"),
            },
        );

        let page_size_exp = PageSizeExp::new(page_size_exp)?;
        Ok(Self { page_size_exp })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PageSizeExp(u8);
impl PageSizeExp {
    fn new(val: u8) -> anyhow::Result<Self> {
        if (9..=16).contains(&val) {
            return Ok(Self(val));
        } else {
            anyhow::bail!("invalid page size exponent: {val}");
        };
    }

    fn get(&self) -> u8 {
        self.0
    }
}

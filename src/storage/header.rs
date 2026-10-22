use crate::storage::pages::types::PageNumber;

pub const MAZE_HEADER_SIZE: usize = 23;
pub const MAX_PAGE_COUNT: u32 = u32::MAX - 1;

const MAGIC_STR: &[u8] = b"Maze Format 2\0";
const HEADER_PAGE_SIZE_OFFSET: usize = 14;
const FIRST_FREELIST_TRUNK_PAGE_OFFSET: usize = 15;
const NUM_FREE_PAGES_OFFSET: usize = 19;
const MAX_PAGE_SIZE: usize = (u16::MAX as usize) + 1; // 65536
const MAX_FREE_PAGE_COUNT: u32 = u32::MAX - 1;

#[derive(Debug, Clone, Copy)]
pub struct MazeHeader {
    // (1 << page_size_exp) gives page size.
    pub page_size_exp: PageSizeExp,
    pub first_freelist_trunk_pn: Option<PageNumber>,
    // Max number of free pages = number of possible pages - 1 (page 1). therefore, type(free_page_count) = u32
    pub free_page_count: u32,
}

impl MazeHeader {
    pub fn new(page_size: usize) -> anyhow::Result<Self> {
        let page_size_exp = PageSizeExp::try_from(page_size)?;
        Ok(Self {
            page_size_exp,
            first_freelist_trunk_pn: None,
            free_page_count: 0,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(MAZE_HEADER_SIZE);
        bytes.extend_from_slice(MAGIC_STR); // 14 bytes
        bytes.push(self.page_size_exp.get()); // byte 14

        match self.first_freelist_trunk_pn {
            Some(pn) => bytes.extend(pn.get().to_be_bytes()),
            None => bytes.extend(0u32.to_be_bytes()),
        }

        bytes.extend(self.free_page_count.to_be_bytes());

        bytes
    }
}

impl TryFrom<&[u8]> for MazeHeader {
    type Error = anyhow::Error;

    fn try_from(buf: &[u8]) -> anyhow::Result<Self> {
        if buf.len() < MAZE_HEADER_SIZE {
            anyhow::bail!("file size is less than database header size")
        }

        if !buf.starts_with(MAGIC_STR) {
            let prefix = String::from_utf8_lossy(buf.get(..MAGIC_STR.len()).ok_or_else(|| {
                anyhow::anyhow!("db file is smaller than magic string itself bro how tf")
            })?);
            anyhow::bail!("invalid magic string: {prefix}");
        }

        let page_size_exp = u8::from_be_bytes(
            match buf[HEADER_PAGE_SIZE_OFFSET..HEADER_PAGE_SIZE_OFFSET + 1].try_into() {
                Ok(b) => b,
                Err(e) => anyhow::bail!("failed to parse page size exp: {e}"),
            },
        );
        let page_size_exp = PageSizeExp::new(page_size_exp)?;

        let freelist_pn_bytes = buf
            .get(FIRST_FREELIST_TRUNK_PAGE_OFFSET..(FIRST_FREELIST_TRUNK_PAGE_OFFSET + 4))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "header does not contain first freelist trunk page number, not even 0"
                )
            })?;
        let first_freelist_trunk_pn = u32::from_be_bytes(match freelist_pn_bytes.try_into() {
            Ok(b) => b,
            Err(e) => anyhow::bail!("failed to parse first freelist trunk page number: {e}"),
        });
        let first_freelist_trunk_pn = match first_freelist_trunk_pn {
            0 => None,
            n => Some(PageNumber::try_from(n)?),
        };

        let freelist_count_bytes = buf
            .get(NUM_FREE_PAGES_OFFSET..(NUM_FREE_PAGES_OFFSET + 4))
            .ok_or_else(|| {
                anyhow::anyhow!("header does not contain number of free pages, not even 0")
            })?;
        let free_page_count = u32::from_be_bytes(match freelist_count_bytes.try_into() {
            Ok(b) => b,
            Err(e) => anyhow::bail!("failed to parse number of free pages: {e}"),
        });

        if free_page_count > MAX_FREE_PAGE_COUNT {
            anyhow::bail!(
                "itne free pages ho hee nahi sakte bhai, are you seriously telling me you have a ~281 TB file full of just 0s?????? atp i think you should reconsider your life choices"
            )
        }

        Ok(Self {
            page_size_exp,
            first_freelist_trunk_pn,
            free_page_count,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PageSizeExp(u8);
impl PageSizeExp {
    pub fn new(val: u8) -> anyhow::Result<Self> {
        if (9..=16).contains(&val) {
            Ok(Self(val))
        } else {
            anyhow::bail!("invalid page size exponent: {val}");
        }
    }

    pub fn get(&self) -> u8 {
        self.0
    }
}

impl TryFrom<usize> for PageSizeExp {
    type Error = anyhow::Error;

    fn try_from(value: usize) -> anyhow::Result<Self> {
        if value > MAX_PAGE_SIZE {
            anyhow::bail!("invalid page size: too large")
        } else if !value.is_power_of_two() {
            anyhow::bail!("invalid page size: not a power of 2")
        }

        Ok(Self(value.ilog2() as u8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_str_parses_correctly() {
        let correct1 = b"Maze Format 2\x00\x09\x00\x00\x00\x00\x00\x00\x00\x00".as_slice(); // 9
        assert_eq!(
            MazeHeader::try_from(correct1).unwrap().page_size_exp.get(),
            PageSizeExp::new(9).unwrap().get()
        );

        let correct2 = b"Maze Format 2\x00\x10\x00\x00\x00\x00\x00\x00\x00\x00".as_slice(); // 16
        assert_eq!(
            MazeHeader::try_from(correct2).unwrap().page_size_exp.get(),
            PageSizeExp::new(16).unwrap().get()
        );
    }

    #[test]
    fn magic_str_parses_incorrectly() {
        let incorrect1 =
            b"this is a completely invalid header that should not parse\x00\x10\x00\x00\x00\x00\x00\x00\x00\x00".as_slice();
        let err = MazeHeader::try_from(incorrect1).unwrap_err();
        let incorrect_prefix = "this is a comp";
        assert_eq!(
            format!("{}", err),
            format!("invalid magic string: {incorrect_prefix}"),
        );

        let incorrect2 =
            b"Maze Format 2\x00\x14\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".as_slice(); // 20
        let err = MazeHeader::try_from(incorrect2).unwrap_err();
        assert_eq!(format!("{}", err), "invalid page size exponent: 20");
    }

    #[test]
    fn header_roundtrip() {
        let h = MazeHeader {
            page_size_exp: PageSizeExp::new(12).unwrap(),
            first_freelist_trunk_pn: None,
            free_page_count: 0,
        };
        let bytes = h.serialize();
        assert_eq!(bytes.len(), MAZE_HEADER_SIZE);
        assert_eq!(
            MazeHeader::try_from(bytes.as_slice())
                .unwrap()
                .page_size_exp
                .get(),
            12
        );
    }
}

pub const HEADER_SIZE: usize = 15;
const MAGIC_STR: &[u8] = b"Maze Format 1\0";
const HEADER_PAGE_SIZE_OFFSET: usize = 14;
const MAX_PAGE_SIZE: u32 = (u16::MAX as u32) + 1; // 65536

#[derive(Debug, Clone, Copy)]
pub struct MazeHeader {
    // (1 << page_size_exp) gives page size.
    pub page_size_exp: PageSizeExp,
}

impl MazeHeader {
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_SIZE);
        bytes.extend_from_slice(MAGIC_STR); // 14 bytes
        bytes.push(self.page_size_exp.get()); // byte 14
        bytes
    }
}

impl TryFrom<&[u8]> for MazeHeader {
    type Error = anyhow::Error;

    fn try_from(buf: &[u8]) -> anyhow::Result<Self> {
        if !buf.starts_with(MAGIC_STR) {
            let prefix = String::from_utf8_lossy(&buf.get(..MAGIC_STR.len()).ok_or_else(|| {
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
        Ok(Self { page_size_exp })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PageSizeExp(u8);
impl PageSizeExp {
    fn new(val: u8) -> anyhow::Result<Self> {
        if (9..=16).contains(&val) {
            Ok(Self(val))
        } else {
            anyhow::bail!("invalid page size exponent: {val}");
        }
    }

    fn get(&self) -> u8 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_str_parses_correctly() {
        let correct1 = b"Maze Format 1\x00\x09".as_slice(); // 9
        assert_eq!(
            MazeHeader::try_from(correct1).unwrap().page_size_exp.get(),
            PageSizeExp::new(9).unwrap().get()
        );

        let correct2 = b"Maze Format 1\x00\x10".as_slice(); // 16
        assert_eq!(
            MazeHeader::try_from(correct2).unwrap().page_size_exp.get(),
            PageSizeExp::new(16).unwrap().get()
        );
    }

    #[test]
    fn magic_str_parses_incorrectly() {
        let incorrect1 =
            b"this is a completely invalid header that should not parse\x00\x10".as_slice();
        let err = MazeHeader::try_from(incorrect1).unwrap_err();
        let incorrect_prefix = "this is a comp";
        assert_eq!(
            format!("{}", err),
            format!("invalid magic string: {incorrect_prefix}"),
        );

        let incorrect2 = b"Maze Format 1\x00\x14".as_slice(); // 20
        let err = MazeHeader::try_from(incorrect2).unwrap_err();
        assert_eq!(format!("{}", err), "invalid page size exponent: 20");
    }

    #[test]
    fn header_roundtrip() {
        let h = MazeHeader {
            page_size_exp: PageSizeExp::new(12).unwrap(),
        };
        let bytes = h.serialize();
        assert_eq!(bytes.len(), HEADER_SIZE);
        assert_eq!(
            MazeHeader::try_from(bytes.as_slice())
                .unwrap()
                .page_size_exp
                .get(),
            12
        );
    }
}

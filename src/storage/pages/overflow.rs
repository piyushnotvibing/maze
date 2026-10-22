use crate::storage::pages::{spec::PageSpec, types::PageNumber};

/*
    Structure of an overflow page:

    <-----------------------page size------------------------->
     __________________________________________________________
    |  Next    |                                               |
    | Overflow | payload bytes                                 |
    | PageNum  |                                               |
    |__________|_______________________________________________|
    <----4B---->
*/

pub struct OverflowPage {
    pub next: Option<PageNumber>,
    pub local_payload: Vec<u8>,
}

impl OverflowPage {
    pub fn new(next: Option<PageNumber>, local_payload: Vec<u8>) -> Self {
        Self { next, local_payload }
    }

    pub fn capacity(page_size: usize) -> usize {
        page_size - 4
    }

    pub fn peek_next(buf: &[u8]) -> anyhow::Result<Option<PageNumber>> {
        if buf.len() < 4 {
            anyhow::bail!("buffer smaller than next overflow page number field size")
        }

        let next_bytes = buf
            .get(..4)
            .ok_or_else(|| anyhow::anyhow!("this error should never occur lol"))?;
        let next = u32::from_be_bytes(match next_bytes.try_into() {
            Ok(b) => b,
            Err(e) => anyhow::bail!("failed to parse next overflow page number: {e}"),
        });
        let next = match next {
            0 => None,
            n => Some(PageNumber::try_from(n)?),
        };

        Ok(next)
    }

    pub fn parse(buf: &[u8], remaining: usize) -> anyhow::Result<Self> {
        if buf.len() < 4 {
            anyhow::bail!("overflow page smaller than overflow header")
        }
        if remaining > (buf.len() - 4) {
            anyhow::bail!(
                "invalid remaining payload length: {remaining} > {}",
                buf.len() - 4
            )
        }

        let next = Self::peek_next(buf)?;

        let local_payload = buf
            .get(4..(remaining + 4))
            .ok_or_else(|| anyhow::anyhow!("invalid remaining value"))?
            .to_vec();

        Ok(Self {
            next,
            local_payload,
        })
    }

    pub fn serialize(&self, ps: PageSpec) -> anyhow::Result<Vec<u8>> {
        if ps.first {
            anyhow::bail!("first page of the database cannot be an overflow page")
        }

        let mut buf = vec![0u8; ps.page_size];

        let next = match self.next {
            Some(n) => n.get(),
            None => 0,
        };

        buf[..4].copy_from_slice(&next.to_be_bytes());
        let local_payload_len = self.local_payload.len();
        buf[4..(local_payload_len + 4)].copy_from_slice(&self.local_payload);

        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pn(n: u32) -> PageNumber {
        PageNumber::try_from(n).unwrap()
    }

    fn fresh_spec(page_size: usize) -> PageSpec {
        PageSpec::fresh(page_size)
    }

    #[test]
    fn capacity_matches_layout_math() {
        // capacity = page_size - 4: a single 4-byte `next` header, the rest is raw payload.
        assert_eq!(OverflowPage::capacity(512), 508);
        assert_eq!(OverflowPage::capacity(4096), 4092);
        assert_eq!(OverflowPage::capacity(65536), 65532);
    }

    #[test]
    fn roundtrips_with_and_without_next() {
        for next in [None, Some(pn(7))] {
            let page = OverflowPage::new(next, vec![0xABu8; 100]);
            let bytes = page.serialize(fresh_spec(512)).unwrap();
            assert_eq!(bytes.len(), 512);
            let parsed = OverflowPage::parse(&bytes, 100).unwrap();
            assert_eq!(parsed.next, next);
            assert_eq!(parsed.local_payload, vec![0xABu8; 100]);
        }
    }

    #[test]
    fn roundtrips_a_full_page_of_payload() {
        let cap = OverflowPage::capacity(512);
        let payload = vec![0xCDu8; cap];
        let page = OverflowPage::new(None, payload.clone());
        let bytes = page.serialize(fresh_spec(512)).unwrap();
        let parsed = OverflowPage::parse(&bytes, cap).unwrap();
        assert_eq!(parsed.local_payload, payload);
        assert_eq!(parsed.next, None);
    }

    #[test]
    fn roundtrips_empty_local_payload() {
        let page = OverflowPage::new(Some(pn(3)), Vec::new());
        let bytes = page.serialize(fresh_spec(512)).unwrap();
        let parsed = OverflowPage::parse(&bytes, 0).unwrap();
        assert!(parsed.local_payload.is_empty());
        assert_eq!(parsed.next, Some(pn(3)));
    }

    #[test]
    fn peek_next_agrees_with_full_parse() {
        for next in [None, Some(pn(42))] {
            let page = OverflowPage::new(next, vec![1, 2, 3]);
            let bytes = page.serialize(fresh_spec(512)).unwrap();
            assert_eq!(OverflowPage::peek_next(&bytes).unwrap(), next);
            assert_eq!(
                OverflowPage::parse(&bytes, 3).unwrap().next,
                OverflowPage::peek_next(&bytes).unwrap()
            );
        }
    }

    #[test]
    fn parse_ignores_bytes_beyond_remaining() {
        // only the first `remaining` bytes after the header are real payload; whatever is
        // physically stored past that (stale bytes from a shorter previous write, etc.) must
        // never leak into the returned local_payload.
        let mut buf = vec![0xFFu8; 512];
        buf[..4].copy_from_slice(&0u32.to_be_bytes());
        buf[4..9].copy_from_slice(&[1, 2, 3, 4, 5]);
        let parsed = OverflowPage::parse(&buf, 5).unwrap();
        assert_eq!(parsed.local_payload, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn parse_rejects_buffer_smaller_than_header() {
        assert!(OverflowPage::parse(&[0u8; 3], 0).is_err());
    }

    #[test]
    fn parse_rejects_remaining_past_capacity() {
        let buf = vec![0u8; 512];
        assert!(OverflowPage::parse(&buf, 509).is_err()); // capacity is 508
        assert!(OverflowPage::parse(&buf, 508).is_ok());
    }

    #[test]
    fn peek_next_rejects_buffer_smaller_than_header() {
        assert!(OverflowPage::peek_next(&[0u8; 3]).is_err());
    }

    #[test]
    fn serialize_rejects_first_page_spec() {
        let page = OverflowPage::new(None, vec![1, 2, 3]);
        let first = PageSpec::new(512, pn(1));
        assert!(first.first);
        assert!(page.serialize(first).is_err());
    }
}

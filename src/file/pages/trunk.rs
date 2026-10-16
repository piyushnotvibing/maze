use crate::file::pages::{spec::PageSpec, types::PageNumber};

const TRUNK_PAGE_HEADER_SIZE: usize = 8;
const FREE_PAGE_COUNT_OFFSET: usize = 4;
const FREE_PAGE_NUMBER_OFFSET: usize = 8;

/*
    Structure of a trunk page:

    <-----------------------page size------------------------->
     _________________________________________________________
    |  Next   |        |Free|                            |Last|
    |  Trunk  | #Free  |Page| .....                      |Free|
    | PageNum | Pages  | #1 |                            |Page|
    |_________|________|____|____________________________|____|
    <---4B----><--4B---><4B->
*/

pub struct FreeListTrunkPage {
    pub next: Option<PageNumber>,
    pub free: Vec<PageNumber>,
}

impl FreeListTrunkPage {
    pub fn capacity(page_size: usize) -> usize {
        (page_size - 8) / 4
    }

    pub fn parse(buf: &[u8]) -> anyhow::Result<Self> {
        if buf.len() < TRUNK_PAGE_HEADER_SIZE {
            anyhow::bail!("buffer smaller than trunk page header")
        }

        let next_bytes = buf
            .get(..FREE_PAGE_COUNT_OFFSET)
            .ok_or_else(|| anyhow::anyhow!("this error should never occur but ok"))?;
        let next = u32::from_be_bytes(match next_bytes.try_into() {
            Ok(b) => b,
            Err(e) => anyhow::bail!("failed to parse next trunk page number: {e}"),
        });
        let next = match next {
            0 => None,
            n => Some(PageNumber::try_from(n)?),
        };

        let count_bytes = buf
            .get(FREE_PAGE_COUNT_OFFSET..FREE_PAGE_NUMBER_OFFSET)
            .ok_or_else(|| anyhow::anyhow!("this error should also never occur but ok"))?;
        let count = u32::from_be_bytes(match count_bytes.try_into() {
            Ok(b) => b,
            Err(e) => anyhow::bail!("failed to parse free page count: {e}"),
        });

        let free = match count {
            0 => Vec::new(),
            count => {
                let free_pn_bytes = buf
                    .get(FREE_PAGE_NUMBER_OFFSET..(FREE_PAGE_NUMBER_OFFSET + (count as usize) * 4))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "either the count of free pages is wrong or this buffer is corrupted"
                        )
                    })?;
                let free = free_pn_bytes
                    .chunks(4)
                    .map(|chunk| {
                        match PageNumber::try_from(u32::from_be_bytes(match chunk.try_into() {
                            Ok(b) => b,
                            Err(e) => anyhow::bail!("failed to parse a free page number: {e}"),
                        })) {
                            Ok(pn) => Ok(pn),
                            Err(e) => {
                                Err(anyhow::anyhow!("failed to parse a free page number: {e}"))
                            }
                        }
                    })
                    .collect::<anyhow::Result<Vec<PageNumber>>>()?;

                free
            }
        };

        Ok(Self { next, free })
    }

    pub fn serialize(&self, ps: PageSpec) -> anyhow::Result<Vec<u8>> {
        if ps.first {
            anyhow::bail!("first page in database can't be a freelist trunk page")
        }

        let free_page_count = self.free.len();
        if free_page_count > Self::capacity(ps.page_size) {
            anyhow::bail!("more free pages than space available to store their page numbers")
        }

        let mut buf = vec![0u8; ps.page_size];

        let next = match self.next {
            Some(n) => n.get(),
            None => 0,
        };

        buf[..FREE_PAGE_COUNT_OFFSET].copy_from_slice(&next.to_be_bytes());
        buf[FREE_PAGE_COUNT_OFFSET..FREE_PAGE_NUMBER_OFFSET]
            .copy_from_slice(&(free_page_count as u32).to_be_bytes());

        let free_pn_bytes = vecu32_to_vecu8(
            &self
                .free
                .iter()
                .map(|free_pn| free_pn.get())
                .collect::<Vec<u32>>(),
        );
        buf[FREE_PAGE_NUMBER_OFFSET..(FREE_PAGE_NUMBER_OFFSET + free_page_count * 4)]
            .copy_from_slice(&free_pn_bytes);

        Ok(buf)
    }

    /// gets a free page from the head trunk page.
    /// if None is returned, take this trunk page as the free page.
    pub fn get(&mut self) -> Option<PageNumber> {
        self.free.pop()
    }

    /// inserts a free page into the head
    pub fn insert(&mut self, freed_pn: PageNumber, page_size: usize) -> bool {
        if self.free.len() + 1 > Self::capacity(page_size) {
            return false;
        }

        self.free.push(freed_pn);
        true
    }
}

fn vecu32_to_vecu8(v: &[u32]) -> Vec<u8> {
    let capacity = (32 / 8) * v.len() as usize;
    let mut out = Vec::with_capacity(capacity);

    for value in v {
        out.extend_from_slice(&value.to_be_bytes());
    }

    out
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
        // capacity = (page_size - 8) / 4 : 4B next + 4B count + 4B per entry.
        assert_eq!(FreeListTrunkPage::capacity(512), 126);
        assert_eq!(FreeListTrunkPage::capacity(4096), 1022);
        assert_eq!(FreeListTrunkPage::capacity(65536), 16382);
    }

    #[test]
    fn empty_trunk_roundtrips_with_and_without_next() {
        for next in [None, Some(pn(7))] {
            let page = FreeListTrunkPage {
                next,
                free: Vec::new(),
            };
            let bytes = page.serialize(fresh_spec(512)).unwrap();
            assert_eq!(bytes.len(), 512);
            let parsed = FreeListTrunkPage::parse(&bytes).unwrap();
            assert_eq!(parsed.next, next);
            assert!(parsed.free.is_empty());
        }
    }

    #[test]
    fn full_trunk_roundtrips_and_one_more_does_not_fit() {
        let full: Vec<PageNumber> = (2u32..=127).map(pn).collect();
        assert_eq!(full.len(), FreeListTrunkPage::capacity(512));
        let page = FreeListTrunkPage {
            next: None,
            free: full.clone(),
        };
        let bytes = page.serialize(fresh_spec(512)).unwrap();
        let parsed = FreeListTrunkPage::parse(&bytes).unwrap();
        assert_eq!(parsed.free, full);
        assert_eq!(parsed.next, None);

        let overfull = FreeListTrunkPage {
            next: None,
            free: (2u32..=128).map(pn).collect(),
        };
        assert!(overfull.serialize(fresh_spec(512)).is_err());
    }

    #[test]
    fn serialize_rejects_first_page_spec() {
        let page = FreeListTrunkPage {
            next: None,
            free: Vec::new(),
        };
        let first = PageSpec::new(512, pn(1));
        assert!(first.first);
        assert!(page.serialize(first).is_err());
    }

    #[test]
    fn parse_rejects_truncated_and_inconsistent_buffers() {
        // shorter than the 8-byte trunk header
        assert!(FreeListTrunkPage::parse(&[0u8; 7]).is_err());

        // count claims entries past the end of the buffer
        let mut buf = vec![0u8; 512];
        buf[4..8].copy_from_slice(&1000u32.to_be_bytes());
        assert!(FreeListTrunkPage::parse(&buf).is_err());

        // count u32::MAX can never fit
        let mut buf = vec![0u8; 512];
        buf[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(FreeListTrunkPage::parse(&buf).is_err());

        // page number 0 is never valid
        let mut buf = vec![0u8; 512];
        buf[4..8].copy_from_slice(&1u32.to_be_bytes());
        buf[8..12].copy_from_slice(&0u32.to_be_bytes());
        assert!(FreeListTrunkPage::parse(&buf).is_err());
    }

    #[test]
    fn parse_tolerates_trailing_garbage_after_zero_count() {
        let mut buf = vec![0xAAu8; 512];
        buf[..4].copy_from_slice(&0u32.to_be_bytes());
        buf[4..8].copy_from_slice(&0u32.to_be_bytes());
        let parsed = FreeListTrunkPage::parse(&buf).unwrap();
        assert_eq!(parsed.next, None);
        assert!(parsed.free.is_empty());
    }

    #[test]
    fn insert_fills_to_capacity_then_reports_full_and_get_is_lifo() {
        let mut page = FreeListTrunkPage {
            next: None,
            free: Vec::new(),
        };
        let cap = FreeListTrunkPage::capacity(512);
        for i in 2u32..(2 + cap as u32) {
            assert!(page.insert(pn(i), 512), "insert of {i} should fit");
        }
        assert!(
            !page.insert(pn(1000), 512),
            "one past capacity must not fit"
        );
        // get pops in reverse insertion order
        assert_eq!(page.get(), Some(pn(2 + cap as u32 - 1)));
        assert!(
            page.insert(pn(1000), 512),
            "slot freed by get should fit again"
        );
        assert_eq!(page.get(), Some(pn(1000)));
    }

    #[test]
    fn get_on_empty_trunk_returns_none() {
        let mut page = FreeListTrunkPage {
            next: Some(pn(9)),
            free: Vec::new(),
        };
        assert_eq!(page.get(), None);
        // head-with-entries contract lives at the pager layer: empty free
        // means "take the trunk page itself", signalled by None here.
    }

    #[test]
    fn parse_accepts_page_one_entries_layering_guards_live_upstairs() {
        // The trunk layer does not reject page 1; pager walks do.
        // Pin the layering so a future trunk-level reject doesn't slip in silently.
        let mut buf = vec![0u8; 512];
        buf[4..8].copy_from_slice(&1u32.to_be_bytes());
        buf[8..12].copy_from_slice(&1u32.to_be_bytes());
        let parsed = FreeListTrunkPage::parse(&buf).unwrap();
        assert_eq!(parsed.free, vec![pn(1)]);

        let mut buf = vec![0u8; 512];
        buf[..4].copy_from_slice(&1u32.to_be_bytes());
        let parsed = FreeListTrunkPage::parse(&buf).unwrap();
        assert_eq!(parsed.next, Some(pn(1)));
    }
}

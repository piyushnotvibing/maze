use std::collections::HashSet;

use crate::file::{
    pager::Pager,
    pages::{overflow::OverflowPage, types::PageNumber},
};

impl Pager {
    /// Creates the overflow pages, inserts the payload and returns the head overflow page's number.
    pub(crate) fn write_overflow(&mut self, payload: &[u8]) -> anyhow::Result<Option<PageNumber>> {
        if payload.is_empty() {
            return Ok(None);
        }

        let cap = OverflowPage::capacity(self.page_size);
        let payload_chunks = payload.chunks(cap).collect::<Vec<&[u8]>>();
        let pns = payload_chunks
            .iter()
            .map(|_| match self.allocate_page() {
                Ok(pn) => Ok(pn),
                Err(e) => anyhow::bail!("failed to allocate overflow page: {e}"),
            })
            .collect::<anyhow::Result<Vec<PageNumber>>>()?;

        for i in 0..pns.len() {
            let next = pns.get(i + 1).copied();
            let overflow_page = OverflowPage::new(next, payload_chunks[i].to_vec());
            self.write_page(pns[i], overflow_page.serialize(self.spec(pns[i]))?)?;
        }

        Ok(Some(pns[0]))
    }

    /// Only returns the resolved payload stored in overflow pages of a table leaf cell.
    /// Need to merge the returned bytes with the local payload of the cell to get the
    /// full payload.
    /// `overflow_payload_len` = total payload len - leaf's local payload len
    pub(crate) fn read_overflow(
        &mut self,
        overflow_head_pn: Option<PageNumber>,
        overflow_payload_len: usize,
    ) -> anyhow::Result<Vec<u8>> {
        if overflow_head_pn.is_none() {
            if overflow_payload_len != 0 {
                anyhow::bail!("head is none but overflow payload exists")
            }
            return Ok(Vec::new());
        }

        let mut out = Vec::with_capacity(overflow_payload_len);
        let mut visited: HashSet<PageNumber> = HashSet::new();
        let mut current = overflow_head_pn.unwrap();

        loop {
            if current == 1 {
                anyhow::bail!("first page of the database cannot be an overflow page")
            }
            if !visited.insert(current) {
                anyhow::bail!("cycle detected; overflow pages form a cycle")
            }
            let remaining = overflow_payload_len - out.len();
            if remaining == 0 {
                anyhow::bail!("overflow page chain longer than declared length")
            }
            let remaining = remaining.min(OverflowPage::capacity(self.page_size));
            let overflow_page = OverflowPage::parse(self.get_page(current)?, remaining)?;
            out.extend(overflow_page.local_payload);
            match overflow_page.next {
                Some(pn) => current = pn,
                None => {
                    if out.len() != overflow_payload_len {
                        anyhow::bail!("overflow page chain shorter than reported payload length")
                    }
                    break;
                }
            }
        }

        Ok(out)
    }

    // Frees overflow pages associated with a leaf page.
    pub(crate) fn free_overflow(
        &mut self,
        overflow_head_pn: Option<PageNumber>,
    ) -> anyhow::Result<()> {
        if overflow_head_pn.is_none() {
            return Ok(());
        }
        let mut current = overflow_head_pn.unwrap();
        let mut visited: HashSet<PageNumber> = HashSet::new();
        loop {
            if current == 1 {
                anyhow::bail!("first page of the database cannot be an overflow page")
            }
            if !visited.insert(current) {
                anyhow::bail!("cycle detected; overflow pages form a cycle")
            }
            let next = OverflowPage::peek_next(self.get_page(current)?)?;
            self.free_page(current)?;
            match next {
                None => break,
                Some(pn) => {
                    current = pn;
                },
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::pages::overflow::OverflowPage;
    use std::sync::atomic::{AtomicU64, Ordering};

    const PAGE_SIZE: usize = 512;
    const CAP: usize = 508; // OverflowPage::capacity(512)

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn pn(n: u32) -> PageNumber {
        PageNumber::try_from(n).unwrap()
    }

    fn temp_db_path(tag: &str) -> String {
        let mut path = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        path.push(format!("maze_overflow_test_{tag}_{unique}_{seq}.mz"));
        path.to_str().unwrap().to_string()
    }

    fn fresh_pager(tag: &str) -> (Pager, String) {
        let path = temp_db_path(tag);
        let pager = Pager::new(&path, PAGE_SIZE).unwrap();
        (pager, path)
    }

    /// Writes a raw overflow page directly, bypassing `write_overflow`, so tests can build
    /// chains with deliberately corrupt shapes (cycles, page-1 links) that the real writer
    /// would never produce.
    fn write_raw_overflow(pager: &mut Pager, at: PageNumber, next: Option<PageNumber>, data: &[u8]) {
        let bytes = OverflowPage::new(next, data.to_vec())
            .serialize(pager.spec(at))
            .unwrap();
        pager.write_page(at, bytes).unwrap();
    }

    #[test]
    fn write_overflow_of_empty_payload_returns_none() {
        let (mut pager, path) = fresh_pager("empty");
        assert_eq!(pager.write_overflow(&[]).unwrap(), None);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_overflow_of_none_head_requires_zero_length() {
        let (mut pager, path) = fresh_pager("none_head");
        assert_eq!(pager.read_overflow(None, 0).unwrap(), Vec::<u8>::new());
        assert!(pager.read_overflow(None, 1).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn free_overflow_of_none_head_is_a_noop() {
        let (mut pager, path) = fresh_pager("free_none");
        pager.free_overflow(None).unwrap();
        assert_eq!(pager.count_fp().unwrap(), 0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn roundtrips_a_payload_smaller_than_one_page() {
        let (mut pager, path) = fresh_pager("single");
        let payload = vec![0xABu8; 100];
        let head = pager.write_overflow(&payload).unwrap();
        assert!(head.is_some());
        assert_eq!(pager.read_overflow(head, payload.len()).unwrap(), payload);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn roundtrips_a_payload_exactly_one_page_capacity() {
        let (mut pager, path) = fresh_pager("exact_cap");
        let payload = vec![0x11u8; CAP];
        let head = pager.write_overflow(&payload).unwrap();
        assert_eq!(pager.read_overflow(head, payload.len()).unwrap(), payload);
        // exactly fills one page: no second page should have been allocated.
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 2 * PAGE_SIZE as u64);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn roundtrips_a_payload_spanning_several_pages() {
        let (mut pager, path) = fresh_pager("multi");
        // enough to need 3 overflow pages: 2 full + a partial third.
        let payload: Vec<u8> = (0u32..(2 * CAP as u32 + 37))
            .map(|i| (i % 251) as u8)
            .collect();
        let head = pager.write_overflow(&payload).unwrap();
        assert_eq!(pager.read_overflow(head, payload.len()).unwrap(), payload);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 4 * PAGE_SIZE as u64);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn write_overflow_reuses_freelist_pages_before_growing_the_file() {
        let (mut pager, path) = fresh_pager("reuse");
        let p2 = pager.allocate_page().unwrap();
        let p3 = pager.allocate_page().unwrap();
        pager.free_page(p2).unwrap();
        pager.free_page(p3).unwrap();
        let size_before = std::fs::metadata(&path).unwrap().len();

        // one full page (p3, LIFO) + a partial second page (p2): needs exactly the two
        // already-free pages, so the file should not grow at all.
        let payload = vec![0x22u8; CAP + 10];
        let head = pager.write_overflow(&payload).unwrap();
        assert_eq!(pager.read_overflow(head, payload.len()).unwrap(), payload);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size_before);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn free_overflow_returns_every_page_in_the_chain() {
        let (mut pager, path) = fresh_pager("free_chain");
        let payload = vec![0x33u8; 2 * CAP + 5]; // 3 overflow pages
        let head = pager.write_overflow(&payload).unwrap();

        pager.free_overflow(head).unwrap();
        assert_eq!(pager.count_fp().unwrap(), 3);

        // and those pages are genuinely reusable afterwards.
        let size_before = std::fs::metadata(&path).unwrap().len();
        let reused = pager.allocate_page().unwrap();
        assert!(pager.list_fp().unwrap().contains(&reused) == false); // no longer free once allocated
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size_before);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_overflow_rejects_chain_shorter_than_declared_length() {
        let (mut pager, path) = fresh_pager("short");
        let head = pager.write_overflow(&vec![0u8; 50]).unwrap();
        // declare more than one page's capacity worth of bytes, so satisfying it would require
        // a second chain page that doesn't exist (the real chain is a single page, next=None).
        let err = pager
            .read_overflow(head, CAP + 10)
            .unwrap_err()
            .to_string();
        assert!(err.contains("shorter"), "unexpected: {err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_overflow_rejects_chain_longer_than_declared_length() {
        let (mut pager, path) = fresh_pager("long");
        let head = pager.write_overflow(&vec![0u8; 2 * CAP + 5]).unwrap();
        // claim there's only 50 bytes when the chain actually continues past that.
        let err = pager.read_overflow(head, 50).unwrap_err().to_string();
        assert!(err.contains("longer"), "unexpected: {err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_and_free_overflow_reject_page_one_in_the_chain() {
        for which in ["head", "mid"] {
            let (mut pager, path) = fresh_pager(&format!("pone_{which}"));
            let p2 = pager.allocate_page().unwrap();
            let p3 = pager.allocate_page().unwrap();
            let (head, total_len) = match which {
                "head" => {
                    // head itself is page 1 — never a legitimate state, but the walk must
                    // still reject it defensively rather than reading page 1 as payload.
                    (Some(pn(1)), 10)
                }
                _ => {
                    write_raw_overflow(&mut pager, p2, Some(pn(1)), &[0u8; 10]);
                    (Some(p2), 10 + 10)
                }
            };
            let _ = p3;
            assert!(pager.read_overflow(head, total_len).is_err());
            assert!(pager.free_overflow(head).is_err());
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn read_and_free_overflow_detect_a_two_page_cycle() {
        let (mut pager, path) = fresh_pager("two_cycle");
        let a = pager.allocate_page().unwrap();
        let b = pager.allocate_page().unwrap();
        write_raw_overflow(&mut pager, a, Some(b), &[0u8; 5]);
        write_raw_overflow(&mut pager, b, Some(a), &[0u8; 5]);

        // declared length must survive a full lap through both pages (2 * CAP) before the walk
        // would loop back to `a` a second time and hit the cycle check; too small a length
        // trips the (also-correct, but different) "chain longer than declared" check instead.
        let declared_len = 2 * CAP + 10;
        let read_err = pager
            .read_overflow(Some(a), declared_len)
            .unwrap_err()
            .to_string();
        assert!(read_err.contains("cycle"), "unexpected: {read_err}");
        let free_err = pager.free_overflow(Some(a)).unwrap_err().to_string();
        assert!(free_err.contains("cycle"), "unexpected: {free_err}");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn read_and_free_overflow_detect_a_self_loop() {
        let (mut pager, path) = fresh_pager("self_loop");
        let a = pager.allocate_page().unwrap();
        write_raw_overflow(&mut pager, a, Some(a), &[0u8; 5]);

        let read_err = pager.read_overflow(Some(a), 10).unwrap_err().to_string();
        assert!(read_err.contains("cycle"), "unexpected: {read_err}");
        let free_err = pager.free_overflow(Some(a)).unwrap_err().to_string();
        assert!(free_err.contains("cycle"), "unexpected: {free_err}");
        std::fs::remove_file(&path).ok();
    }
}

use std::collections::HashSet;

use crate::file::{
    pager::Pager,
    pages::{trunk::FreeListTrunkPage, types::PageNumber},
};

impl Pager {
    /// inserts a free page into the list.
    /// only ever inserts a free page's number into the head freelist trunk page.
    /// cases:
    ///     1. the freelist is empty: the first call to free_page() stores that single free page's number, and
    ///     the next allocate_page() directly takes this page number.
    ///     2. the freelist has a head: the free page's number is inserted into the head.
    ///     3. the freelist's head is full: the freelist trunk page at the head is full, so the newly freed page
    ///         becomes the new empty freelist trunk page head, and it points to the old head.
    pub(super) fn insert_fp(&mut self, freed_pn: PageNumber) -> anyhow::Result<()> {
        if freed_pn == 1 {
            anyhow::bail!("first page of the database cannot be a freelist trunk page")
        }
        // Very slow duplicate free page check.
        // if self.list_fp()?.contains(&freed_pn) {
        //     anyhow::bail!("cannot free an already freed page")
        // }
        let new_count = self.header.free_page_count.checked_add(1).ok_or_else(|| {
            anyhow::anyhow!("dawg there are enough free pages already why ARE YOU ADDING MORE???")
        })?;
        match self.header.first_freelist_trunk_pn {
            Some(head_pn) => {
                let mut head = FreeListTrunkPage::parse(self.get_page(head_pn)?)?;
                if !head.insert(freed_pn, self.spec(freed_pn).page_size) {
                    let mut new_head_bytes = vec![0u8; self.spec(freed_pn).page_size];
                    new_head_bytes[..4].copy_from_slice(&head_pn.get().to_be_bytes());
                    self.write_page(freed_pn, new_head_bytes)?;
                    self.header.first_freelist_trunk_pn = Some(freed_pn);
                } else {
                    self.write_page(head_pn, head.serialize(self.spec(head_pn))?)?;
                }
            }
            None => {
                self.write_page(freed_pn, vec![0u8; self.spec(freed_pn).page_size])?;
                self.header.first_freelist_trunk_pn = Some(freed_pn);
            }
        }

        self.sync_freelist_header(self.header.first_freelist_trunk_pn, new_count)?;

        Ok(())
    }

    /// gets a free page's number.
    /// only ever retrieves free pages from th ehad freelist trunk page.
    /// cases:  
    ///     1. the freelist is empty: return None.
    ///     2. the freelist head is not empty: retrieve a page number from the trunk page (removing it and all is trunk page's responsibility)
    ///     3. the freelist head is empty but there is a next trunk page: return head, set freelist head = current head -> next
    ///     4. the freelist head is empty and there is no next trunk: the head is returned and the freelist is empty.
    pub(super) fn get_fp(&mut self) -> anyhow::Result<Option<PageNumber>> {
        match self.header.first_freelist_trunk_pn {
            None => Ok(None),
            Some(head_pn) => {
                let mut head = FreeListTrunkPage::parse(self.get_page(head_pn)?)?;

                match head.get() {
                    Some(free_pn) => {
                        if free_pn == 1 {
                            anyhow::bail!("first page of the database cannot be a free page")
                        }
                        let new_count =
                            self.header.free_page_count.checked_sub(1).ok_or_else(|| {
                                anyhow::anyhow!("how tf are number of free pages going negative")
                            })?;
                        self.write_page(head_pn, head.serialize(self.spec(head_pn))?)?;
                        self.sync_freelist_header(self.header.first_freelist_trunk_pn, new_count)?;
                        self.write_page(free_pn, vec![0u8; self.spec(free_pn).page_size])?;
                        Ok(Some(free_pn))
                    }
                    // Cases 3 and 4 both are covered by this branch.
                    None => {
                        if let Some(next) = head.next
                            && next == 1
                        {
                            anyhow::bail!("first page of the database cannot be a free page")
                        }
                        let new_count =
                            self.header.free_page_count.checked_sub(1).ok_or_else(|| {
                                anyhow::anyhow!("how tf are number of free pages going negative")
                            })?;
                        self.header.first_freelist_trunk_pn = head.next;
                        self.write_page(head_pn, vec![0u8; self.spec(head_pn).page_size])?;
                        self.sync_freelist_header(self.header.first_freelist_trunk_pn, new_count)?;
                        Ok(Some(head_pn))
                    }
                }
            }
        }
    }

    pub fn count_fp(&mut self) -> anyhow::Result<usize> {
        match self.header.first_freelist_trunk_pn {
            None => Ok(0),
            Some(head_pn) => {
                let head = FreeListTrunkPage::parse(self.get_page(head_pn)?)?;
                let mut count = 0;
                let mut current = head;
                let mut visited: HashSet<PageNumber> = HashSet::new();
                let mut trunk_pn = head_pn;
                loop {
                    if current.free.contains(&PageNumber::try_from(1).unwrap()) {
                        anyhow::bail!("first page of the database cannot be a freelist trunk page")
                    }
                    count += current.free.len() + 1;
                    visited.insert(trunk_pn);
                    if current.next.is_none() {
                        break;
                    }
                    let next = current.next.unwrap();
                    if next == 1 {
                        anyhow::bail!("first page of the database cannot be a freelist trunk page")
                    }
                    if visited.contains(&next) {
                        anyhow::bail!("cycle exists in freelist")
                    }
                    current = FreeListTrunkPage::parse(self.get_page(next)?)?;
                    trunk_pn = next;
                }
                Ok(count)
            }
        }
    }

    pub fn list_fp(&mut self) -> anyhow::Result<Vec<PageNumber>> {
        match self.header.first_freelist_trunk_pn {
            None => Ok(Vec::new()),
            Some(head_pn) => {
                let mut current = FreeListTrunkPage::parse(self.get_page(head_pn)?)?;
                let mut trunk_pn = head_pn;
                let mut visited: HashSet<PageNumber> = HashSet::new();
                let mut out = Vec::new();
                loop {
                    if current.free.contains(&PageNumber::try_from(1).unwrap()) {
                        anyhow::bail!("first page of the database cannot be a freelist trunk page")
                    }
                    out.extend_from_slice(current.free.as_slice());
                    out.push(trunk_pn);
                    visited.insert(trunk_pn);
                    if current.next.is_none() {
                        break;
                    }
                    let next = current.next.unwrap();
                    if next == 1 {
                        anyhow::bail!("first page of the database cannot be a freelist trunk page")
                    }
                    if visited.contains(&next) {
                        anyhow::bail!("cycle exists in freelist")
                    }
                    current = FreeListTrunkPage::parse(self.get_page(next)?)?;
                    trunk_pn = next;
                }
                Ok(out)
            }
        }
    }

    fn sync_freelist_header(
        &mut self,
        freelist_head: Option<PageNumber>,
        free_page_count: u32,
    ) -> anyhow::Result<()> {
        self.header.first_freelist_trunk_pn = freelist_head;
        self.header.free_page_count = free_page_count;

        let root_pn = PageNumber::try_from(1).unwrap();
        let root_bytes = self.get_page(root_pn)?.to_vec();
        self.write_page(root_pn, root_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::pager::Pager;
    use std::sync::atomic::{AtomicU64, Ordering};

    const PAGE_SIZE: usize = 512;
    /// (512 - 8) / 4 trunk leaf slots; the 127th free forces a second trunk.
    const TRUNK_CAP: usize = 126;

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
        path.push(format!("maze_freelist_test_{tag}_{unique}_{seq}.mz"));
        path.to_str().unwrap().to_string()
    }

    fn fresh_pager(tag: &str) -> (Pager, String) {
        let path = temp_db_path(tag);
        let pager = Pager::new(&path, PAGE_SIZE).unwrap();
        (pager, path)
    }

    /// Append `n` pages; on a fresh DB these are pns 2..2+n.
    fn alloc_n(pager: &mut Pager, n: usize) -> Vec<PageNumber> {
        (0..n).map(|_| pager.allocate_page().unwrap()).collect()
    }

    /// Persisted (head, count) pair: mutate + flush page 1.
    fn set_header(pager: &mut Pager, head: Option<PageNumber>, count: u32) {
        pager.header.first_freelist_trunk_pn = head;
        pager.header.free_page_count = count;
        let root = pager.get_page(pn(1)).unwrap().to_vec();
        pager.write_page(pn(1), root).unwrap();
    }

    fn write_trunk(
        pager: &mut Pager,
        at: PageNumber,
        next: Option<PageNumber>,
        free: &[PageNumber],
    ) {
        let bytes = FreeListTrunkPage {
            next,
            free: free.to_vec(),
        }
        .serialize(pager.spec(at))
        .unwrap();
        pager.write_page(at, bytes).unwrap();
    }

    fn header_raw(pager: &mut Pager) -> Vec<u8> {
        pager.get_page(pn(1)).unwrap()[..23].to_vec()
    }

    fn assert_header_synced(pager: &mut Pager) {
        let raw = header_raw(pager);
        let head = u32::from_be_bytes(raw[15..19].try_into().unwrap());
        let count = u32::from_be_bytes(raw[19..23].try_into().unwrap());
        let expect_head = pager
            .header
            .first_freelist_trunk_pn
            .map(|p| p.get())
            .unwrap_or(0);
        assert_eq!(head, expect_head, "page-1 head bytes must match header");
        assert_eq!(
            count, pager.header.free_page_count,
            "page-1 count bytes must match"
        );
    }

    #[test]
    fn empty_freelist_reports_none_zero_empty() {
        let (mut pager, path) = fresh_pager("empty");
        assert_eq!(pager.get_fp().unwrap(), None);
        assert_eq!(pager.count_fp().unwrap(), 0);
        assert!(pager.list_fp().unwrap().is_empty());
        assert_eq!(pager.header.free_page_count, 0);
        assert!(pager.header.first_freelist_trunk_pn.is_none());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn single_free_alloc_roundtrip_syncs_header_and_zeroes() {
        let (mut pager, path) = fresh_pager("single");
        let pns = alloc_n(&mut pager, 1);
        pager.free_page(pns[0]).unwrap();

        assert_eq!(pager.header.first_freelist_trunk_pn, Some(pns[0]));
        assert_eq!(pager.header.free_page_count, 1);
        assert_eq!(pager.count_fp().unwrap(), 1);
        assert_eq!(pager.list_fp().unwrap(), vec![pns[0]]);
        assert_header_synced(&mut pager);

        let got = pager.get_fp().unwrap();
        assert_eq!(got, Some(pns[0]));
        assert!(pager.header.first_freelist_trunk_pn.is_none());
        assert_eq!(pager.header.free_page_count, 0);
        assert_eq!(pager.get_page(pns[0]).unwrap(), vec![0u8; PAGE_SIZE]);
        assert_header_synced(&mut pager);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn leaf_entries_come_back_lifo() {
        let (mut pager, path) = fresh_pager("lifo");
        let pns = alloc_n(&mut pager, 3);
        for &p in &pns {
            pager.free_page(p).unwrap();
        }
        assert_eq!(pager.header.free_page_count, 3);
        let mut out = Vec::new();
        while let Some(p) = pager.get_fp().unwrap() {
            out.push(p);
        }
        assert_eq!(out, vec![pns[2], pns[1], pns[0]]);
        assert!(pager.header.first_freelist_trunk_pn.is_none());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn two_trunk_overflow_and_drain_order() {
        let (mut pager, path) = fresh_pager("twotrunk");
        // 128 frees: 1 trunk + 126 leaves + 1 new trunk.
        let total = TRUNK_CAP + 2;
        let pns = alloc_n(&mut pager, total);
        for &p in &pns {
            pager.free_page(p).unwrap();
        }

        let new_head = *pns.last().unwrap();
        let old_head = pns[0];
        assert_eq!(pager.header.first_freelist_trunk_pn, Some(new_head));
        assert_eq!(pager.header.free_page_count, total as u32);
        assert_eq!(pager.count_fp().unwrap(), total);
        assert_header_synced(&mut pager);

        let list = pager.list_fp().unwrap();
        assert_eq!(list.len(), total);
        // head trunk (empty) first, old trunk last.
        assert_eq!(list[0], new_head);
        assert_eq!(*list.last().unwrap(), old_head);
        let got_set: HashSet<PageNumber> = list.into_iter().collect();
        let want_set: HashSet<PageNumber> = pns.iter().copied().collect();
        assert_eq!(got_set, want_set);

        // Drain: new trunk itself first, then old trunk's leaves LIFO,
        // then the old trunk itself.
        let mut out = Vec::new();
        while let Some(p) = pager.get_fp().unwrap() {
            out.push(p);
        }
        let mut expected = vec![new_head];
        expected.extend(pns[1..pns.len() - 1].iter().rev().copied());
        expected.push(old_head);
        assert_eq!(out, expected);
        assert!(pager.header.first_freelist_trunk_pn.is_none());
        assert_eq!(pager.header.free_page_count, 0);
        assert_eq!(pager.count_fp().unwrap(), 0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn free_rejects_page_one_double_free_and_live_trunks() {
        let (mut pager, path) = fresh_pager("rejects");
        let pns = alloc_n(&mut pager, 2);
        assert!(pager.free_page(pn(1)).is_err());

        pager.free_page(pns[0]).unwrap();
        assert!(pager.free_page(pns[0]).is_err(), "double free must fail");

        // freeing the live head trunk itself must fail (it is in the list).
        let head = pager.header.first_freelist_trunk_pn.unwrap();
        assert!(pager.free_page(head).is_err());

        // freeing a live leaf listed in the head must fail.
        pager.free_page(pns[1]).unwrap();
        assert!(pager.free_page(pns[1]).is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn free_rejects_trunk_buried_in_two_trunk_chain() {
        let (mut pager, path) = fresh_pager("buried");
        let total = TRUNK_CAP + 2;
        let pns = alloc_n(&mut pager, total);
        for &p in &pns {
            pager.free_page(p).unwrap();
        }
        // old trunk (pns[0]) is mid-chain now: freeing it must fail.
        assert!(pager.free_page(pns[0]).is_err());
        // a leaf buried in the old trunk must fail too.
        assert!(pager.free_page(pns[1]).is_err());
        assert_eq!(pager.header.free_page_count, total as u32);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn get_with_zero_count_bails_without_mutating_state() {
        let (mut pager, path) = fresh_pager("zerocount");
        let pns = alloc_n(&mut pager, 1);
        pager.free_page(pns[0]).unwrap();
        // corrupt the count in memory only.
        pager.header.free_page_count = 0;
        assert!(pager.get_fp().unwrap_err().to_string().contains("negative"));
        assert_eq!(pager.header.first_freelist_trunk_pn, Some(pns[0]));
        assert_eq!(pager.header.free_page_count, 0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn insert_at_max_count_bails_without_mutating_state() {
        let (mut pager, path) = fresh_pager("maxcount");
        let pns = alloc_n(&mut pager, 2);
        pager.header.free_page_count = u32::MAX;
        assert!(pager.free_page(pns[0]).is_err());
        assert!(pager.header.first_freelist_trunk_pn.is_none());
        assert_eq!(pager.header.free_page_count, u32::MAX);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn returned_pages_are_zeroed_on_both_paths() {
        let (mut pager, path) = fresh_pager("zeroed");
        // leaf-pop path
        let pns = alloc_n(&mut pager, 2);
        pager.free_page(pns[0]).unwrap();
        pager.free_page(pns[1]).unwrap();
        let leaf = pager.get_fp().unwrap().unwrap();
        assert_eq!(leaf, pns[1]);
        assert_eq!(pager.get_page(leaf).unwrap(), vec![0u8; PAGE_SIZE]);

        // trunk-consume path: lone trunk returns itself zeroed.
        let (mut pager2, path2) = fresh_pager("zeroed_trunk");
        let q = alloc_n(&mut pager2, 1);
        pager2.free_page(q[0]).unwrap();
        let trunk = pager2.get_fp().unwrap().unwrap();
        assert_eq!(trunk, q[0]);
        assert_eq!(pager2.get_page(trunk).unwrap(), vec![0u8; PAGE_SIZE]);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&path2).ok();
    }

    #[test]
    fn freelist_survives_reopen_and_reuses_pages() {
        let path = temp_db_path("reopen");
        let (head, count, list) = {
            let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
            let pns = alloc_n(&mut pager, 3);
            for &p in &pns {
                pager.free_page(p).unwrap();
            }
            let snap = (
                pager.header.first_freelist_trunk_pn,
                pager.header.free_page_count,
                pager.list_fp().unwrap(),
            );
            assert_header_synced(&mut pager);
            snap
        };
        let mut pager = Pager::init(&path).unwrap();
        assert_eq!(pager.header.first_freelist_trunk_pn, head);
        assert_eq!(pager.header.free_page_count, count);
        assert_eq!(pager.list_fp().unwrap(), list);

        let size_before = std::fs::metadata(&path).unwrap().len();
        let reused = pager.allocate_page().unwrap();
        assert!(list.contains(&reused));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size_before);
        drop(pager);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn leaf_entry_of_page_one_is_rejected_everywhere() {
        let (mut pager, path) = fresh_pager("pone_leaf");
        let pns = alloc_n(&mut pager, 2);
        write_trunk(&mut pager, pns[0], None, &[pn(1)]);
        set_header(&mut pager, Some(pns[0]), 2);

        assert!(pager.count_fp().is_err());
        assert!(pager.list_fp().is_err());
        // get_fp must bail before persisting anything.
        assert!(pager.get_fp().is_err());
        assert_eq!(pager.header.first_freelist_trunk_pn, Some(pns[0]));
        assert_eq!(pager.header.free_page_count, 2);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn next_pointer_of_page_one_is_rejected_everywhere() {
        let (mut pager, path) = fresh_pager("pone_next");
        let pns = alloc_n(&mut pager, 2);
        write_trunk(&mut pager, pns[0], Some(pn(1)), &[]);
        set_header(&mut pager, Some(pns[0]), 1);

        assert!(pager.count_fp().is_err());
        assert!(pager.list_fp().is_err());
        assert!(pager.get_fp().is_err());
        assert_eq!(pager.header.first_freelist_trunk_pn, Some(pns[0]));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn trunk_cycles_terminate_with_errors() {
        for tag in ["cycle2", "selfloop"] {
            let (mut pager, path) = fresh_pager(tag);
            let pns = alloc_n(&mut pager, 2);
            if tag == "cycle2" {
                write_trunk(&mut pager, pns[0], Some(pns[1]), &[]);
                write_trunk(&mut pager, pns[1], Some(pns[0]), &[]);
            } else {
                write_trunk(&mut pager, pns[0], Some(pns[0]), &[]);
            }
            set_header(&mut pager, Some(pns[0]), 2);
            let c_err = pager.count_fp().unwrap_err().to_string();
            assert!(c_err.contains("cycle"), "unexpected: {c_err}");
            let l_err = pager.list_fp().unwrap_err().to_string();
            assert!(l_err.contains("cycle"), "unexpected: {l_err}");
            drop(pager);
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn truncated_trunk_count_propagates_errors() {
        let (mut pager, path) = fresh_pager("truncated");
        let pns = alloc_n(&mut pager, 1);
        let mut buf = vec![0u8; PAGE_SIZE];
        buf[4..8].copy_from_slice(&1000u32.to_be_bytes());
        pager.write_page(pns[0], buf).unwrap();
        set_header(&mut pager, Some(pns[0]), 1);

        assert!(pager.count_fp().is_err());
        assert!(pager.list_fp().is_err());
        assert!(pager.get_fp().is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn head_past_eof_propagates_errors() {
        let (mut pager, path) = fresh_pager("pasteof");
        set_header(&mut pager, Some(pn(9999)), 1);
        assert!(pager.count_fp().is_err());
        assert!(pager.list_fp().is_err());
        assert!(pager.get_fp().is_err());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn init_rejects_inconsistent_headers() {
        use std::fs::OpenOptions;
        use std::io::{Seek, SeekFrom, Write};

        fn patch(path: &str, offset: u64, v: u32) {
            let mut f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            f.seek(SeekFrom::Start(offset)).unwrap();
            f.write_all(&v.to_be_bytes()).unwrap();
            f.sync_all().unwrap();
        }

        // None head with nonzero count.
        let (pager, path) = fresh_pager("hdr_none_count");
        drop(pager);
        patch(&path, 19, 1);
        assert!(Pager::init(&path).is_err());

        // Some head with zero count.
        let (mut pager, path2) = fresh_pager("hdr_some_zero");
        let pns = alloc_n(&mut pager, 1);
        pager.free_page(pns[0]).unwrap();
        drop(pager);
        patch(&path2, 19, 0);
        assert!(Pager::init(&path2).is_err());

        // head == page 1 (with nonzero count to reach the head check).
        let (pager3, path3) = fresh_pager("hdr_head_one");
        drop(pager3);
        patch(&path3, 15, 1);
        patch(&path3, 19, 1);
        assert!(Pager::init(&path3).is_err());

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&path2).ok();
        std::fs::remove_file(&path3).ok();
    }
}

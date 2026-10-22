pub mod freelist;
pub mod overflow;

use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{self, ErrorKind, Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::storage::{
    header::{MAX_PAGE_COUNT, MAZE_HEADER_SIZE, MazeHeader},
    pages::{PageType, table::leaf::TableLeafCell, spec::PageSpec, types::PageNumber},
};

pub struct Pager {
    pub header: MazeHeader,
    pub page_size: usize, // [2^9 to 2^16]

    file: File,
    cache: HashMap<PageNumber, Vec<u8>>,
    free_cache: HashSet<PageNumber>,
    dirty: HashSet<u32>,
}

impl Pager {
    // for .mz files that have already been created.
    pub fn init(path: &str) -> anyhow::Result<Self> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        file.seek(SeekFrom::Start(0))?;

        let mut maze_header_bytes = Vec::with_capacity(MAZE_HEADER_SIZE);
        Read::by_ref(&mut file)
            .take(MAZE_HEADER_SIZE as u64)
            .read_to_end(&mut maze_header_bytes)?;
        let maze_header = MazeHeader::try_from(maze_header_bytes.as_slice())?;
        let page_size = 2u32.pow(maze_header.page_size_exp.get() as u32) as usize;

        if maze_header.first_freelist_trunk_pn.is_none() && maze_header.free_page_count > 0 {
            anyhow::bail!(
                "how can first freelist trunk page be null and count of free pages be more than 0?"
            )
        }
        if maze_header.first_freelist_trunk_pn.is_some() && maze_header.free_page_count == 0 {
            anyhow::bail!(
                "how can first freelist trunk page be present but count of free pages be 0?"
            )
        }
        if let Some(head_pn) = maze_header.first_freelist_trunk_pn
            && head_pn == 1
        {
            anyhow::bail!("first page of the database cannot be a freelist trunk page")
        }

        let mut pager = Self {
            header: maze_header,
            file,
            page_size,
            cache: HashMap::new(),
            free_cache: HashSet::new(),
            dirty: HashSet::new(),
        };

        pager.free_cache = pager.list_fp()?.into_iter().collect();

        Ok(pager)
    }

    // for new database creation -> writes header bytes along with an empty table leaf page in page 1.
    pub fn new(path: &str, page_size: usize) -> anyhow::Result<Self> {
        let create_new_file = true;
        let mut file = open_rw(path, create_new_file)?;

        let header = MazeHeader::new(page_size)?;
        let pn1 = PageNumber::try_from(1)?;
        let ps = PageSpec::new(page_size, pn1);
        let no_rightmost_child = None;
        let no_cells = Vec::<TableLeafCell>::new();

        // `PageType::serialize` already reserves the leading
        // `MAZE_HEADER_SIZE` bytes for page 1 (left zeroed) -- this is the
        // whole page_size-length page 1 buffer, not just its own page header.
        let mut page1 = PageType::TableLeaf.serialize(&no_cells, no_rightmost_child, ps)?;
        page1[0..MAZE_HEADER_SIZE].copy_from_slice(&header.serialize());

        write_page_to_disk(&mut file, page_size, pn1, &page1)?;

        Ok(Self {
            header,
            file,
            page_size,
            cache: HashMap::new(),
            free_cache: HashSet::new(),
            dirty: HashSet::new(),
        })
    }

    pub fn spec(&self, pn: PageNumber) -> PageSpec {
        PageSpec::new(self.page_size, pn)
    }

    pub fn get_page(&mut self, page_number: PageNumber) -> anyhow::Result<&[u8]> {
        use std::collections::hash_map::Entry;
        match self.cache.entry(page_number) {
            Entry::Occupied(oe) => Ok(oe.into_mut().as_slice()),
            Entry::Vacant(ve) => {
                let bytes = read_page_from_disk(&mut self.file, self.page_size, page_number)?;
                Ok(ve.insert(bytes).as_slice())
            }
        }
    }

    pub fn get_page_mut(&mut self, page_number: PageNumber) -> anyhow::Result<&mut [u8]> {
        use std::collections::hash_map::Entry;
        match self.cache.entry(page_number) {
            Entry::Occupied(oe) => Ok(oe.into_mut()),
            Entry::Vacant(ve) => {
                let bytes = read_page_from_disk(&mut self.file, self.page_size, page_number)?;
                Ok(ve.insert(bytes))
            }
        }
    }

    /// Assumes that the bytes for Maze header were accounted for in `bytes`, as this method will
    /// write the header to bytes[0..MAZE_HEADER_SIZE]
    pub fn write_page(
        &mut self,
        page_number: PageNumber,
        mut bytes: Vec<u8>,
    ) -> anyhow::Result<()> {
        if page_number == 1 {
            bytes[0..MAZE_HEADER_SIZE].copy_from_slice(&self.header.serialize());
        }

        write_page_to_disk(&mut self.file, self.page_size, page_number, &bytes)?;
        self.cache.insert(page_number, bytes);
        Ok(())
    }

    /// If a free page is available, initializes it to all zeros, returns its page number.
    /// Else allocates a new page, initializes it to all zeros and returns its page number.
    pub fn allocate_page(&mut self) -> anyhow::Result<PageNumber> {
        match self.get_fp()? {
            Some(free_pn) => {
                self.free_cache.remove(&free_pn);
                Ok(free_pn)
            }
            None => {
                let new_pn = PageNumber::try_from(u32::try_from(
                    self.file
                        .metadata()?
                        .len()
                        .checked_div(self.page_size as u64)
                        .ok_or_else(|| anyhow::anyhow!("can't divide file size by page size"))?
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("can't add 1 to filesize/pagesize"))?,
                )?)?;
                if new_pn.get() > MAX_PAGE_COUNT {
                    anyhow::bail!(
                        "sorry bro database size limit reached make a new database holy shit tf are you doing with 281 TB OF DATA????"
                    )
                }
                self.write_page(new_pn, vec![0u8; self.page_size])?;
                Ok(new_pn)
            }
        }
    }

    /// Adds a page to the freelist.
    pub fn free_page(&mut self, freed_pn: PageNumber) -> anyhow::Result<()> {
        if freed_pn == 1
            || freed_pn.get() > MAX_PAGE_COUNT
            || (freed_pn.get() as u64) > (self.file.metadata()?.len() / self.page_size as u64)
        {
            anyhow::bail!("invalid page number")
        }
        self.file.sync_all()?;
        if self.free_cache.contains(&freed_pn) {
            anyhow::bail!("attempted double free: cannot free an already freed page")
        }
        self.insert_fp(freed_pn)?;
        self.free_cache.insert(freed_pn);
        Ok(())
    }
}

fn read_page_from_disk(
    file: &mut File,
    page_size: usize,
    page_number: PageNumber,
) -> anyhow::Result<Vec<u8>> {
    let file_offset = ((page_number.get() - 1) as u64) * page_size as u64;
    file.seek(SeekFrom::Start(file_offset))?;
    let mut buf = vec![0u8; page_size];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_page_to_disk(
    file: &mut File,
    page_size: usize,
    page_number: PageNumber,
    data: &[u8],
) -> anyhow::Result<()> {
    let file_offset = ((page_number.get() - 1) as u64) * page_size as u64;
    file.seek(SeekFrom::Start(file_offset))?;
    file.write_all(data)?;
    Ok(())
}

fn open_rw(path: &str, create: bool) -> std::io::Result<File> {
    if create {
        let path = Path::new(path);
        if path.exists() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "File already exists",
            ));
        }

        return OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path);
    }
    OpenOptions::new().read(true).write(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::pages::Page;

    fn temp_db_path(tag: &str) -> String {
        let mut path = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("maze_pager_test_{tag}_{unique}.mz"));
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn new_creates_a_file_with_a_valid_empty_root_leaf() {
        let path = temp_db_path("new_creates_valid_root");
        let mut pager = Pager::new(&path, 512).unwrap();

        let pn1 = PageNumber::try_from(1).unwrap();
        let ps = PageSpec::new(512, pn1);
        let bytes = pager.get_page(pn1).unwrap().to_vec();
        match Page::parse(&bytes, ps).unwrap() {
            Page::TableLeaf(lp) => assert!(lp.cells.is_empty()),
            _ => panic!("expected an empty leaf page as the fresh root"),
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn init_reopens_a_file_created_by_new() {
        let path = temp_db_path("init_reopens");
        {
            let _pager = Pager::new(&path, 512).unwrap();
        } // dropped here, closing the file

        let mut reopened = Pager::init(&path).unwrap();
        assert_eq!(reopened.page_size, 512);

        let pn1 = PageNumber::try_from(1).unwrap();
        let ps = PageSpec::new(512, pn1);
        let bytes = reopened.get_page(pn1).unwrap().to_vec();
        match Page::parse(&bytes, ps).unwrap() {
            Page::TableLeaf(lp) => assert!(lp.cells.is_empty()),
            _ => panic!("expected a leaf page"),
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn get_page_errors_past_end_of_file() {
        let path = temp_db_path("get_page_oob");
        let mut pager = Pager::new(&path, 512).unwrap();

        let far_page = PageNumber::try_from(999).unwrap();
        assert!(pager.get_page(far_page).is_err());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn get_page_returns_consistent_bytes_on_repeated_reads() {
        let path = temp_db_path("get_page_caches");
        let mut pager = Pager::new(&path, 512).unwrap();
        let pn1 = PageNumber::try_from(1).unwrap();

        let first = pager.get_page(pn1).unwrap().to_vec();
        let second = pager.get_page(pn1).unwrap().to_vec();
        assert_eq!(first, second);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn free_page_rejects_page_one_huge_and_past_eof() {
        let path = temp_db_path("free_validation");
        let mut pager = Pager::new(&path, 512).unwrap();

        assert!(pager.free_page(PageNumber::try_from(1).unwrap()).is_err());
        // above MAX_PAGE_COUNT without needing a huge file
        assert!(
            pager
                .free_page(PageNumber::try_from(u32::MAX).unwrap())
                .is_err()
        );
        // past end of a 1-page file
        assert!(pager.free_page(PageNumber::try_from(50).unwrap()).is_err());
        // double free fails the second time
        let p2 = pager.allocate_page().unwrap();
        pager.free_page(p2).unwrap();
        assert!(pager.free_page(p2).is_err());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn allocate_appends_zeroed_pages_when_freelist_empty() {
        let path = temp_db_path("alloc_append");
        let mut pager = Pager::new(&path, 512).unwrap();

        let p2 = pager.allocate_page().unwrap();
        assert_eq!(p2, PageNumber::try_from(2).unwrap());
        let p3 = pager.allocate_page().unwrap();
        assert_eq!(p3, PageNumber::try_from(3).unwrap());
        assert_eq!(pager.get_page(p2).unwrap(), vec![0u8; 512]);
        assert_eq!(pager.get_page(p3).unwrap(), vec![0u8; 512]);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 3 * 512);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn allocate_reuses_freed_pages_lifo_without_growing_file() {
        let path = temp_db_path("alloc_reuse");
        let mut pager = Pager::new(&path, 512).unwrap();
        let p2 = pager.allocate_page().unwrap();
        let p3 = pager.allocate_page().unwrap();
        let p4 = pager.allocate_page().unwrap();
        pager.free_page(p2).unwrap();
        pager.free_page(p3).unwrap();
        pager.free_page(p4).unwrap();

        let size_before = std::fs::metadata(&path).unwrap().len();
        // freelist head holds p4's trunk with [p2?..] — LIFO pops p4's leaves first.
        // p2 became the trunk, p3/p4 are its leaves: pop order p4, p3, then trunk p2.
        assert_eq!(pager.allocate_page().unwrap(), p4);
        assert_eq!(pager.allocate_page().unwrap(), p3);
        assert_eq!(pager.allocate_page().unwrap(), p2);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size_before);
        // every reused page comes back zeroed
        assert_eq!(pager.get_page(p2).unwrap(), vec![0u8; 512]);
        // freelist is drained: next allocate grows the file again
        let p5 = pager.allocate_page().unwrap();
        assert_eq!(p5, PageNumber::try_from(5).unwrap());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size_before + 512);

        std::fs::remove_file(&path).ok();
    }
}

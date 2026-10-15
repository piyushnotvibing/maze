use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{self, ErrorKind, Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::file::{
    header::{MAZE_HEADER_SIZE, MazeHeader},
    pages::{PageType, leaf::TableLeafCell, spec::PageSpec, types::PageNumber},
};

pub struct Pager {
    pub header: MazeHeader,
    pub page_size: usize, // [2^9 to 2^16]
    pub leaked: Vec<PageNumber>,
    file: File,
    cache: HashMap<PageNumber, Vec<u8>>,
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

        Ok(Self {
            header: maze_header,
            file,
            page_size,
            leaked: Vec::new(),
            cache: HashMap::new(),
            dirty: HashSet::new(),
        })
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
            leaked: Vec::new(),
            cache: HashMap::new(),
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

    /// Creates a page buffer, writes it to disk and cache, returns mutable reference of the page buffer in cache.
    pub fn allocate_page(&mut self) -> anyhow::Result<PageNumber> {
        let new_pn = PageNumber::try_from(u32::try_from(
            self.file
                .metadata()?
                .len()
                .checked_div(self.page_size as u64)
                .ok_or_else(|| anyhow::anyhow!("can't divide file size by page size"))?
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("can't add 1 to filesize/pagesize"))?,
        )?)?;
        self.write_page(new_pn, vec![0u8; self.page_size])?;
        Ok(new_pn)
    }

    /// Adds a page to the leaked list (for now).
    pub fn free_page(&mut self, freed_pn: PageNumber) {
        self.leaked.push(freed_pn);
    }

    /// Returns an immutable reference to the list of leaked pages.
    pub fn leaked_pages(&self) -> &[PageNumber] {
        &self.leaked
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
    use crate::file::pages::Page;

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
            Page::TableInterior(_) => panic!("expected an empty leaf page as the fresh root"),
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
            Page::TableInterior(_) => panic!("expected a leaf page"),
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
}

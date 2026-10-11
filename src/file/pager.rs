use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
};

use crate::file::{
    header::{MAZE_HEADER_SIZE, MazeHeader},
    pages::{PageType, leaf::TableLeafCell, types::PageNumber},
};

pub struct Pager {
    file: File,
    page_size: u32, // [2^9 to 2^16]
    cache: HashMap<PageNumber, Vec<u8>>,
    dirty: HashSet<u32>,
}

impl Pager {
    // for .mz files that have already been created.
    pub fn init(path: &str) -> anyhow::Result<Self> {
        // Deliberately does NOT create the file: opening a path that doesn't
        // exist yet is a real error here, not "start a new empty database" --
        // that's what `Pager::new` is for.
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        file.seek(SeekFrom::Start(0))?;

        let mut maze_header_bytes = Vec::with_capacity(MAZE_HEADER_SIZE);
        Read::by_ref(&mut file)
            .take(MAZE_HEADER_SIZE as u64)
            .read_to_end(&mut maze_header_bytes)?;
        let maze_header = MazeHeader::try_from(maze_header_bytes.as_slice())?;
        let page_size = 2u32.pow(maze_header.page_size_exp.get() as u32);

        Ok(Self {
            file,
            page_size,
            cache: HashMap::new(),
            dirty: HashSet::new(),
        })
    }

    // for new database creation -> writes header bytes along with an empty table leaf page in page 1.
    pub fn new(path: &str, page_size: u32) -> anyhow::Result<Self> {
        let mut file = open_rw(path)?;
        file.seek(SeekFrom::Start(0))?;

        let pn = PageNumber::try_from(1)?;
        let no_rightmost_child = None;
        let no_cells = Vec::<TableLeafCell>::new();

        // `PageType::serialize` already reserves the leading
        // `MAZE_HEADER_SIZE` bytes for page 1 (left zeroed) -- this *is* the
        // whole page_size-length page 1 buffer, not just its own page header.
        let mut page1 =
            PageType::TableLeaf.serialize(pn, no_rightmost_child, &no_cells, page_size as usize)?;
        let header = MazeHeader::new(page_size)?;
        page1[0..MAZE_HEADER_SIZE].copy_from_slice(&header.serialize());

        file.write_all(&page1)?;

        Ok(Self {
            file,
            page_size,
            cache: HashMap::new(),
            dirty: HashSet::new(),
        })
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

    pub fn write_page(&mut self, page_number: PageNumber, bytes: Vec<u8>) -> anyhow::Result<()> {
        write_page_to_disk(&mut self.file, self.page_size, page_number, &bytes)?;
        self.cache.insert(page_number, bytes);
        Ok(())
    }

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
        self.write_page(new_pn, vec![0u8; self.page_size as usize])?;
        Ok(new_pn)
    }
}

fn read_page_from_disk(
    file: &mut File,
    page_size: u32,
    page_number: PageNumber,
) -> anyhow::Result<Vec<u8>> {
    let file_offset = (page_number.get() - 1) * page_size;
    file.seek(SeekFrom::Start(file_offset as u64))?;
    let mut buf = vec![0u8; page_size as usize];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

fn write_page_to_disk(
    file: &mut File,
    page_size: u32,
    page_number: PageNumber,
    data: &[u8],
) -> anyhow::Result<()> {
    let file_offset = (page_number.get() - 1) * page_size;
    file.seek(SeekFrom::Start(file_offset as u64))?;
    file.write_all(data)?;
    Ok(())
}

// Used only by `Pager::new`: creates the file if it's missing, and starts
// from a clean slate (truncates) if a file already existed at this path.
fn open_rw(path: &str) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(path)
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
        let bytes = pager.get_page(pn1).unwrap().to_vec();
        match Page::parse(&bytes, pn1).unwrap() {
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
        let bytes = reopened.get_page(pn1).unwrap().to_vec();
        match Page::parse(&bytes, pn1).unwrap() {
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

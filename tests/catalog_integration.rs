//! Integration tests for the `Catalog` layer together with the
//! storage/btree layers it sits on top of. Unlike `tests/table_and_index.rs`
//! (which roots its trees at hardcoded page numbers), everything here goes
//! through the public `Catalog` API -- `create_table`/`create_index` do the
//! page allocation -- so these tests exercise the actual intended usage
//! pattern: multiple tables and indexes coexisting, query correctness
//! against data reached only through catalog-reported root pages, page
//! reuse after a drop, and a real file close/reopen with overflowing rows.

use std::collections::HashMap;

use maze::btree::{
    check::{check_index_tree, check_table_tree},
    index_cursor::IndexCursor,
    table_cursor::TableCursor,
};
use maze::catalog::{Catalog, Column};
use maze::storage::{
    pager::Pager,
    pages::types::RowID,
    record::{
        Record,
        values::{DataType, Value},
    },
};

fn rid(n: u64) -> RowID {
    RowID::try_from(n).unwrap()
}

fn temp_db_path(tag: &str) -> String {
    let mut path = std::env::temp_dir();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!("maze_catalog_integration_test_{tag}_{unique}.mz"));
    path.to_str().unwrap().to_string()
}

fn empty_catalog() -> Catalog {
    Catalog {
        tables: HashMap::new(),
        indexes: HashMap::new(),
    }
}

fn shuffled(n: u64) -> Vec<u64> {
    let mut v: Vec<u64> = (1..=n).collect();
    let mut x = 0x2545F4914F6CDD1Du64;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
    v
}

/// Decodes a `(id, salary)` row and pulls out `salary`.
fn salary_of(payload: &[u8]) -> i64 {
    let (values, _) = Record::decode(payload).unwrap();
    match values.as_slice() {
        [Value::Int(_), Value::Int(s)] => *s,
        other => panic!("unexpected row shape: {other:?}"),
    }
}

/// Create two unrelated tables and a non-unique index on one of them,
/// entirely through the `Catalog` API. Maintain the index by hand on every
/// insert (there's no automatic maintenance wiring yet), then check that an
/// index-based equality lookup matches both a hand-kept model and a real
/// full table scan + filter -- and that the unrelated second table is left
/// completely untouched.
#[test]
fn catalog_driven_table_and_index_match_a_full_scan() {
    const PAGE_SIZE: usize = 512;
    let path = temp_db_path("table_and_index");
    let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
    let mut catalog = empty_catalog();

    let id_col = Column {
        name: "id".to_string(),
        c_type: DataType::Int,
    };
    let salary_col = Column {
        name: "salary".to_string(),
        c_type: DataType::Int,
    };

    catalog
        .create_table("employees", vec![id_col.clone(), salary_col.clone()], &mut pager)
        .unwrap();
    catalog
        .create_table("departments", vec![id_col], &mut pager)
        .unwrap();
    catalog
        .create_index("idx_salary", "employees", vec![salary_col], false, &mut pager)
        .unwrap();

    let employees_root = catalog.tables.get("employees").unwrap().root_pn;
    let salary_index_root = catalog.indexes.get("idx_salary").unwrap().root_pn;

    let n = 2000u64;
    let mut model: Vec<(RowID, i64)> = Vec::with_capacity(n as usize);

    for i in shuffled(n) {
        let salary = (i % 300) as i64;
        let payload = Record::new(vec![Value::Int(i as i64), Value::Int(salary)])
            .unwrap()
            .encode();

        {
            let mut table = TableCursor::new(&mut pager, employees_root);
            table.insert(rid(i), &payload).unwrap();
        }
        {
            let mut index = IndexCursor::new(&mut pager, salary_index_root, false);
            index.insert(vec![Value::Int(salary)], rid(i)).unwrap();
        }

        model.push((rid(i), salary));
    }

    let table_stats = check_table_tree(&mut pager, employees_root).unwrap();
    assert_eq!(table_stats.rows, n as usize);
    let index_stats = check_index_tree(&mut pager, salary_index_root).unwrap();
    assert_eq!(index_stats.rows, n as usize);

    let target = 77i64;
    let mut want: Vec<u64> = model
        .iter()
        .filter(|(_, s)| *s == target)
        .map(|(r, _)| r.get())
        .collect();
    want.sort();

    let mut got: Vec<u64> = {
        let mut index = IndexCursor::new(&mut pager, salary_index_root, false);
        index
            .range_scan(&[Value::Int(target)], &[Value::Int(target)])
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect()
    };
    got.sort();
    assert_eq!(
        got, want,
        "index-based lookup through the catalog's own root page must match the reference model"
    );

    let mut scanned: Vec<u64> = {
        let mut table = TableCursor::new(&mut pager, employees_root);
        table
            .scan()
            .unwrap()
            .into_iter()
            .filter(|cell| salary_of(&cell.local_payload) == target)
            .map(|cell| cell.row_id.get())
            .collect()
    };
    scanned.sort();
    assert_eq!(
        got, scanned,
        "index-based lookup must match a real full table scan + filter"
    );

    // the unrelated table must be completely untouched by any of this.
    let departments_root = catalog.tables.get("departments").unwrap().root_pn;
    let mut departments = TableCursor::new(&mut pager, departments_root);
    assert!(departments.scan().unwrap().is_empty());

    std::fs::remove_file(&path).ok();
}

/// Dropping a table should return its pages to the pager's freelist, not
/// just remove its catalog entry. Build a table large enough to span several
/// pages, drop it, then build a second table of comparable size and check
/// that the file didn't grow by roughly another whole table's worth of
/// space -- i.e. the freed pages actually got reused.
#[test]
fn dropping_a_table_frees_its_pages_for_reuse_by_a_later_create() {
    const PAGE_SIZE: usize = 512;
    let path = temp_db_path("drop_reuses_pages");
    let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
    let mut catalog = empty_catalog();

    let id_col = Column {
        name: "id".to_string(),
        c_type: DataType::Int,
    };

    catalog
        .create_table("big_table", vec![id_col.clone()], &mut pager)
        .unwrap();
    let big_root = catalog.tables.get("big_table").unwrap().root_pn;

    for i in 1..=2000u64 {
        let payload = Record::new(vec![Value::Int(i as i64)]).unwrap().encode();
        let mut table = TableCursor::new(&mut pager, big_root);
        table.insert(rid(i), &payload).unwrap();
    }

    let stats_before_drop = check_table_tree(&mut pager, big_root).unwrap();
    assert!(
        stats_before_drop.pages.len() > 1,
        "table should have grown beyond a single page"
    );

    let file_size_before_drop = std::fs::metadata(&path).unwrap().len();

    catalog.drop_table("big_table", &mut pager).unwrap();

    // dropping frees pages, it doesn't truncate the file.
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        file_size_before_drop,
        "drop_table should free pages for reuse, not shrink the file"
    );

    // a second table of the same size right after the drop should mostly
    // reuse the freed pages rather than growing the file by roughly another
    // whole table's worth of space.
    catalog
        .create_table("second_table", vec![id_col], &mut pager)
        .unwrap();
    let second_root = catalog.tables.get("second_table").unwrap().root_pn;
    for i in 1..=2000u64 {
        let payload = Record::new(vec![Value::Int(i as i64)]).unwrap().encode();
        let mut table = TableCursor::new(&mut pager, second_root);
        table.insert(rid(i), &payload).unwrap();
    }

    let file_size_after_second_table = std::fs::metadata(&path).unwrap().len();
    let tolerance = PAGE_SIZE as u64 * 8;
    assert!(
        file_size_after_second_table <= file_size_before_drop + tolerance,
        "a same-sized table created right after a drop should reuse freed pages \
         instead of growing the file by roughly its own size again \
         (before drop: {file_size_before_drop}, after second table: {file_size_after_second_table})"
    );

    std::fs::remove_file(&path).ok();
}

/// Create two tables and an index through the catalog, with rows large
/// enough to force overflow pages, then actually close the file (drop the
/// `Pager`) and reopen it fresh. The reloaded catalog's schema, the
/// overflowing row data, and the index must all still be intact and
/// structurally sound.
#[test]
fn close_and_reopen_preserves_overflowing_rows_across_two_tables_and_an_index() {
    const PAGE_SIZE: usize = 512;
    let path = temp_db_path("reopen_with_overflow");
    let big_bio = "x".repeat(PAGE_SIZE * 2);

    {
        let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        let id_col = Column {
            name: "id".to_string(),
            c_type: DataType::Int,
        };
        let bio_col = Column {
            name: "bio".to_string(),
            c_type: DataType::Text(0),
        };

        catalog
            .create_table("profiles", vec![id_col.clone(), bio_col], &mut pager)
            .unwrap();
        catalog.create_table("tags", vec![id_col.clone()], &mut pager).unwrap();
        catalog
            .create_index("idx_profiles_id", "profiles", vec![id_col], false, &mut pager)
            .unwrap();

        let profiles_root = catalog.tables.get("profiles").unwrap().root_pn;
        let index_root = catalog.indexes.get("idx_profiles_id").unwrap().root_pn;

        for i in 1..=5u64 {
            let payload = Record::new(vec![Value::Int(i as i64), Value::Text(big_bio.clone())])
                .unwrap()
                .encode();
            {
                let mut table = TableCursor::new(&mut pager, profiles_root);
                table.insert(rid(i), &payload).unwrap();
            }
            {
                let mut index = IndexCursor::new(&mut pager, index_root, false);
                index.insert(vec![Value::Int(i as i64)], rid(i)).unwrap();
            }
        }

        // `pager` drops here, closing the file for real.
    }

    let mut pager = Pager::init(&path).unwrap();
    let catalog = Catalog::load(&mut pager).unwrap();

    assert_eq!(catalog.tables.len(), 2);
    assert_eq!(catalog.indexes.len(), 1);

    let profiles_root = catalog.tables.get("profiles").unwrap().root_pn;
    let index_root = catalog.indexes.get("idx_profiles_id").unwrap().root_pn;

    let table_stats = check_table_tree(&mut pager, profiles_root).unwrap();
    assert_eq!(table_stats.rows, 5);
    let index_stats = check_index_tree(&mut pager, index_root).unwrap();
    assert_eq!(index_stats.rows, 5);

    let mut table = TableCursor::new(&mut pager, profiles_root);
    let rows = table.scan().unwrap();
    assert_eq!(rows.len(), 5);
    for cell in &rows {
        let (values, _) = Record::decode(&cell.local_payload).unwrap();
        match values.as_slice() {
            [Value::Int(_), Value::Text(bio)] => {
                assert_eq!(bio, &big_bio, "overflowing text must survive a real close+reopen")
            }
            other => panic!("unexpected row shape: {other:?}"),
        }
    }

    let mut index = IndexCursor::new(&mut pager, index_root, false);
    assert_eq!(index.scan().unwrap().len(), 5);

    std::fs::remove_file(&path).ok();
}

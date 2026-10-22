//! Integration test for Subproject 8's definition of done: build a table
//! with thousands of rows, maintain a secondary index on one column by
//! hand (the "manual call from test code" the plan allows before
//! Subproject 11 wires index maintenance up automatically), and verify
//! that an index-based lookup -- both the equality and range case --
//! returns exactly the same rows a linear scan + filter over the table
//! would.

use maze::btree::{
    check::{check_index_tree, check_table_tree},
    index_cursor::IndexCursor,
    table_cursor::TableCursor,
};
use maze::storage::{
    pager::Pager,
    pages::{
        Page,
        index::leaf::IndexLeafPage,
        types::{PageNumber, RowID},
    },
    record::{Record, values::Value},
};

fn pn(n: u32) -> PageNumber {
    PageNumber::try_from(n).unwrap()
}

fn rid(n: u64) -> RowID {
    RowID::try_from(n).unwrap()
}

fn temp_db_path(tag: &str) -> String {
    let mut path = std::env::temp_dir();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!("maze_table_and_index_test_{tag}_{unique}.mz"));
    path.to_str().unwrap().to_string()
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

/// Decodes a row's payload and pulls out its `salary` column (the second of
/// the two columns every row has: `(id, salary)`).
fn salary_of(payload: &[u8]) -> i64 {
    let (values, _) = Record::decode(payload).unwrap();
    match values.as_slice() {
        [Value::Int(_), Value::Int(s)] => *s,
        other => panic!("unexpected row shape: {other:?}"),
    }
}

#[test]
fn index_lookup_matches_a_full_table_scan_and_filter() {
    const PAGE_SIZE: usize = 512;
    let path = temp_db_path("eq_and_range");
    let mut pager = Pager::new(&path, PAGE_SIZE).unwrap();

    // The table root is page 1 (`Pager::new`'s convention). The index root
    // can't also be page 1 -- that page is reserved for the schema table --
    // so it gets a freshly allocated page instead, exactly like
    // `index_cursor`'s own tests root their trees at page 2.
    let index_root = pager.allocate_page().unwrap();
    {
        let spec = pager.spec(index_root);
        let bytes = Page::IndexLeaf(IndexLeafPage { cells: vec![] })
            .serialize(spec)
            .unwrap();
        pager.write_page(index_root, bytes).unwrap();
    }

    // 3000 rows, each `(id, salary)` with `salary = id % 500`: every salary
    // value has exactly 6 rows, enough duplicates to force the index to
    // actually use the row_id tiebreaker (and, for a range query, to span
    // more than one leaf).
    let n = 3000u64;
    let mut model: Vec<(RowID, i64)> = Vec::with_capacity(n as usize);

    for i in shuffled(n) {
        let salary = (i % 500) as i64;
        let payload = Record::new(vec![Value::Int(i as i64), Value::Int(salary)])
            .unwrap()
            .encode();

        // Every table insert is paired with a manual index insert -- there's
        // no automatic maintenance wiring yet (that's Subproject 11's job).
        {
            let mut table = TableCursor::new(&mut pager, pn(1));
            table.insert(rid(i), &payload).unwrap();
        }
        {
            let mut index = IndexCursor::new(&mut pager, index_root, false);
            index.insert(vec![Value::Int(salary)], rid(i)).unwrap();
        }

        model.push((rid(i), salary));
    }

    let table_stats = check_table_tree(&mut pager, pn(1)).unwrap();
    assert_eq!(table_stats.rows, n as usize);
    let index_stats = check_index_tree(&mut pager, index_root).unwrap();
    assert_eq!(index_stats.rows, n as usize);

    // --- equality lookup: salary == 123 -----------------------------------
    let target = 123i64;

    let mut want: Vec<u64> = model
        .iter()
        .filter(|(_, s)| *s == target)
        .map(|(r, _)| r.get())
        .collect();
    want.sort();
    assert_eq!(
        want.len(),
        6,
        "every salary value should have exactly 6 rows"
    );

    let mut got: Vec<u64> = {
        let mut index = IndexCursor::new(&mut pager, index_root, false);
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
        "index-based equality lookup must match the reference model"
    );

    let mut scanned_want: Vec<u64> = {
        let mut table = TableCursor::new(&mut pager, pn(1));
        table
            .scan()
            .unwrap()
            .into_iter()
            .filter(|cell| salary_of(&cell.local_payload) == target)
            .map(|cell| cell.row_id.get())
            .collect()
    };
    scanned_want.sort();
    assert_eq!(
        got, scanned_want,
        "index-based equality lookup must match a real full table scan + filter"
    );

    // the index only has to touch the 6 matching entries, not all 3000 rows.
    assert!(got.len() * 100 < n as usize);

    // --- range lookup: 100 <= salary <= 200 --------------------------------
    let (lo, hi) = (100i64, 200i64);

    let mut want: Vec<u64> = model
        .iter()
        .filter(|(_, s)| *s >= lo && *s <= hi)
        .map(|(r, _)| r.get())
        .collect();
    want.sort();
    assert_eq!(want.len(), 6 * (hi - lo + 1) as usize);

    let mut got: Vec<u64> = {
        let mut index = IndexCursor::new(&mut pager, index_root, false);
        index
            .range_scan(&[Value::Int(lo)], &[Value::Int(hi)])
            .unwrap()
            .into_iter()
            .map(|c| c.row_id.get())
            .collect()
    };
    got.sort();
    assert_eq!(
        got, want,
        "index-based range lookup must match the reference model"
    );

    let mut scanned_want: Vec<u64> = {
        let mut table = TableCursor::new(&mut pager, pn(1));
        table
            .scan()
            .unwrap()
            .into_iter()
            .filter(|cell| {
                let s = salary_of(&cell.local_payload);
                s >= lo && s <= hi
            })
            .map(|cell| cell.row_id.get())
            .collect()
    };
    scanned_want.sort();
    assert_eq!(
        got, scanned_want,
        "index-based range lookup must match a real full table scan + filter"
    );

    std::fs::remove_file(&path).ok();
}

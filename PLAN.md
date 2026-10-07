# Maze — Build Plan

A toy SQLite-style database engine in Rust. The goal of this project is not
to ship anything — it's to learn Rust and, more importantly, the core
principles of database management systems: page-based storage, B-trees and
B+trees, indexing, query execution, and transactions.

**Ground rules for this project:**
- Hand-roll everything that's a learning surface: pages, varints, records,
  B-trees, the SQL lexer/parser, the planner/executor, and transactions.
  No parser-combinator crates, no SQL crates, no B-tree/collection crates.
  Peripheral utility crates (e.g. `anyhow`, already in use) are fine.
- Build on the existing code in `src/file/` and `FILE.md` — repair bugs as
  you find them rather than starting over.
- Each subproject should leave the tree in a state where `cargo test` passes
  and proves the subproject's "definition of done."

## Master Checklist

- [ ] Subproject 0 — Baseline hygiene
- [ ] Subproject 1 — Fix & solidify the storage format foundation
- [ ] Subproject 2 — Record format (the "row" encoding)
- [ ] Subproject 3 — B-tree read path (interior pages + traversal)
- [ ] Subproject 4 — Pager / buffer pool
- [ ] Subproject 5 — B-tree write path: insert & split
- [ ] Subproject 6 — Delete & rebalancing (simplified)
- [ ] Subproject 7 — Freelist & overflow pages
- [ ] Subproject 8 — Secondary indexes (index B-trees / B+tree style)
- [ ] Subproject 9 — Catalog & schema
- [ ] Subproject 10 — SQL tokenizer & parser
- [ ] Subproject 11 — Query planning & execution
- [ ] Subproject 12 — Transactions & durability
- [ ] Subproject 13 — CLI / REPL and integration
- [ ] Stretch goals (optional, see bottom)

---

## Subproject 0 — Baseline hygiene

**Learning goal:** a trustworthy dev loop before any feature work — `cargo
build`/`cargo test` must mean something.

**What you'll build:** no new features — just make the existing skeleton
internally consistent and testable.

**Checklist**
- [ ] Reconcile `FILE.md`'s documented page-size-byte offset (14) with
      `header.rs::HEADER_PAGE_SIZE_OFFSET` (currently 16) — decide which is
      correct and fix the other.
- [ ] Re-derive and document `HEADER_SIZE` (currently a bare `23` with no
      justification in `FILE.md` or in a comment).
- [ ] Add a `tests/` directory (integration tests) alongside the existing
      inline unit tests.
- [ ] Add `cargo fmt` and `cargo clippy --all-targets` to your personal dev
      loop (run them before every commit).
- [ ] Commit this as its own "chore: baseline hygiene" commit before moving on.

**Definition of done:** `cargo build`, `cargo test`, and `cargo clippy` all
run clean, and `FILE.md` matches the code exactly for every offset it
documents.

---

## Subproject 1 — Fix & solidify the storage format foundation

**Learning goal:** variable-length integer encoding, and getting the
lowest layer of the on-disk format provably correct before anything is
built on top of it.

**What you'll build:** `src/file/varint.rs`, and fixes to `page.rs`.

**Checklist**
- [ ] Write `varint::encode(u64) -> Vec<u8>` and `varint::decode(&[u8]) ->
      (u64, usize)` implementing SQLite-style variable-length integers
      (1–8 bytes each contributing 7 bits with a continuation bit, 9th byte
      special-cased to contribute a full 8 bits). Write round-trip property
      tests (`encode` then `decode` recovers the original value) across the
      value space, including edge values (0, 2^7-1, 2^7, u64::MAX).
  - [ ] Replace `TableLeafCell`'s fixed 8-byte `size`/`row_id` reads
      (`read_word_u64_be`) with varint reads. Update `parse_table_leaf_cell`
      accordingly.
- [ ] Fix `parse_table_leaf_page`'s content-buffer bug: it always slices at
      `LEAF_PAGE_HEADER_SIZE`, ignoring `leaf_data_offset` on the first page.
      Make the first page (which has the 23-byte file header in front of its
      page header) compute the cell-pointer array's start correctly.
- [ ] Remove the redundant re-parse of `page_type` inside
      `parse_page_header` (the caller already knows it).
- [ ] Write a hand-built "golden file" test: construct the exact bytes of a
      single-page database with a header + a few leaf cells by hand in a
      test function, parse it, and assert the resulting `Page`/`HeaderInfo`
      match what you encoded.
- [ ] Write a symmetric serializer: `HeaderInfo::to_bytes()` and a page
      serializer for `TableLeafPage`, so you can round-trip
      bytes → struct → bytes and assert equality. (You don't have a pager
      yet — these just operate on `Vec<u8>`/`&mut [u8]` for now.)

**Definition of done:** a golden-file test parses a hand-built page
correctly, and a round-trip test (parse → serialize → parse again) produces
identical structs, for a leaf page with multiple cells of varying payload
sizes.

---

## Subproject 2 — Record format (the "row" encoding)

**Learning goal:** how a logical row (typed columns) becomes the bytes
stored in a cell's payload — decoupling "what a row contains" from "how
cells sit on a page."

**What you'll build:** `src/file/record.rs`.

**Checklist**
- [ ] Define a `Value` enum for the column types you'll support to start:
      `Null`, `Int(i64)` (pick one or a few widths), `Float(f64)`,
      `Text(String)`, `Blob(Vec<u8>)`.
- [ ] Define "serial type" codes (a small fixed enum/const table mapping a
      serial type number to a value's on-disk width/representation) and a
      varint-encoded header: `[header length varint][serial type varints...]`
      followed by the body: the concatenated values in order.
- [ ] Write `Record::encode(&[Value]) -> Vec<u8>` and
      `Record::decode(&[u8]) -> anyhow::Result<Vec<Value>>`.
- [ ] Round-trip unit tests per type, and a mixed-type row test.
- [ ] Wire `TableLeafCell.payload` to be interpreted as a `Record` (add a
      `cell.decode_record() -> anyhow::Result<Vec<Value>>` helper, or store
      the decoded form directly — your call, but keep the raw bytes
      available too since Subproject 7 needs to relocate payloads untouched).

**Definition of done:** you can construct a row of mixed-type values,
encode it as a record, embed it as a cell payload, parse the page, and get
the original values back out.

---

## Subproject 3 — B-tree read path (interior pages + traversal)

**Learning goal:** B-tree search — the first real DBMS data-structure work.

**What you'll build:** interior-page parsing in `page.rs`, and a new
`src/btree/` module with a `Cursor`.

**Checklist**
- [ ] Implement `PageType::TableInterior` parsing (currently `todo!()` in
      `parse_page`): a list of `(child_page_number, max_row_id_in_subtree)`
      cells plus the existing `rightmost_child` pointer.
- [ ] Add a `TableInteriorCell` type and a `TableInteriorPage` variant to
      the `Page` enum.
- [ ] Create `src/btree/mod.rs` with a `Cursor` type that, given a root page
      number and a `row_id`, walks down through interior pages (binary- or
      linear-searching separator keys) to the correct leaf.
- [ ] Implement `Cursor::seek(row_id) -> Option<&TableLeafCell>` (point
      lookup).
- [ ] Implement `Cursor::iter()` — an iterator that walks every leaf cell
      in row-id order across multiple leaf pages (you'll need some way to
      move "next leaf" — either via parent back-links you track during
      descent, or by re-descending from the root for the next key; pick the
      simpler one now and note it as a simplification).
- [ ] Hand-build a 3-level tree fixture (root interior → interior → leaves)
      across several `Vec<u8>` "pages" in a test harness (no real file I/O
      yet — that's Subproject 4) and test both point lookups and a full
      ordered scan against it.

**Definition of done:** given the hand-built multi-page fixture, point
lookups for both present and absent row ids behave correctly, and a full
scan returns every row in ascending row-id order.

---

## Subproject 4 — Pager / buffer pool

**Learning goal:** separating "logical pages" from "bytes on disk," and
basic buffer-pool management — a concept central to every real DBMS.

**What you'll build:** `src/pager/mod.rs`.

**Checklist**
- [ ] Define `PageId = u32` and a `Pager` struct wrapping a `std::fs::File`
      opened with read/write access, plus the page size (read from the
      header on open).
- [ ] Implement `Pager::open(path) -> anyhow::Result<Pager>`: read the first
      100... er, 23-ish bytes (whatever `HEADER_SIZE` ends up being) to get
      the page size, validate the magic string.
- [ ] Implement `Pager::create(path, page_size) -> anyhow::Result<Pager>`
      for brand-new files (write a fresh header + an empty root leaf page).
- [ ] Implement `Pager::read_page(id: PageId) -> anyhow::Result<&[u8]>` /
      `get_page_mut` backed by an in-memory cache (`HashMap<PageId, Vec<u8>>`
      to start — no eviction policy yet, note that as a known limitation).
- [ ] Implement dirty-page tracking (a `HashSet<PageId>` or a flag per
      cached page) and `Pager::flush() -> anyhow::Result<()>` that writes
      every dirty page back and clears the dirty set.
- [ ] Implement `Pager::allocate_page() -> PageId` (append a new page at
      EOF for now — Subproject 7's freelist will make this smarter).
- [ ] Rewire `btree::Cursor` to take a `&mut Pager` instead of raw page
      slices, and port the Subproject 3 tests to run against a real
      temp-file-backed `Pager` (use `tempfile`-style manual temp dirs, or
      just `std::env::temp_dir()` + a unique name, to avoid adding a crate).

**Definition of done:** create a fresh database file on disk via `Pager`,
and the Subproject 3 traversal tests pass unchanged in spirit but now go
through real file I/O with a cache in front of it.

---

## Subproject 5 — B-tree write path: insert & split

**Learning goal:** the heart of B-tree mechanics — this is the subproject
most people mean when they say "I want to understand B-trees."

**What you'll build:** insert logic in `src/btree/`.

**Checklist**
- [ ] Implement `Cursor`/`BTree::insert(row_id, payload: &[u8])`: descend to
      the correct leaf, insert the new cell in sorted position among
      existing cell pointers, shifting pointers as needed.
- [ ] Implement overflow detection: after a tentative insert, check whether
      the cell content plus the pointer array still fits in the page; if
      not, the page must split.
- [ ] Implement leaf split: allocate a new page via `Pager::allocate_page`,
      move the upper half of cells to it, and determine the separator key
      (the first row id of the new right page, or last of the left,
      depending on your convention — document which).
- [ ] Implement separator propagation into the parent interior page:
      insert `(separator_key, new_child_page)` into the parent, which may
      itself overflow and split (recursive/iterative "split propagation").
- [ ] Implement root split: when the root itself overflows, allocate two
      new pages for the old root's contents and turn the root page into a
      fresh interior page with one separator — root page number never
      changes, which matters once Subproject 9 stores it in the catalog.
- [ ] Write invariant-checking test helpers now (you'll reuse them in
      Subproject 6): every leaf at the same depth, every leaf's keys sorted
      and non-overlapping with siblings, every interior separator consistent
      with the max key in its left subtree.
- [ ] Test: insert enough rows (in both ascending and random order) to force
      multiple splits and at least one root split; then do a full ordered
      scan via the pager-backed file and assert every row comes back
      correct and in order; run the invariant checker over the resulting
      tree.

**Definition of done:** inserting thousands of rows in random order into a
real file, closing and reopening the `Pager`, and scanning back all rows in
sorted order with invariants holding.

---

## Subproject 6 — Delete & rebalancing (simplified)

**Learning goal:** the half of B-trees most tutorials skip.

**What you'll build:** delete logic in `src/btree/`.

**Checklist**
- [ ] Implement leaf delete: locate the cell by row id, remove its pointer,
      compact the cell-pointer array (you can leave the "hole" in the cell
      content area unreclaimed for now — note that as a simplification that
      Subproject 7's freelist thinking doesn't need to solve at the page-
      internal level, only at the whole-page level).
- [ ] Decide and document your rebalancing policy explicitly rather than
      implementing full SQLite-style borrow/merge on every underflow. A
      reasonable simplified policy: tolerate underfull leaves, and only
      merge two leaves when one becomes completely empty (reclaim that page
      via the pager's free-page mechanism from Subproject 7, or stub it as
      "leaked" until Subproject 7 lands — pick one and note it).
- [ ] If a merge empties out a separator in the parent, remove that
      separator (and recursively handle parent underflow using the same
      simplified policy).
- [ ] Reuse the Subproject 5 invariant checker after every delete in tests.
- [ ] Test: insert N rows, delete a pseudo-random subset, assert the
      remaining rows are exactly right and invariants hold; also test
      deleting a nonexistent row id is a clean no-op/error per your API
      design.

**Definition of done:** a test that inserts, deletes a chunk, inserts more,
deletes more, and after each phase both (a) scanning matches an in-memory
reference `BTreeMap` you maintain in the test and (b) the invariant checker
passes.

---

## Subproject 7 — Freelist & overflow pages

**Learning goal:** reclaiming disk space, and handling payloads larger than
a single page.

**What you'll build:** freelist support in `Pager`, overflow-page chaining
in `src/file/`.

**Checklist**
- [ ] Implement the freelist per `FILE.md`'s structure (trunk pages holding
      pointers to leaf/free pages): `Pager::free_page(id)` pushes onto the
      freelist, `Pager::allocate_page()` pops from it first before
      appending at EOF.
- [ ] Store the freelist head page number in the file header (you'll need
      to add that field — `FILE.md` has it commented out; design its
      offset/size and update both the doc and `header.rs`).
- [ ] Wire Subproject 6's "leaked page on merge" into real
      `Pager::free_page` calls.
- [ ] Implement overflow pages: when a record's payload is too large for a
      cell on a given page, store the first N bytes inline followed by a
      pointer to an overflow page (which itself may chain to further
      overflow pages for very large payloads). Implement both the write
      side (splitting a large payload across a chain) and the read side
      (`Record`/cell decoding transparently follows the chain to
      reassemble the full payload).
- [ ] Test: insert a blob/text value large enough to require 2+ overflow
      pages, read it back and verify byte-for-byte equality; delete enough
      rows to exercise freelist reuse and assert a subsequent insert reuses
      a freed page number rather than growing the file.

**Definition of done:** round-trip test for an oversized payload across
multiple overflow pages, and a test proving freed pages get reused (assert
file size doesn't grow when inserts reuse freed capacity).

---

## Subproject 8 — Secondary indexes (index B-trees / B+tree style)

**Learning goal:** the practical difference between a table B-tree (keyed
by row id, leaf carries the full row) and an index B-tree (keyed by an
indexed column's value, leaf carries a pointer back to the row id) — this
*is* the B+tree half of "B-trees and B+trees" from the original goal.

**What you'll build:** `IndexLeafCell`/`IndexInteriorCell` in `page.rs`,
index-aware insert/delete/search in `src/btree/`.

**Checklist**
- [ ] Define index cell types: a leaf cell holds `(key: Record, row_id)`;
      interior cells hold separator keys + child pointers, same shape as
      table interior pages but comparing on the indexed value instead of
      row id.
- [ ] Generalize (or duplicate, if cleaner for learning purposes — your
      call, both are defensible) the Subproject 5/6 insert/split/delete
      logic to work for index trees, parameterized over a key comparator.
- [ ] Implement `Cursor::seek_index(key) -> impl Iterator<Item = row_id>`
      for equality lookups, and a range variant for `>`/`<`/`BETWEEN`-style
      lookups (ordered iteration from a starting key).
- [ ] Implement index maintenance: every table insert/update/delete that
      touches an indexed column must also insert/update/delete the
      corresponding index entry. For now this can be a manual call from
      test code — Subproject 11 wires it automatically.
- [ ] Decide and document your uniqueness story: are you supporting
      `UNIQUE` indexes, duplicate keys, or both? Implement accordingly.
- [ ] Test: build a table with 1000s of rows and a secondary index on one
      column, verify an index-based lookup returns the same row(s) a full
      table scan + filter would, and that it's actually doing less work
      (e.g. count pages visited, or just trust the structure — but at least
      assert correctness).

**Definition of done:** index-based equality and range lookups return
results identical to a linear scan over the same data, verified against a
reference implementation in the test (e.g. filtering a `Vec` directly).

---

## Subproject 9 — Catalog & schema

**Learning goal:** how a database tracks its own structure — the hinge
between "storage engine" and "SQL layer."

**What you'll build:** `src/catalog/mod.rs`, a `maze_master` system table.

**Checklist**
- [ ] Design the `maze_master` row shape: `(type: "table"|"index", name,
      table_name, root_page: u32, sql: String)` — `sql` stores the original
      `CREATE TABLE`/`CREATE INDEX` text (once Subproject 10 exists) or for
      now just a structured schema description.
- [ ] Reserve page 1 (or whatever your root page convention is) as the
      `maze_master` table's own root, created when `Pager::create` makes a
      fresh database.
- [ ] Implement `Catalog::load(&mut Pager) -> anyhow::Result<Catalog>`:
      scan `maze_master` on open and build an in-memory map of table/index
      name → (root page, column schema).
- [ ] Implement `Catalog::create_table(&mut Pager, name, columns) ->
      anyhow::Result<()>`: allocate a root page for the new table, insert
      its row into `maze_master`, update the in-memory map.
- [ ] Implement `Catalog::create_index(...)` similarly.
- [ ] Implement basic validation helpers: does a table/index exist, does a
      column exist on a table, what's its declared type — used by
      Subproject 11's planner/executor to validate statements before
      running them.
- [ ] Test: create a database, define two tables and an index via the
      catalog API, close and reopen the file, reload the catalog, and
      assert the schema came back correctly and the tables' data (inserted
      before close) is still reachable via their root pages.

**Definition of done:** a full close/reopen cycle preserves schema and data,
driven entirely through the `Catalog` + `Pager` + `btree` layers, no SQL
text yet.

---

## Subproject 10 — SQL tokenizer & parser

**Learning goal:** hand-written lexing and recursive-descent parsing.

**What you'll build:** `src/sql/lexer.rs`, `src/sql/ast.rs`,
`src/sql/parser.rs`.

**Checklist**
- [ ] Define a `Token` enum: keywords (`CREATE`, `TABLE`, `INDEX`, `INSERT`,
      `INTO`, `VALUES`, `SELECT`, `FROM`, `WHERE`, `ORDER`, `BY`, `LIMIT`,
      `UPDATE`, `SET`, `DELETE`, `AND`, `OR`, `NULL`, type names, etc.),
      identifiers, literals (int/float/string), punctuation
      (`,`, `(`, `)`, `;`, operators), and EOF.
- [ ] Implement `Lexer::next_token` by hand (no regex crate — manual
      character scanning), handling whitespace/comments, quoted
      identifiers/strings, and number literals.
- [ ] Define the AST (`ast.rs`): `Stmt::CreateTable`, `Stmt::CreateIndex`,
      `Stmt::Insert`, `Stmt::Select`, `Stmt::Update`, `Stmt::Delete`, plus
      `Expr` for `WHERE`/`SET` expressions (column refs, literals,
      comparisons, `AND`/`OR`).
- [ ] Implement the recursive-descent parser for each statement kind, one
      at a time, in roughly this order: `CREATE TABLE` → `INSERT` →
      `SELECT` (no `WHERE` yet) → `WHERE` expressions → `ORDER BY`/`LIMIT`
      → `UPDATE`/`DELETE` → `CREATE INDEX`.
- [ ] Unit test each grammar rule in isolation, plus a handful of
      deliberately invalid inputs per statement kind asserting a parse
      error (not a panic).

**Definition of done:** a table-driven test feeding in ~30+ example SQL
strings across every supported statement kind produces the expected AST
(or a parse error for the deliberately-invalid ones).

---

## Subproject 11 — Query planning & execution

**Learning goal:** turning parsed SQL into actual B-tree operations, and
the iterator-based ("Volcano style") execution model used by real engines.

**What you'll build:** `src/exec/plan.rs`, `src/exec/executor.rs`.

**Checklist**
- [ ] Define a small logical plan enum: `Scan(table)`, `IndexScan(index,
      key_or_range)`, `Filter(plan, expr)`, `Project(plan, columns)`,
      `Sort(plan, columns)`, `Limit(plan, n)`.
- [ ] Implement a trivial planner: for `SELECT ... WHERE col = <literal>`,
      check the catalog for an index on `col` and emit `IndexScan`;
      otherwise emit `Scan` + `Filter`. Keep the heuristic simple and
      document it as intentionally naive.
- [ ] Implement each plan node as something iterator-like producing rows
      (`fn next(&mut self) -> anyhow::Result<Option<Row>>`, composed the
      way `Iterator` combinators compose) — this is the "pull" execution
      model.
- [ ] Implement `INSERT`/`UPDATE`/`DELETE` execution: translate the AST
      into catalog lookups + `btree` operations, and crucially keep any
      secondary indexes (Subproject 8) in sync on every mutation.
- [ ] Tie it together: a `Database` type exposing `execute(sql: &str) ->
      anyhow::Result<QueryResult>` that lexes, parses, plans, and executes
      in one call.
- [ ] Test end-to-end: create a table, insert rows via SQL text, select
      with and without `WHERE`, with `ORDER BY`/`LIMIT`, update and delete
      subsets, and verify results at each step — purely through the
      `execute(sql)` interface now.

**Definition of done:** a test suite that only ever calls
`db.execute("...")` with real SQL strings and asserts on returned rows,
covering every statement kind from Subproject 10.

---

## Subproject 12 — Transactions & durability

**Learning goal:** atomicity and crash-durability, scoped down to something
implementable by hand.

**What you'll build:** `src/txn/mod.rs`, a rollback-journal mechanism.

**Checklist**
- [ ] Implement a rollback journal: before `Pager` first modifies a page
      within a transaction, copy that page's pre-image (original bytes +
      page number) to a journal file.
- [ ] Implement `BEGIN`/`COMMIT`: `COMMIT` flushes dirty pages then deletes
      (or truncates) the journal file; until commit, nothing is guaranteed
      durable.
- [ ] Implement `ROLLBACK`: restore every page recorded in the journal from
      its pre-image, discard in-memory dirty state, delete the journal.
- [ ] Implement crash recovery: on `Pager::open`, if a journal file exists
      from a previous run, replay it (restore pre-images) before doing
      anything else — simulating "the process died mid-transaction."
- [ ] Wire `BEGIN`/`COMMIT`/`ROLLBACK` into the SQL layer (Subproject 10/11)
      as additional statement kinds, with an implicit transaction wrapping
      any statement run outside an explicit one.
- [ ] Test: run a transaction that makes several writes, kill it with an
      explicit `ROLLBACK`, assert the database is byte-for-byte as it was
      before. Simulate a crash (copy the file + journal mid-transaction in
      a test, without calling `COMMIT`/`ROLLBACK`, then "reopen" and assert
      recovery restores the pre-transaction state).

**Definition of done:** the crash-simulation test above passes — a
"crashed" mid-write database recovers to a consistent prior state on next
open.

---

## Subproject 13 — CLI / REPL and integration

**Learning goal:** tying every layer together into something you actually
use.

**What you'll build:** an interactive binary (`src/bin/maze.rs` or an
extended `main.rs`).

**Checklist**
- [ ] Build a REPL loop: read a line from stdin, if it starts with `.` treat
      it as a dot-command (`.tables`, `.schema [table]`, `.open <file>`,
      `.quit`), otherwise feed it to `Database::execute` and pretty-print
      the resulting rows (simple column-aligned text table is enough).
- [ ] Support multi-line statement input (buffer until a trailing `;`).
- [ ] Add decent error reporting: parse errors, catalog errors (unknown
      table/column), and execution errors should all print a readable
      message instead of a panic or a raw `Debug` dump.
- [ ] Write a handful of true end-to-end integration tests in `tests/`
      that open a fresh temp file, run a realistic sequence of DDL/DML/
      queries/transactions through the full stack, and assert on results —
      this is the test that exercises every subproject built so far in one
      pass.
- [ ] Update `FILE.md`/write a short `README.md` describing what Maze is,
      how to run it, and which parts of the on-disk format diverge from
      real SQLite and why (a nice capstone artifact of what you learned).

**Definition of done:** you can run the binary, type SQL interactively
against a file on disk, quit, reopen it, and your data is still there —
plus the full integration test suite passes.

---

## Stretch goals (optional, not required for the core plan)

- [ ] Concurrency: single-writer/multi-reader locking, or a minimal MVCC
      scheme, instead of the single-threaded model assumed above.
- [ ] WAL mode as an alternative to the Subproject 12 rollback journal —
      implement it and compare the two approaches' tradeoffs in your
      README.
- [ ] `EXPLAIN`-style plan printing for the Subproject 11 planner.
- [ ] Compatibility check (diagnostic only, not a goal): read a page dump
      from a real `sqlite3` database file and compare byte-for-byte against
      your own parser's understanding of the format, to see exactly where
      Maze's format has diverged.

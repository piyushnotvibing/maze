/*
    maze_schema table structure:
    | obj_type | unique | name | table_name | root_pn | column list |

    -> obj_type: either index or table, as these are the only two types of database objects (no views or triggers yet).
    -> name: if type == table: name == table_name, else if type == index, name = name of the index, table_name = name of the table the index is on.
    -> table_name: name of the table, or the index the table is on.
    -> root_pn: the page number of the root page of this database object.
    // -> sql: the NORMALIZED SQL statement used to create this object.
    -> column list: a Record encoded list of all column names and their data types.
        -> for indexes, this would mean the index is on these columns on the table mentioned in `name`.
        -> for tables, this is the list of columns in that table.
        -> format: [Name1, Type1 code, Name2, Type2... NameN, TypeN]
*/

use std::collections::{HashMap, HashSet};

use crate::{
    btree::{index_cursor::IndexCursor, table_cursor::TableCursor},
    storage::{
        pager::Pager,
        pages::{
            btree_page::BTreePage,
            index::leaf::IndexLeafPage,
            table::leaf::TableLeafPage,
            types::{PageNumber, RowID},
        },
        record::{
            Record,
            values::{DataType, Value},
        },
    },
};

const MAZE_SCHEMA_ROOT_PN: u32 = 1;
const MAZE_SCHEMA_COLUMN_COUNT: usize = 6;

type TableName = String;
type IndexName = String;

pub struct TableInfo {
    pub root_pn: PageNumber,
    pub columns: Vec<Column>,
}

// this whole column struct is gonna go and
// be replaced by sql column in maze_schema
// i'll write a function in the sql module
// that will take a CREATE TABLE/INDEX
// string and return the columns in it.
// So we simply store the SQL string, then 
// create_table() & create_index() will call
// that function get_columns() or something
// to get the columns out of the sql string.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Column {
    pub name: String,
    pub c_type: DataType,
}

impl Column {
    fn to_values(&self) -> anyhow::Result<Vec<Value>> {
        Ok(vec![
            Value::Text(self.name.to_owned()),
            Value::Int(self.c_type.encode()? as i64),
        ])
    }
}

pub struct IndexInfo {
    pub table_name: String,
    pub root_pn: PageNumber,
    pub on_columns: Vec<Column>,
    pub unique: bool,
}

pub struct Catalog {
    pub tables: HashMap<TableName, TableInfo>,
    pub indexes: HashMap<IndexName, IndexInfo>,
}

impl Catalog {
    pub fn load(pager: &mut Pager) -> anyhow::Result<Self> {
        let mut schema_cursor = TableCursor::new(pager, PageNumber::try_from(MAZE_SCHEMA_ROOT_PN)?);
        let rows = schema_cursor
            .scan()?
            .into_iter()
            .map(|cell| cell.local_payload)
            .collect::<Vec<Vec<u8>>>();

        Self::parse(rows)
    }

    pub fn save(&self, pager: &mut Pager) -> anyhow::Result<()> {
        let mut schema_cursor = TableCursor::new(pager, PageNumber::try_from(MAZE_SCHEMA_ROOT_PN)?);
        schema_cursor.delete_all()?;

        let maze_schema_row_bytes = self.serialize()?;
        for (i, row) in maze_schema_row_bytes.into_iter().enumerate() {
            let row_id = RowID::try_from((i + 1) as u64)?;
            schema_cursor.insert(row_id, &row)?;
        }

        Ok(())
    }

    pub fn create_table(
        &mut self,
        new_table_name: &str,
        columns: Vec<Column>,
        pager: &mut Pager,
    ) -> anyhow::Result<()> {
        if self.tables.contains_key(new_table_name) {
            anyhow::bail!("table already exists")
        }
        if columns.is_empty() {
            anyhow::bail!("table does not have columns")
        }

        let mut schema_cursor = TableCursor::new(pager, PageNumber::try_from(MAZE_SCHEMA_ROOT_PN)?);
        let new_table_root_pn = schema_cursor.pager.allocate_page()?;
        schema_cursor.pager.write_page(
            new_table_root_pn,
            TableLeafPage::new(vec![]).serialize(schema_cursor.pager.spec(new_table_root_pn))?,
        )?;

        let new_table_info = TableInfo {
            root_pn: new_table_root_pn,
            columns,
        };
        self.tables
            .insert(new_table_name.to_string(), new_table_info);
        self.save(&mut schema_cursor.pager)
    }

    pub fn drop_table(&mut self, table_name: &str, pager: &mut Pager) -> anyhow::Result<()> {
        if !self.tables.contains_key(table_name) {
            anyhow::bail!("non-existent table can't be dropped")
        }
        let tbd_table_info = self.tables.get(table_name).unwrap();

        let mut tbd_table_cursor =
            TableCursor::new(pager, PageNumber::try_from(tbd_table_info.root_pn)?);
        tbd_table_cursor.delete_all()?;
        tbd_table_cursor.pager.free_page(tbd_table_info.root_pn)?;

        self.tables.remove(table_name);
        self.save(pager)
    }

    pub fn create_index(
        &mut self,
        new_index_name: &str,
        on_table: &str,
        on_columns: Vec<Column>,
        unique: bool,
        pager: &mut Pager,
    ) -> anyhow::Result<()> {
        if self.indexes.contains_key(new_index_name) {
            anyhow::bail!("index {new_index_name} already exists on table {on_table}")
        }
        if !self.tables.contains_key(on_table) {
            anyhow::bail!(
                "the table {on_table}, for which you want to create the index {new_index_name}, does not exists"
            )
        }
        if on_columns.is_empty() {
            anyhow::bail!(
                "do you want to create an index or not wtf there are no columns to create an index on"
            )
        }
        if self.tables.contains_key(on_table) {
            let columns = self
                .tables
                .get(on_table)
                .unwrap()
                .columns
                .iter()
                .collect::<HashSet<&Column>>();
            if !on_columns.iter().all(|col| columns.contains(col)) {
                anyhow::bail!(
                    "table {on_table} does not contain one of these columns {:?}",
                    on_columns
                )
            }
        }

        let mut schema_cursor = TableCursor::new(pager, PageNumber::try_from(MAZE_SCHEMA_ROOT_PN)?);
        let new_index_root_pn = schema_cursor.pager.allocate_page()?;
        schema_cursor.pager.write_page(
            new_index_root_pn,
            IndexLeafPage::new(vec![]).serialize(schema_cursor.pager.spec(new_index_root_pn))?,
        )?;
        let new_index_info = IndexInfo {
            table_name: on_table.to_string(),
            root_pn: new_index_root_pn,
            on_columns,
            unique,
        };

        self.indexes
            .insert(new_index_name.to_string(), new_index_info);
        self.save(&mut schema_cursor.pager)
    }

    pub fn drop_index(&mut self, index_name: &str, pager: &mut Pager) -> anyhow::Result<()> {
        if !self.indexes.contains_key(index_name) {
            anyhow::bail!("non-existent table can't be dropped")
        }
        let tbd_index_info = self.indexes.get(index_name).unwrap();

        let mut tbd_index_cursor = IndexCursor::new(
            pager,
            PageNumber::try_from(tbd_index_info.root_pn)?,
            tbd_index_info.unique,
        );
        tbd_index_cursor.delete_all()?;
        tbd_index_cursor.pager.free_page(tbd_index_info.root_pn)?;

        self.indexes.remove(index_name);
        self.save(pager)
    }

    /// the upper layer will pass
    /// `(schema_cursor (of type TableCursor)).scan().into_iter()
    ///     .map(|cell| cell.total_payload).collect()`,
    /// so `rows` is a Vec<Vec<u8>>
    fn parse(rows: Vec<Vec<u8>>) -> anyhow::Result<Self> {
        let mut tables: HashMap<TableName, TableInfo> = HashMap::new();
        let mut indexes: HashMap<IndexName, IndexInfo> = HashMap::new();

        for (i, row_bytes) in rows.iter().enumerate() {
            let (row, _) = Record::decode(row_bytes)?;

            if row.len() != MAZE_SCHEMA_COLUMN_COUNT {
                anyhow::bail!("maze schema row has incorrect number of columns")
            }

            let (mut obj_type, mut root_pn) = (0u8, 0u32);
            let (mut name, mut table_name) = (String::new(), String::new());
            let mut unique = false;
            let mut columns: Vec<Column> = Vec::new();
            for (idx, value) in row.into_iter().enumerate() {
                match value {
                    Value::Null => anyhow::bail!("no column's value in maze_manager can be null"),
                    Value::Float(_) => anyhow::bail!(
                        "no column's value in maze_manager can be a floating point number"
                    ),

                    Value::Int(i) if idx == 0 => obj_type = u8::try_from(i)?,
                    Value::Int(i) if idx == 1 => unique = i == 1,
                    Value::Text(t) if idx == 2 => name = t,
                    Value::Text(t) if idx == 3 => table_name = t,
                    Value::Int(i) if idx == 4 => root_pn = u32::try_from(i)?,
                    Value::Blob(cols) if idx == 5 => {
                        let (column_list, _) = Record::decode(&cols)?;
                        if !column_list.len().is_multiple_of(2) {
                            anyhow::bail!("column list of row {i} is incomplete or corrupted")
                        }
                        columns = column_list
                            .chunks(2)
                            .map(|col| {
                                let column_name = match &col[0] {
                                    Value::Null => anyhow::bail!("column name cannot be null"),
                                    Value::Int(_) => anyhow::bail!("column name cannot be an int"),
                                    Value::Float(_) => {
                                        anyhow::bail!("column name cannot be a float")
                                    }
                                    Value::Blob(_) => anyhow::bail!("column name cannot be a blob"),
                                    Value::Text(t) => Ok::<String, anyhow::Error>(t.to_string()),
                                }?;
                                let column_type = match &col[1] {
                                    Value::Null => anyhow::bail!("column type cannot be null"),
                                    Value::Int(code) => {
                                        if code.is_negative() {
                                            anyhow::bail!("invalid column code: negative number")
                                        }
                                        DataType::new(*code as u64)
                                    }
                                    Value::Float(_) => {
                                        anyhow::bail!("column type cannot be a float")
                                    }
                                    Value::Blob(_) => anyhow::bail!("column type cannot be a blob"),
                                    Value::Text(_) => anyhow::bail!("column type cannot be text"),
                                }?;

                                Ok(Column {
                                    name: column_name,
                                    c_type: column_type,
                                })
                            })
                            .collect::<anyhow::Result<Vec<Column>>>()?;
                    }

                    _ => anyhow::bail!(
                        "column mismatch, invalid data in column {}",
                        match idx {
                            0 => "obj_type",
                            1 => "unique",
                            2 => "name",
                            3 => "table_name",
                            4 => "root_pn",
                            5 => "column_codes",
                            _ => "error",
                        }
                    ),
                }
            }

            let root_pn = PageNumber::try_from(root_pn)?;
            let obj_type = ObjectType::try_from(obj_type)?;

            match obj_type {
                ObjectType::Index => {
                    indexes.insert(
                        name,
                        IndexInfo {
                            table_name,
                            root_pn,
                            on_columns: columns,
                            unique,
                        },
                    );
                }
                ObjectType::Table => {
                    tables.insert(table_name, TableInfo { root_pn, columns });
                }
            }
        }

        Ok(Self { tables, indexes })
    }

    pub fn serialize(&self) -> anyhow::Result<Vec<Vec<u8>>> {
        let mut bytes = Vec::new();

        for (table_name, table_info) in &self.tables {
            let mut columns_serialized: Vec<Value> =
                Vec::with_capacity(table_info.columns.len() * 2);
            for column in &table_info.columns {
                columns_serialized.extend(column.to_values()?);
            }

            let row = vec![
                Value::Int(ObjectType::Table.encode() as i64),
                Value::Int(0),
                Value::Text(table_name.to_owned()),
                Value::Text(table_name.to_owned()),
                Value::Int(table_info.root_pn.get() as i64),
                Value::Blob(Record::new(columns_serialized)?.encode()),
            ];

            let row_bytes = Record::new(row)?.encode();
            bytes.push(row_bytes);
        }

        for (index_name, index_info) in &self.indexes {
            let mut on_columns_serialized: Vec<Value> =
                Vec::with_capacity(index_info.on_columns.len() * 2);
            for column in &index_info.on_columns {
                on_columns_serialized.extend(column.to_values()?);
            }

            let row = vec![
                Value::Int(ObjectType::Index.encode() as i64),
                Value::Int(if index_info.unique { 1 } else { 0 }),
                Value::Text(index_name.to_owned()),
                Value::Text(index_info.table_name.to_owned()),
                Value::Int(index_info.root_pn.get() as i64),
                Value::Blob(Record::new(on_columns_serialized)?.encode()),
            ];

            let row_bytes = Record::new(row)?.encode();
            bytes.push(row_bytes);
        }

        Ok(bytes)
    }
}

pub enum ObjectType {
    Index,
    Table,
}

impl ObjectType {
    pub fn encode(&self) -> u8 {
        match self {
            ObjectType::Index => 1,
            ObjectType::Table => 2,
        }
    }
}

// ObjectType really only needs a bit, but lets reserve a byte for it in case I ever implement views and/or triggers.
impl TryFrom<u8> for ObjectType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> anyhow::Result<Self> {
        match value {
            1 => Ok(ObjectType::Index),
            2 => Ok(ObjectType::Table),
            _ => anyhow::bail!("invalid object type"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // builds the blob stored in a maze_schema row's "column list" field:
    // [Name1, Type1 code, Name2, Type2 code, ...], Record-encoded.
    fn encode_column_list(columns: &[(&str, DataType)]) -> Vec<u8> {
        let mut values = Vec::new();
        for (name, c_type) in columns {
            values.push(Value::Text(name.to_string()));
            values.push(Value::Int(c_type.encode().unwrap() as i64));
        }
        Record::new(values).unwrap().encode()
    }

    // builds one raw maze_schema row's bytes, exactly as the upper layer would
    // hand to Catalog::parse (one Vec<u8> per row). `unique` is only meaningful
    // for index rows; pass 0 for table rows, matching Catalog::serialize.
    fn encode_row(
        obj_type: ObjectType,
        unique: i64,
        name: &str,
        table_name: &str,
        root_pn: u32,
        columns: &[(&str, DataType)],
    ) -> Vec<u8> {
        let values = vec![
            Value::Int(obj_type.encode() as i64),
            Value::Int(unique),
            Value::Text(name.to_string()),
            Value::Text(table_name.to_string()),
            Value::Int(root_pn as i64),
            Value::Blob(encode_column_list(columns)),
        ];
        Record::new(values).unwrap().encode()
    }

    #[test]
    fn parse_single_table_row_produces_correct_table_info() {
        let row = encode_row(
            ObjectType::Table,
            0,
            "users",
            "users",
            5,
            &[("id", DataType::Int), ("email", DataType::Text(0))],
        );

        let catalog = Catalog::parse(vec![row]).unwrap();

        assert_eq!(catalog.tables.len(), 1);
        assert!(catalog.indexes.is_empty());

        let table = catalog.tables.get("users").expect("users table missing");
        assert_eq!(table.root_pn, 5);
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.columns[0].name, "id");
        assert_eq!(table.columns[0].c_type, DataType::Int);
        assert_eq!(table.columns[1].name, "email");
        assert_eq!(table.columns[1].c_type, DataType::Text(0));
    }

    #[test]
    fn parse_single_index_row_produces_correct_index_info() {
        let row = encode_row(
            ObjectType::Index,
            1,
            "idx_email",
            "users",
            9,
            &[("email", DataType::Text(0))],
        );

        let catalog = Catalog::parse(vec![row]).unwrap();

        assert!(catalog.tables.is_empty());
        assert_eq!(catalog.indexes.len(), 1);

        let index = catalog.indexes.get("idx_email").expect("idx_email missing");
        assert_eq!(index.table_name, "users");
        assert_eq!(index.root_pn, 9);
        assert!(index.unique);
        assert_eq!(index.on_columns.len(), 1);
        assert_eq!(index.on_columns[0].name, "email");
        assert_eq!(index.on_columns[0].c_type, DataType::Text(0));
    }

    #[test]
    fn parse_multiple_rows_builds_both_tables_and_indexes_together() {
        let users_row = encode_row(
            ObjectType::Table,
            0,
            "users",
            "users",
            5,
            &[("id", DataType::Int)],
        );
        let posts_row = encode_row(
            ObjectType::Table,
            0,
            "posts",
            "posts",
            7,
            &[("id", DataType::Int), ("body", DataType::Text(0))],
        );
        let index_row = encode_row(
            ObjectType::Index,
            0,
            "idx_email",
            "users",
            9,
            &[("email", DataType::Text(0))],
        );

        // order shouldn't matter - parse has to sort rows into the right map by obj_type.
        let catalog = Catalog::parse(vec![posts_row, index_row, users_row]).unwrap();

        assert_eq!(catalog.tables.len(), 2);
        assert_eq!(catalog.indexes.len(), 1);

        let users = catalog.tables.get("users").expect("users missing");
        assert_eq!(users.root_pn, 5);
        assert_eq!(users.columns.len(), 1);

        let posts = catalog.tables.get("posts").expect("posts missing");
        assert_eq!(posts.root_pn, 7);
        assert_eq!(posts.columns.len(), 2);

        let idx = catalog.indexes.get("idx_email").expect("idx_email missing");
        assert_eq!(idx.table_name, "users");
        assert_eq!(idx.root_pn, 9);
    }

    #[test]
    fn round_trip_of_an_empty_catalog_stays_empty() {
        let catalog = Catalog {
            tables: HashMap::new(),
            indexes: HashMap::new(),
        };

        let rows = catalog.serialize().unwrap();
        assert!(rows.is_empty());

        let round_tripped = Catalog::parse(rows).unwrap();
        assert!(round_tripped.tables.is_empty());
        assert!(round_tripped.indexes.is_empty());
    }

    #[test]
    fn serialize_then_parse_round_trips_to_an_equal_catalog() {
        let mut tables = HashMap::new();
        tables.insert(
            "users".to_string(),
            TableInfo {
                root_pn: PageNumber::try_from(5).unwrap(),
                columns: vec![
                    Column {
                        name: "id".to_string(),
                        c_type: DataType::Int,
                    },
                    Column {
                        name: "email".to_string(),
                        c_type: DataType::Text(0),
                    },
                ],
            },
        );

        let mut indexes = HashMap::new();
        indexes.insert(
            "idx_email".to_string(),
            IndexInfo {
                table_name: "users".to_string(),
                root_pn: PageNumber::try_from(9).unwrap(),
                on_columns: vec![Column {
                    name: "email".to_string(),
                    c_type: DataType::Text(0),
                }],
                unique: false,
            },
        );

        let original = Catalog { tables, indexes };

        let rows = original.serialize().unwrap();
        let round_tripped = Catalog::parse(rows).unwrap();

        assert_eq!(round_tripped.tables.len(), original.tables.len());
        assert_eq!(round_tripped.indexes.len(), original.indexes.len());

        let users = round_tripped.tables.get("users").unwrap();
        let original_users = &original.tables["users"];
        assert_eq!(users.root_pn, original_users.root_pn.get());
        assert_eq!(users.columns.len(), original_users.columns.len());
        for (got, want) in users.columns.iter().zip(original_users.columns.iter()) {
            assert_eq!(got.name, want.name);
            assert_eq!(got.c_type, want.c_type);
        }

        let idx = round_tripped.indexes.get("idx_email").unwrap();
        let original_idx = &original.indexes["idx_email"];
        assert_eq!(idx.table_name, original_idx.table_name);
        assert_eq!(idx.root_pn, original_idx.root_pn.get());
        for (got, want) in idx.on_columns.iter().zip(original_idx.on_columns.iter()) {
            assert_eq!(got.name, want.name);
            assert_eq!(got.c_type, want.c_type);
        }
    }

    const TEST_PAGE_SIZE: usize = 512;

    fn temp_db_path(tag: &str) -> String {
        let mut path = std::env::temp_dir();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        path.push(format!("maze_catalog_test_{tag}_{unique}.mz"));
        path.to_str().unwrap().to_string()
    }

    fn empty_catalog() -> Catalog {
        Catalog {
            tables: HashMap::new(),
            indexes: HashMap::new(),
        }
    }

    #[test]
    fn create_table_then_load_shows_the_new_table_with_its_columns() {
        let path = temp_db_path("create_table_load");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        catalog
            .create_table(
                "users",
                vec![
                    Column {
                        name: "id".to_string(),
                        c_type: DataType::Int,
                    },
                    Column {
                        name: "email".to_string(),
                        c_type: DataType::Text(0),
                    },
                ],
                &mut pager,
            )
            .unwrap();

        assert!(catalog.tables.contains_key("users"));

        let reloaded = Catalog::load(&mut pager).unwrap();
        let users = reloaded
            .tables
            .get("users")
            .expect("users table missing after reload");
        assert_eq!(users.columns.len(), 2);
        assert_eq!(users.columns[0].name, "id");
        assert_eq!(users.columns[0].c_type, DataType::Int);
        assert_eq!(users.columns[1].name, "email");
        assert_eq!(users.columns[1].c_type, DataType::Text(0));
    }

    #[test]
    fn create_table_rejects_a_duplicate_name() {
        let path = temp_db_path("create_table_dup");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        let cols = vec![Column {
            name: "id".to_string(),
            c_type: DataType::Int,
        }];
        catalog
            .create_table("users", cols.clone(), &mut pager)
            .unwrap();

        assert!(catalog.create_table("users", cols, &mut pager).is_err());
    }

    #[test]
    fn create_table_rejects_an_empty_column_list() {
        let path = temp_db_path("create_table_no_cols");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        assert!(catalog.create_table("users", vec![], &mut pager).is_err());
    }

    #[test]
    fn create_table_initializes_the_new_root_page_so_it_can_be_inserted_into() {
        let path = temp_db_path("create_table_root_init");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        catalog
            .create_table(
                "users",
                vec![Column {
                    name: "id".to_string(),
                    c_type: DataType::Int,
                }],
                &mut pager,
            )
            .unwrap();

        let root_pn = catalog.tables.get("users").unwrap().root_pn;
        let mut table_cursor = TableCursor::new(&mut pager, root_pn);
        table_cursor
            .insert(RowID::try_from(1).unwrap(), b"hello")
            .unwrap();
        let scanned = table_cursor.scan().unwrap();
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].local_payload, b"hello");
    }

    #[test]
    fn drop_table_removes_it_from_the_catalog_and_persists_after_reload() {
        let path = temp_db_path("drop_table");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        let id_col = Column {
            name: "id".to_string(),
            c_type: DataType::Int,
        };
        catalog
            .create_table("users", vec![id_col.clone()], &mut pager)
            .unwrap();
        catalog
            .create_table("posts", vec![id_col], &mut pager)
            .unwrap();

        catalog.drop_table("users", &mut pager).unwrap();
        assert!(!catalog.tables.contains_key("users"));
        assert!(catalog.tables.contains_key("posts"));

        let reloaded = Catalog::load(&mut pager).unwrap();
        assert!(!reloaded.tables.contains_key("users"));
        assert!(reloaded.tables.contains_key("posts"));
    }

    #[test]
    fn drop_table_rejects_a_non_existent_table() {
        let path = temp_db_path("drop_table_missing");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        assert!(catalog.drop_table("ghost", &mut pager).is_err());
    }

    #[test]
    fn create_index_then_load_shows_the_new_index_with_its_columns_and_uniqueness() {
        let path = temp_db_path("create_index_load");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        let email_col = Column {
            name: "email".to_string(),
            c_type: DataType::Text(0),
        };
        catalog
            .create_table(
                "users",
                vec![
                    Column {
                        name: "id".to_string(),
                        c_type: DataType::Int,
                    },
                    email_col.clone(),
                ],
                &mut pager,
            )
            .unwrap();

        catalog
            .create_index("idx_email", "users", vec![email_col], true, &mut pager)
            .unwrap();

        assert!(catalog.indexes.contains_key("idx_email"));

        let reloaded = Catalog::load(&mut pager).unwrap();
        let idx = reloaded
            .indexes
            .get("idx_email")
            .expect("idx_email missing after reload");
        assert_eq!(idx.table_name, "users");
        assert!(idx.unique);
        assert_eq!(idx.on_columns.len(), 1);
        assert_eq!(idx.on_columns[0].name, "email");
        assert_eq!(idx.on_columns[0].c_type, DataType::Text(0));
    }

    #[test]
    fn create_index_rejects_a_non_existent_table() {
        let path = temp_db_path("create_index_no_table");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        let result = catalog.create_index(
            "idx_email",
            "ghost_table",
            vec![Column {
                name: "email".to_string(),
                c_type: DataType::Text(0),
            }],
            false,
            &mut pager,
        );
        assert!(result.is_err());
    }

    #[test]
    fn create_index_rejects_an_empty_column_list() {
        let path = temp_db_path("create_index_no_cols");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        catalog
            .create_table(
                "users",
                vec![Column {
                    name: "id".to_string(),
                    c_type: DataType::Int,
                }],
                &mut pager,
            )
            .unwrap();

        assert!(
            catalog
                .create_index("idx_empty", "users", vec![], false, &mut pager)
                .is_err()
        );
    }

    #[test]
    fn create_index_rejects_a_column_not_on_the_table() {
        let path = temp_db_path("create_index_bad_col");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        catalog
            .create_table(
                "users",
                vec![Column {
                    name: "id".to_string(),
                    c_type: DataType::Int,
                }],
                &mut pager,
            )
            .unwrap();

        let not_a_real_column = Column {
            name: "nonexistent".to_string(),
            c_type: DataType::Text(0),
        };
        assert!(
            catalog
                .create_index(
                    "idx_bad",
                    "users",
                    vec![not_a_real_column],
                    false,
                    &mut pager
                )
                .is_err()
        );
    }

    #[test]
    fn create_index_rejects_a_duplicate_index_name_even_on_a_different_table() {
        let path = temp_db_path("create_index_dup_name");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        let id_col = Column {
            name: "id".to_string(),
            c_type: DataType::Int,
        };
        catalog
            .create_table("users", vec![id_col.clone()], &mut pager)
            .unwrap();
        catalog
            .create_table("posts", vec![id_col.clone()], &mut pager)
            .unwrap();

        catalog
            .create_index("idx_id", "users", vec![id_col.clone()], false, &mut pager)
            .unwrap();

        // same index NAME, but on an unrelated table - index names must be globally
        // unique since `self.indexes` is keyed only by name, so this has to be
        // rejected rather than silently overwriting the first index's entry.
        assert!(
            catalog
                .create_index("idx_id", "posts", vec![id_col], false, &mut pager)
                .is_err()
        );
    }

    #[test]
    fn drop_index_removes_it_from_the_catalog_and_persists_after_reload() {
        let path = temp_db_path("drop_index");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        let id_col = Column {
            name: "id".to_string(),
            c_type: DataType::Int,
        };
        catalog
            .create_table("users", vec![id_col.clone()], &mut pager)
            .unwrap();
        catalog
            .create_index("idx_id", "users", vec![id_col], false, &mut pager)
            .unwrap();

        catalog.drop_index("idx_id", &mut pager).unwrap();
        assert!(!catalog.indexes.contains_key("idx_id"));

        let reloaded = Catalog::load(&mut pager).unwrap();
        assert!(!reloaded.indexes.contains_key("idx_id"));
    }

    #[test]
    fn drop_index_rejects_a_non_existent_index() {
        let path = temp_db_path("drop_index_missing");
        let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
        let mut catalog = empty_catalog();

        assert!(catalog.drop_index("ghost_index", &mut pager).is_err());
    }

    // Subproject 9's definition of done: create tables/an index through the
    // catalog API, actually close the file (drop the Pager, not just reuse the
    // same in-memory one), reopen it fresh, reload the catalog, and check both
    // that the schema came back correctly AND that the tables' own data is
    // still reachable via the root pages the reloaded catalog points at.
    #[test]
    fn full_close_and_reopen_cycle_preserves_schema_and_data() {
        let path = temp_db_path("full_persistence_cycle");

        {
            let mut pager = Pager::new(&path, TEST_PAGE_SIZE).unwrap();
            let mut catalog = empty_catalog();

            let id_col = Column {
                name: "id".to_string(),
                c_type: DataType::Int,
            };
            let email_col = Column {
                name: "email".to_string(),
                c_type: DataType::Text(0),
            };

            catalog
                .create_table("users", vec![id_col.clone(), email_col.clone()], &mut pager)
                .unwrap();
            catalog
                .create_table("posts", vec![id_col], &mut pager)
                .unwrap();
            catalog
                .create_index("idx_email", "users", vec![email_col], true, &mut pager)
                .unwrap();

            // populate the users table's own b-tree through its freshly
            // allocated root page, so the test can check that row data (not
            // just schema) survives the reopen.
            let users_root = catalog.tables.get("users").unwrap().root_pn;
            let mut users_cursor = TableCursor::new(&mut pager, users_root);
            users_cursor
                .insert(RowID::try_from(1).unwrap(), b"alice")
                .unwrap();
            users_cursor
                .insert(RowID::try_from(2).unwrap(), b"bob")
                .unwrap();

            // `pager` (and its open file handle) drops here - simulating
            // actually closing the database, not just finishing a method call.
        }

        let mut reopened_pager = Pager::init(&path).unwrap();
        let reloaded = Catalog::load(&mut reopened_pager).unwrap();

        assert_eq!(reloaded.tables.len(), 2);
        assert_eq!(reloaded.indexes.len(), 1);

        let users = reloaded
            .tables
            .get("users")
            .expect("users table missing after reopen");
        assert_eq!(users.columns.len(), 2);
        assert_eq!(users.columns[0].name, "id");
        assert_eq!(users.columns[1].name, "email");

        let posts = reloaded
            .tables
            .get("posts")
            .expect("posts table missing after reopen");
        assert_eq!(posts.columns.len(), 1);

        let idx = reloaded
            .indexes
            .get("idx_email")
            .expect("idx_email missing after reopen");
        assert_eq!(idx.table_name, "users");
        assert!(idx.unique);
        assert_eq!(idx.on_columns.len(), 1);
        assert_eq!(idx.on_columns[0].name, "email");

        // the row data inserted before closing must still be reachable
        // through the root page the reloaded catalog now points at.
        let mut users_cursor = TableCursor::new(&mut reopened_pager, users.root_pn);
        let rows = users_cursor.scan().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].local_payload, b"alice");
        assert_eq!(rows[1].local_payload, b"bob");
    }
}

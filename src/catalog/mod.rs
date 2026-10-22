/*
    maze_manager table structure:
    | obj_type | name | table_name | root_pn | sql |

    -> obj_type: either index or table, as these are the only two types of database objects (no views or triggers yet).
    -> name: if type == table: name == table_name, else if type == index, name = name of the index, table_name = name of the table the index is on.
    -> table_name: name of the table, or the index the table is on.
    -> root_pn: the page number of the root page of this database object.
    -> sql: the NORMALIZED SQL statement used to create this object.
*/

use crate::storage::{
    header::{MAZE_HEADER_SIZE, MazeHeader},
    pages::types::PageNumber,
    record::{Record, values::Value},
};

// GOTTA REDO THIS ENTIRE SHIT BECAUSE MAZEMANAGER NEEDS TO BE STORED IN A TABLE BTREE PAGE.

// pub struct MazeManager {
//     pub rows: Vec<MazeManagerRow>,
// }

// impl MazeManager {
//     pub fn parse(mut buf: &[u8]) -> anyhow::Result<Self> {
//         // this line is a check; if the buffer is not of page 1, it would not contain the database header, and this function won't parse further.
//         let _ = MazeHeader::try_from(buf)?;
//         buf = buf.get(MAZE_HEADER_SIZE..).ok_or_else(|| {
//             anyhow::anyhow!("i can't believe my eyes this error can't be happening")
//         })?;

//         let mut rows = Vec::new();
//         let (mut row, mut bytes_read) = MazeManagerRow::parse(buf)?;
//         while bytes_read < buf.len() {
//             rows.push(row);
//             buf = match buf.get(bytes_read..) {
//                 Some(b) => b,
//                 None => break,
//             };
//             (row, bytes_read) = MazeManagerRow::parse(b)?;
//         }

//         todo!()
//     }
// }

// pub struct MazeManagerRow {
//     pub obj_type: ObjectType,
//     pub name: String,
//     pub table_name: String,
//     pub root_pn: PageNumber,
//     pub sql: String,
// }

// impl MazeManagerRow {
//     pub fn parse(buf: &[u8]) -> anyhow::Result<(Self, usize)> {
//         let (record, bytes_read) = Record::decode(buf)?;

//         let (mut obj_type, mut root_pn) = (0u8, 0u32);
//         let (mut name, mut table_name, mut sql) = (String::new(), String::new(), String::new());
//         for (idx, value) in record.into_iter().enumerate() {
//             match value {
//                 Value::Null => anyhow::bail!("no column's value in maze_manager can be null"),
//                 Value::Float(_) => anyhow::bail!(
//                     "no column's value in maze_manager can be a floating point number"
//                 ),
//                 Value::Blob(_) => {
//                     anyhow::bail!("no column's value in maze_manager can be a blob")
//                 }

//                 Value::Int(i) if idx == 0 => obj_type = u8::try_from(i)?,
//                 Value::Text(t) if idx == 1 => name = t,
//                 Value::Text(t) if idx == 2 => table_name = t,
//                 Value::Int(i) if idx == 3 => root_pn = u32::try_from(i)?,
//                 Value::Text(t) if idx == 4 => sql = t,

//                 _ => anyhow::bail!(
//                     "column mismatch, invalid data in column {}",
//                     match idx {
//                         0 => "obj_type",
//                         1 => "name",
//                         2 => "table_name",
//                         3 => "root_pn",
//                         4 => "sql",
//                         _ => "error",
//                     }
//                 ),
//             }
//         }

//         let root_pn = PageNumber::try_from(root_pn)?;
//         let obj_type = ObjectType::try_from(obj_type)?;

//         Ok((
//             Self {
//                 obj_type,
//                 name,
//                 table_name,
//                 root_pn,
//                 sql,
//             },
//             bytes_read,
//         ))
//     }

//     pub fn serialize(&self) -> anyhow::Result<Vec<u8>> {
//         let record = Record::new(vec![
//             Value::Int(self.obj_type.encode() as i64),
//             Value::Text(self.name.clone()),
//             Value::Text(self.table_name.clone()),
//             Value::Int(self.root_pn.get() as i64),
//             Value::Text(self.sql.clone()),
//         ])?;
//         Ok(record.encode())
//     }
// }

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
            1 => Ok(ObjectType::Table),
            2 => Ok(ObjectType::Index),
            _ => anyhow::bail!("invalid object type"),
        }
    }
}

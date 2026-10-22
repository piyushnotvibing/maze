use crate::storage::pages::types::PageNumber;

pub mod check;
pub mod table_cursor;
pub mod index_cursor;

enum InsertOutcome<Key> {
    Done,
    Split {
        old_child: PageNumber,
        new_child: PageNumber,
        divider: Key,
    },
}

enum DeleteOutcome {
    Done,
    DeletedChild,
}
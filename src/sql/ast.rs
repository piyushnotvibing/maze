use crate::{
    sql::{Column, lexer::Literal},
    storage::record::values::Value,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    CreateTable {
        table: String,
        columns: Vec<Column>,
    },
    CreateIndex {
        index: String,
        table: String,
        on_columns: Vec<Column>,
        unique: bool,
    },

    /*
        If the insert query is like
        INSERT INTO employees VALUES (...);
        then into_columns = None and
        values.len() must match number of
        columns in the table.
    */
    Insert {
        table: String,
        into_columns: Option<Vec<Column>>,
        values: Vec<Value>,
    },

    Select {
        table: String,
        // SELECT name as EMP_NAME, age FROM ... is stored as Some({{name: EMP_NAME}, {age: age}})
        // If SELECT * FROM..., then Some({{"*": "*"}})
        select_columns: Vec<Column>,
        where_clause: Option<Expr>,
        order_by: Option<(Expr, SortDir)>,
        limit: Option<u64>,
    },

    Update {
        table: String,
        set: Vec<Assignment>,
        where_clause: Option<Expr>,
    },

    Delete {
        table: String,
        where_clause: Option<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Ident(String),
    Literal(Literal),
    Unary(UnaryOp, Box<Expr>),
    Binary(Box<Expr>, BinOp, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Lteq,
    Gt,
    Gteq,
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub column: Column,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectItems {
    pub column: Expr,
    // if Expr is a string literal, then alias = string literal
    // if Expr is a unary or binary op, then alias = stringified op (eg SELECT )
    pub alias: String,
    pub is_star: bool, 
}

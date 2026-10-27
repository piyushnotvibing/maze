use crate::{
    sql::lexer::{Literal, TypeName},
    storage::record::values::Value,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    CreateTable {
        table: String,
        columns: Vec<CreateTableColumn>,
    },
    CreateIndex {
        index: String,
        table: String,
        on_columns: Vec<CreateIndexColumn>,
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
        into_columns: Option<Vec<InsertColumn>>,
        values: Vec<Value>,
    },

    Select {
        table: String,
        // SELECT name as EMP_NAME, age FROM ... is stored as Some({{name: EMP_NAME}, {age: age}})
        // If SELECT * FROM..., then Some({{"*": "*"}})
        select_columns: Vec<SelectColumn>,
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

impl Expr {
    pub fn try_to_string(&self) -> anyhow::Result<String> {
        match self {
            Expr::Ident(id) => Ok(id.clone()),
            Expr::Literal(l) => match l {
                Literal::Int(i) => Ok(i.to_string()),
                Literal::Float(f) => Ok(f.to_string()),
                Literal::String(s) => Ok(s.clone()),
                Literal::Blob(items) => Ok(String::from_utf8_lossy(items).into_owned()),
                Literal::Null => Ok(String::from("NULL")),
            },
            Expr::Unary(op, expr) => Ok(format!("{} {}", op.to_string(), expr.try_to_string()?)),
            Expr::Binary(expr1, bin_op, expr2) => Ok(format!(
                "{} {} {}",
                expr1.try_to_string()?,
                bin_op.to_string(),
                expr2.try_to_string()?
            )),
        }
    }
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

impl ToString for BinOp {
    fn to_string(&self) -> String {
        match self {
            BinOp::Or => "OR".to_string(),
            BinOp::And => "AND".to_string(),
            BinOp::Eq => "=".to_string(),
            BinOp::Ne => "<>".to_string(),
            BinOp::Lt => "<".to_string(),
            BinOp::Lteq => "<=".to_string(),
            BinOp::Gt => ">".to_string(),
            BinOp::Gteq => ">=".to_string(),
            BinOp::Add => "+".to_string(),
            BinOp::Sub => "-".to_string(),
            BinOp::Mul => "*".to_string(),
            BinOp::Div => "/".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

impl ToString for UnaryOp {
    fn to_string(&self) -> String {
        match self {
            UnaryOp::Neg => "-".to_string(),
            UnaryOp::Not => "~".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    pub column: AssignedColumn,
    pub value: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SelectColumn {
    // SELECT age + salary FROM emp; is a valid SQL query, so
    // the column here has to be an expression.
    // Later on when I add aggregate functions this will
    // help massively, as they will simply be more enums of Expr
    pub name: Expr,
    // if Expr is a string literal, then alias = string literal
    // if Expr is a unary or binary op, then alias = stringified op (eg SELECT )
    pub alias: String,
    pub is_star: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreateTableColumn {
    pub name: String,
    pub c_type: TypeName,
    pub constraints: ColumnConstraints,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ColumnConstraints {
    None,
    PrimaryKey,
    Unique,
    NotNull,
    UniqueNotNull,
}

pub type InsertColumn = String;
pub type CreateIndexColumn = String;
pub type AssignedColumn = String;

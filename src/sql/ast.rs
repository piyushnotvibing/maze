use crate::sql::lexer::Literal;

enum Expr {
    Ident(String),
    Literal(Literal),
    Unary(UnaryOp, Box<Expr>),
    Binary(Box<Expr>, BinOp, Box<Expr>),
}
enum BinOp {
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
enum UnaryOp {
    Neg,
    Not,
}

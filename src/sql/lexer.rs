use crate::sql::ast::BinOp;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Create,
    Table,
    Index,
    On,
    Insert,
    Into,
    Select,
    Update,
    Delete,
    Primary,
    Key,
    Unique,
    Star,
    Values,
    From,
    Where,
    Order,
    By,
    As,
    Asc,
    Desc,
    Limit,
    Set,
    Or,
    And,
    Not,

    Id(String),
    DataType(TypeName),
    Literal(Literal),
    Operation(Operation),

    LParen,
    RParen,
    Comma,
    Semicolon,

    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Operation {
    Add, // +
    Sub, // -
    // STAR for mul
    Div,  // /
    Lt,   // <
    Lteq, // <=
    Gt,   // >
    Gteq, // >=
    Eq,   // =, used for comparisons as well as assignment
    Ne,   // != or <>
}

impl Operation {
    pub fn is_comp_op(&self) -> bool {
        match self {
            Operation::Lt
            | Operation::Lteq
            | Operation::Gt
            | Operation::Gteq
            | Operation::Eq
            | Operation::Ne => true,
            _ => false,
        }
    }

    pub fn to_bin_op(&self) -> BinOp {
        match self {
            // BinOp::Mul must be manually done.
            Operation::Add => BinOp::Add,
            Operation::Sub => BinOp::Sub,
            Operation::Div => BinOp::Div,
            Operation::Lt => BinOp::Lt,
            Operation::Lteq => BinOp::Lteq,
            Operation::Gt => BinOp::Gt,
            Operation::Gteq => BinOp::Gteq,
            Operation::Eq => BinOp::Eq,
            Operation::Ne => BinOp::Ne,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    String(String),
    Blob(Vec<u8>),
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeName {
    Integer,
    Real,
    Text,
    Blob,
}

pub fn tokenize(sql: String) -> anyhow::Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut characters = sql.chars().peekable();

    let mut identity = String::new();
    let mut numeric_literal = String::new();
    let mut string_literal = String::new();
    let mut blob_literal = String::new();

    let mut skip = false;
    let mut is_str_char = false;
    let mut is_blob_char = false;
    while let Some(char) = characters.next() {
        if skip {
            skip = false;
            continue;
        }
        let next = characters.peek();
        match char {
            ' ' if is_str_char && !is_blob_char => string_literal.push(' '),
            ' ' if !is_blob_char => push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?,
            '+' if is_str_char && !is_blob_char => string_literal.push('+'),
            '+' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                tokens.push(Token::Operation(Operation::Add));
            }
            '-' if is_str_char && !is_blob_char => string_literal.push('-'),
            '-' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                tokens.push(Token::Operation(Operation::Sub));
            }
            '*' if is_str_char && !is_blob_char => string_literal.push('*'),
            '*' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                tokens.push(Token::Star);
            }
            '/' if is_str_char && !is_blob_char => string_literal.push('/'),
            '/' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                tokens.push(Token::Operation(Operation::Div));
            }

            '(' if is_str_char && !is_blob_char => string_literal.push('('),
            '(' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                tokens.push(Token::LParen);
            }
            ')' if is_str_char && !is_blob_char => string_literal.push(')'),
            ')' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                tokens.push(Token::RParen);
            }
            ',' if is_str_char && !is_blob_char => string_literal.push(','),
            ',' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                tokens.push(Token::Comma);
            }
            ';' if is_str_char && !is_blob_char => string_literal.push(';'),
            ';' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                tokens.push(Token::Semicolon);
            }

            '>' if is_str_char && !is_blob_char => string_literal.push('>'),
            '>' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                if let Some(&next_char) = next
                    && next_char == '='
                {
                    tokens.push(Token::Operation(Operation::Gteq));
                    skip = true;
                } else {
                    tokens.push(Token::Operation(Operation::Gt));
                }
            }
            '=' if is_str_char => string_literal.push('='),
            '=' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                tokens.push(Token::Operation(Operation::Eq))
            }
            '<' if is_str_char && !is_blob_char => string_literal.push('<'),
            '<' if !is_blob_char => {
                push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                if let Some(&next_char) = next {
                    match next_char {
                        '=' => {
                            tokens.push(Token::Operation(Operation::Lteq));
                            skip = true;
                        }
                        '>' => {
                            tokens.push(Token::Operation(Operation::Ne));
                            skip = true;
                        }
                        _ => tokens.push(Token::Operation(Operation::Lt)),
                    }
                } else {
                    tokens.push(Token::Operation(Operation::Lt));
                }
            }
            '!' if is_str_char && !is_blob_char => string_literal.push('!'),
            '!' if !is_blob_char => {
                if let Some(&next_char) = next
                    && next_char == '='
                {
                    push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;
                    tokens.push(Token::Operation(Operation::Ne));
                    skip = true;
                } else {
                    anyhow::bail!("lexer error: invalid character !")
                }
            }
            '\'' => {
                if let Some(&next_char) = next
                    && next_char == '\''
                {
                    string_literal.push('\'');
                    skip = true;
                    continue;
                }

                if is_blob_char && !blob_literal.is_empty() {
                    tokens.push(Token::Literal(Literal::Blob(
                        blob_literal.as_bytes().to_vec(),
                    )));
                    is_blob_char = false;
                    blob_literal.clear();
                }
                // if string_literal is not empty, then this is the closing single quote
                else if is_str_char && !string_literal.is_empty() {
                    tokens.push(Token::Literal(Literal::String(string_literal.clone())));
                    is_str_char = false;
                    string_literal.clear();
                } else {
                    is_str_char = true;
                }
            }
            'X' if !is_str_char
                && !is_blob_char
                && (next.is_some_and(|&c| c == '\'') && blob_literal.is_empty()) =>
            {
                if let Some(&next_char) = next
                    && next_char == '\''
                {
                    is_blob_char = true;
                    skip = true;
                }
            }
            c => {
                if is_blob_char {
                    blob_literal.push(c);
                    continue;
                }
                if is_str_char {
                    string_literal.push(c);
                    continue;
                }
                // Since .x can be parsed by rust into 0.x, we will allow it
                if c == '.' {
                    if let Some(next_char) = next {
                        let period_invalid = period_already_in_literal(&numeric_literal)
                            || a_number_does_not_follow_period(next_char);
                        if period_invalid {
                            anyhow::bail!("lexer error: unexpected period")
                        }
                    } else {
                        anyhow::bail!("lexer error: unexpected period")
                    }
                }

                if c.is_ascii_alphabetic()
                    || c == '_'
                    || (c.is_ascii_digit() && !identity.is_empty())
                {
                    identity.push(c);
                } else if c.is_ascii_digit() || c == '.' {
                    numeric_literal.push(c);
                } else {
                    anyhow::bail!("lexer: invalid character {c} used")
                }
            }
        }
    }
    if is_str_char {
        anyhow::bail!("lexer error: unterminated string literal");
    }
    push_buffers(&mut identity, &mut numeric_literal, &mut tokens)?;

    tokens.push(Token::Eof);
    Ok(tokens)
}

fn lookup_sql_keyword(literal: &str) -> Option<Token> {
    match literal {
        "create" | "CREATE" => Some(Token::Create),
        "table" | "TABLE" => Some(Token::Table),
        "index" | "INDEX" => Some(Token::Index),
        "on" | "ON" => Some(Token::On),
        "insert" | "INSERT" => Some(Token::Insert),
        "into" | "INTO" => Some(Token::Into),
        "select" | "SELECT" => Some(Token::Select),
        "update" | "UPDATE" => Some(Token::Update),
        "delete" | "DELETE" => Some(Token::Delete),
        "primary" | "PRIMARY" => Some(Token::Primary),
        "key" | "KEY" => Some(Token::Key),
        "unique" | "UNIQUE" => Some(Token::Unique),
        "*" => Some(Token::Star),
        "values" | "VALUES" => Some(Token::Values),
        "from" | "FROM" => Some(Token::From),
        "where" | "WHERE" => Some(Token::Where),
        "order" | "ORDER" => Some(Token::Order),
        "by" | "BY" => Some(Token::By),
        "as" | "AS" => Some(Token::As),
        "asc" | "ASC" => Some(Token::Asc),
        "desc" | "DESC" => Some(Token::Desc),
        "limit" | "LIMIT" => Some(Token::Limit),
        "set" | "SET" => Some(Token::Set),
        "or" | "OR" => Some(Token::Or),
        "and" | "AND" => Some(Token::And),
        "not" | "NOT" => Some(Token::Not),

        "INTEGER" => Some(Token::DataType(TypeName::Integer)),
        "REAL" => Some(Token::DataType(TypeName::Real)),
        "TEXT" => Some(Token::DataType(TypeName::Text)),
        "BLOB" => Some(Token::DataType(TypeName::Blob)),

        "null" | "NULL" => Some(Token::Literal(Literal::Null)),
        _ => None,
    }
}

#[inline(always)]
fn period_already_in_literal(literal: &str) -> bool {
    !literal.is_empty() && literal.contains('.')
}

#[inline(always)]
fn a_number_does_not_follow_period(next_char: &char) -> bool {
    !['0', '1', '2', '3', '4', '5', '6', '7', '8', '9'].contains(next_char)
}

fn parse_and_push_i64(numeric_literal: &str, tokens: &mut Vec<Token>) -> anyhow::Result<bool> {
    if numeric_literal.contains(".") {
        return Ok(false);
    }
    if !numeric_literal.is_empty() {
        match numeric_literal.parse::<i64>() {
            Ok(i) => tokens.push(Token::Literal(Literal::Int(i))),
            Err(e) => anyhow::bail!("lexer error: failed to parse {numeric_literal} as i64: {e}"),
        }
    }
    Ok(true)
}

fn parse_and_push_f64(numeric_literal: &str, tokens: &mut Vec<Token>) -> anyhow::Result<bool> {
    if !numeric_literal.contains(".") {
        return Ok(false);
    }
    if !numeric_literal.is_empty() {
        match numeric_literal.parse::<f64>() {
            Ok(f) => {
                if f.is_subnormal() || f.is_nan() || f.is_infinite() {
                    anyhow::bail!("lexer error: parsed real number {f} is NaN, infinite or invalid")
                }
                tokens.push(Token::Literal(Literal::Float(f)))
            }
            Err(e) => anyhow::bail!("lexer error: failed to parse {numeric_literal} as f64: {e}"),
        }
    }
    Ok(true)
}

fn push_buffers(
    identity: &mut String,
    numeric_literal: &mut String,
    tokens: &mut Vec<Token>,
) -> anyhow::Result<()> {
    if !numeric_literal.is_empty() {
        if !parse_and_push_i64(&numeric_literal, tokens)?
            && !parse_and_push_f64(&numeric_literal, tokens)?
        {
            anyhow::bail!("lexer error: {numeric_literal:?} does not contain numeric characters")
        }
        numeric_literal.clear();
    }
    if !identity.is_empty() {
        if let Some(keyword_token) = lookup_sql_keyword(&identity) {
            tokens.push(keyword_token);
        } else {
            tokens.push(Token::Id(identity.clone()));
        }
        identity.clear();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(sql: &str) -> anyhow::Result<Vec<Token>> {
        tokenize(sql.to_string())
    }

    // ---- accept: normal statements, one of each kind ----

    #[test]
    fn select_with_where_order_by_limit_alias_and_escaped_string() {
        let tokens = tok(
            "SELECT name AS unc FROM employees WHERE age >= 30.6 AND name = 'Piyush Udhao''s Dog' ORDER BY name LIMIT 10;",
        )
        .unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::Id("name".to_string()),
                Token::As,
                Token::Id("unc".to_string()),
                Token::From,
                Token::Id("employees".to_string()),
                Token::Where,
                Token::Id("age".to_string()),
                Token::Operation(Operation::Gteq),
                Token::Literal(Literal::Float(30.6)),
                Token::And,
                Token::Id("name".to_string()),
                Token::Operation(Operation::Eq),
                Token::Literal(Literal::String("Piyush Udhao's Dog".to_string())),
                Token::Order,
                Token::By,
                Token::Id("name".to_string()),
                Token::Limit,
                Token::Literal(Literal::Int(10)),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn select_star_without_where() {
        let tokens = tok("SELECT * FROM employees;").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::Star,
                Token::From,
                Token::Id("employees".to_string()),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn select_with_arithmetic_in_projection() {
        let tokens = tok("SELECT salary + bonus FROM employees;").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::Id("salary".to_string()),
                Token::Operation(Operation::Add),
                Token::Id("bonus".to_string()),
                Token::From,
                Token::Id("employees".to_string()),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn create_table_with_multiple_typed_columns() {
        let tokens = tok("CREATE TABLE employees (id INTEGER, name TEXT);").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Create,
                Token::Table,
                Token::Id("employees".to_string()),
                Token::LParen,
                Token::Id("id".to_string()),
                Token::DataType(TypeName::Integer),
                Token::Comma,
                Token::Id("name".to_string()),
                Token::DataType(TypeName::Text),
                Token::RParen,
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn create_unique_index_on_table() {
        let tokens = tok("CREATE UNIQUE INDEX idx_salary ON employees (salary);").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Create,
                Token::Unique,
                Token::Index,
                Token::Id("idx_salary".to_string()),
                Token::On,
                Token::Id("employees".to_string()),
                Token::LParen,
                Token::Id("salary".to_string()),
                Token::RParen,
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn insert_with_explicit_column_list_and_values() {
        let tokens = tok("INSERT INTO employees (id, name) VALUES (1, 'Ada');").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Insert,
                Token::Into,
                Token::Id("employees".to_string()),
                Token::LParen,
                Token::Id("id".to_string()),
                Token::Comma,
                Token::Id("name".to_string()),
                Token::RParen,
                Token::Values,
                Token::LParen,
                Token::Literal(Literal::Int(1)),
                Token::Comma,
                Token::Literal(Literal::String("Ada".to_string())),
                Token::RParen,
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn update_with_set_and_where() {
        let tokens = tok("UPDATE employees SET name = 'Bob' WHERE id = 1;").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Update,
                Token::Id("employees".to_string()),
                Token::Set,
                Token::Id("name".to_string()),
                Token::Operation(Operation::Eq),
                Token::Literal(Literal::String("Bob".to_string())),
                Token::Where,
                Token::Id("id".to_string()),
                Token::Operation(Operation::Eq),
                Token::Literal(Literal::Int(1)),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn delete_with_where() {
        let tokens = tok("DELETE FROM employees WHERE id = 1;").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Delete,
                Token::From,
                Token::Id("employees".to_string()),
                Token::Where,
                Token::Id("id".to_string()),
                Token::Operation(Operation::Eq),
                Token::Literal(Literal::Int(1)),
                Token::Semicolon,
                Token::Eof,
            ]
        );
    }

    // ---- accept: narrow regressions for specific lexer behaviors ----

    #[test]
    fn identifier_with_underscore_is_a_single_token() {
        assert_eq!(
            tok("my_table").unwrap(),
            vec![Token::Id("my_table".to_string()), Token::Eof]
        );
    }

    #[test]
    fn identifier_with_trailing_digit_is_a_single_token() {
        assert_eq!(
            tok("col1").unwrap(),
            vec![Token::Id("col1".to_string()), Token::Eof]
        );
    }

    #[test]
    fn comparison_operator_without_surrounding_whitespace() {
        assert_eq!(
            tok("age>=30").unwrap(),
            vec![
                Token::Id("age".to_string()),
                Token::Operation(Operation::Gteq),
                Token::Literal(Literal::Int(30)),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn comma_and_parens_without_whitespace_still_separate_identifiers() {
        assert_eq!(
            tok("(a,b)").unwrap(),
            vec![
                Token::LParen,
                Token::Id("a".to_string()),
                Token::Comma,
                Token::Id("b".to_string()),
                Token::RParen,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn trailing_identifier_without_a_separator_is_not_truncated() {
        assert_eq!(
            tok("FROM employees").unwrap(),
            vec![Token::From, Token::Id("employees".to_string()), Token::Eof,]
        );
    }

    #[test]
    fn trailing_number_without_a_separator_is_not_truncated() {
        assert_eq!(
            tok("LIMIT 10").unwrap(),
            vec![Token::Limit, Token::Literal(Literal::Int(10)), Token::Eof]
        );
    }

    #[test]
    fn string_literal_containing_punctuation_is_preserved_verbatim() {
        assert_eq!(
            tok("'hello, world (bye)'").unwrap(),
            vec![
                Token::Literal(Literal::String("hello, world (bye)".to_string())),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn type_name_keywords_tokenize_as_data_type_not_identifier() {
        assert_eq!(
            tok("INTEGER TEXT REAL BLOB").unwrap(),
            vec![
                Token::DataType(TypeName::Integer),
                Token::DataType(TypeName::Text),
                Token::DataType(TypeName::Real),
                Token::DataType(TypeName::Blob),
                Token::Eof,
            ]
        );
    }

    // ---- reject: invalid characters, or valid characters in the wrong place ----

    #[test]
    fn rejects_an_unterminated_string_literal() {
        assert!(tok("SELECT * FROM t WHERE name = 'abc").is_err());
    }

    #[test]
    fn rejects_an_unrecognized_character() {
        assert!(tok("SELECT @ FROM t").is_err());
    }

    #[test]
    fn rejects_a_standalone_bang_not_followed_by_equals() {
        assert!(tok("SELECT * FROM t WHERE id ! 5").is_err());
    }

    #[test]
    fn rejects_a_trailing_period_with_no_digits_after_it() {
        assert!(tok("SELECT 12. FROM t").is_err());
    }

    #[test]
    fn rejects_a_number_with_two_decimal_points() {
        assert!(tok("SELECT 1.2.3 FROM t").is_err());
    }
}

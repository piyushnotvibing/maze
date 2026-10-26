use std::collections::HashMap;

use crate::{
    sql::{
        Column,
        ast::{Assignment, BinOp, Expr, SortDir, Stmt, UnaryOp},
        lexer::{Literal, Operation, Token, TypeName},
    },
    storage::record::values::Value,
};

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> anyhow::Result<Token> {
        let token = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?;
        let token = token.clone();
        self.pos += 1;
        Ok(token)
    }

    fn expect(&mut self, expected_token: Token) -> anyhow::Result<()> {
        let token = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?;
        if *token == expected_token {
            self.pos += 1;
            Ok(())
        } else {
            anyhow::bail!("parser error: expected {expected_token:?} but got {token:?}")
        }
    }

    /*============================ Grammar Functions ============================*/

    // Entry point.
    fn parse_statements(&mut self) -> anyhow::Result<Vec<Stmt>> {
        let mut stmts = Vec::new();

        let stmt1 = self.parse_statement()?;
        if stmt1.is_none() {
            anyhow::bail!("parser error: this is literally a blank sql string")
        }
        let _ = self.advance()?;
        stmts.push(stmt1.unwrap());

        while let t = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            && *t == Token::Semicolon
        {
            let _ = self.advance()?;
            let stmt = self.parse_statement()?;
            if stmt.is_none() {
                break;
            }
            stmts.push(stmt.unwrap());
        }
        self.expect(Token::Semicolon)?;

        Ok(stmts)
    }

    fn parse_statement(&mut self) -> anyhow::Result<Option<Stmt>> {
        match self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
        {
            Token::Create => {
                let _ = self.advance()?;
                Ok(Some(self.parse_create()?))
            }
            Token::Insert => {
                let _ = self.advance()?;
                Ok(Some(self.parse_insert()?))
            }
            Token::Select => {
                let _ = self.advance()?;
                Ok(Some(self.parse_select()?))
            }
            Token::Update => {
                let _ = self.advance()?;
                Ok(Some(self.parse_update()?))
            }
            Token::Delete => {
                let _ = self.advance()?;
                Ok(Some(self.parse_delete()?))
            }
            Token::Eof => Ok(None),
            t => anyhow::bail!(
                "parser error: expected CREATE/INSERT/SELECT/UPDATE/DELETE, found {t:?}"
            ),
        }
    }

    fn parse_create(&mut self) -> anyhow::Result<Stmt> {
        match self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
        {
            Token::Table => {
                let _ = self.advance()?;
                let table =
                    extract_id(self.peek().ok_or_else(|| {
                        anyhow::anyhow!("parser error: parser exhausted tokens")
                    })?)?;
                let _ = self.advance()?;
                self.expect(Token::LParen)?;
                let columns = self.parse_column_def()?;
                self.expect(Token::RParen)?;

                Ok(Stmt::CreateTable { table, columns })
            }
            Token::Unique => {
                let _ = self.advance()?;
                self.expect(Token::Index)?;
                let unique = true;
                Ok(self.parse_index(unique)?)
            }
            Token::Index => {
                let _ = self.advance()?;
                let unique = false;
                Ok(self.parse_index(unique)?)
            }
            t => anyhow::bail!("parser error: expected TABLE or INDEX, found {t:?}"),
        }
    }

    fn parse_index(&mut self, unique: bool) -> anyhow::Result<Stmt> {
        let index_name = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        self.expect(Token::On)?;
        let table = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        self.expect(Token::LParen)?;
        let on_columns = self.parse_columns()?;
        self.expect(Token::RParen)?;

        Ok(Stmt::CreateIndex {
            index: index_name,
            table,
            on_columns,
            unique,
        })
    }

    fn parse_insert(&mut self) -> anyhow::Result<Stmt> {
        self.expect(Token::Into)?;
        let table = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        let mut into_columns: Option<Vec<Column>> = None;
        if *self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            == Token::LParen
        {
            let _ = self.advance()?;
            into_columns = Some(self.parse_columns()?);
            self.expect(Token::RParen)?;
        }
        self.expect(Token::Values)?;
        self.expect(Token::LParen)?;
        let values = self.parse_values()?;
        self.expect(Token::RParen)?;

        Ok(Stmt::Insert {
            table,
            into_columns,
            values,
        })
    }

    fn parse_select(&mut self) -> anyhow::Result<Stmt> {
        let select_columns = if *self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            == Token::Star
        {
            let mut map = HashMap::new();
            map.insert(
                Column {
                    name: "*".to_string(),
                    c_type: None,
                },
                Column {
                    name: "*".to_string(),
                    c_type: None,
                },
            );
            map
        } else {
            self.parse_select_columns()?
        };
        self.expect(Token::From)?;
        let table = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;

        let where_clause = self.parse_where_clause()?;
        let order_by = self.parse_order_by()?;
        let limit = self.parse_limit()?;

        Ok(Stmt::Select {
            table,
            select_columns,
            where_clause,
            order_by,
            limit,
        })
    }

    fn parse_update(&mut self) -> anyhow::Result<Stmt> {
        let table = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        self.expect(Token::Set)?;
        let set = self.parse_assignment()?;
        let _ = self.advance()?;
        let where_clause = self.parse_where_clause()?;

        Ok(Stmt::Update {
            table,
            set,
            where_clause,
        })
    }

    fn parse_delete(&mut self) -> anyhow::Result<Stmt> {
        self.expect(Token::From)?;
        let table = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        let where_clause = self.parse_where_clause()?;

        Ok(Stmt::Delete {
            table,
            where_clause,
        })
    }

    /// Parses CREATE ... VALUES ( [[id INT PRIMARY KEY, name TEXT NOT NULL, age INT, salary REAL, passport_photo BLOB UNIQUE]] )
    fn parse_column_def(&mut self) -> anyhow::Result<Vec<Column>> {
        let mut columns = Vec::new();

        let first_col_name = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        let first_col_type = extract_type(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        columns.push(Column {
            name: first_col_name,
            c_type: Some(first_col_type),
        });

        while let t = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            && *t == Token::Comma
        {
            let _ = self.advance()?;
            let name = extract_id(
                self.peek()
                    .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
            )?;
            let _ = self.advance()?;
            let c_type = Some(extract_type(self.peek().ok_or_else(|| {
                anyhow::anyhow!("parser error: parser exhausted tokens")
            })?)?);
            let _ = self.advance()?;

            columns.push(Column { name, c_type });
        }

        Ok(columns)
    }

    /// Parses SELECT [[name, age AS unc, dob as DateOfBirth]]... as {{name: name}, {age: unc}, {dob: DateOfBirth}}.
    /// Advances the parser position after parsing the last column.
    fn parse_select_columns(&mut self) -> anyhow::Result<HashMap<Column, Column>> {
        let mut columns: HashMap<Column, Column> = HashMap::new();

        let first_col = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        let alias = if let t = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            && *t == Token::As
        {
            let alias = extract_id(
                self.peek()
                    .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
            )?;
            let _ = self.advance()?;
            alias
        } else {
            first_col.clone()
        };
        if first_col == "*"
        {
            anyhow::bail!(
                "parser error: * captures all columns, so adding other columns after * in a SELECT statement makes no sense"
            )
        }
        columns.insert(
            Column {
                name: first_col,
                c_type: None,
            },
            Column {
                name: alias,
                c_type: None,
            },
        );

        while let t = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            && *t == Token::Comma
        {
            let _ = self.advance()?;
            let col = extract_id(
                self.peek()
                    .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
            )?;
            let _ = self.advance()?;
            let alias = if let t = self
                .peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
                && *t == Token::As
            {
                let alias =
                    extract_id(self.peek().ok_or_else(|| {
                        anyhow::anyhow!("parser error: parser exhausted tokens")
                    })?)?;
                let _ = self.advance()?;
                alias
            } else {
                col.clone()
            };
            columns.insert(
                Column {
                    name: col,
                    c_type: None,
                },
                Column {
                    name: alias,
                    c_type: None,
                },
            );
        }

        Ok(columns)
    }

    /// Parses INSERT INTO table ( [[name, age]] )...
    /// Advances the parser position after parsing the last column.
    fn parse_columns(&mut self) -> anyhow::Result<Vec<Column>> {
        let mut columns = Vec::new();

        let first_col_name = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        columns.push(Column {
            name: first_col_name,
            c_type: None,
        });

        while let t = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            && *t == Token::Comma
        {
            let _ = self.advance()?;
            let name = extract_id(
                self.peek()
                    .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
            )?;
            let _ = self.advance()?;

            columns.push(Column { name, c_type: None });
        }

        Ok(columns)
    }

    /// Parses INSERT ... VALUES ( [[10, 'peter parker', 25, 50000.55, X'asidjsa2112321le12jkkjbk3j1h4j1k23']] )
    /// Advances the parser position after parsing the last value.
    fn parse_values(&mut self) -> anyhow::Result<Vec<Value>> {
        let mut values = Vec::new();

        let first_val = match self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
        {
            Token::Literal(l) => match l {
                Literal::Int(i) => Value::Int(*i),
                Literal::Float(f) => Value::Float(*f),
                Literal::String(t) => Value::Text(t.clone()),
                Literal::Blob(b) => Value::Blob(b.clone()),
                Literal::Null => Value::Null,
            },
            t => anyhow::bail!("parser error: expected a literal, found {t:?}"),
        };
        let _ = self.advance()?;
        values.push(first_val);

        while let t = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            && *t == Token::Comma
        {
            let _ = self.advance()?;
            let val = match self
                .peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            {
                Token::Literal(l) => match l {
                    Literal::Int(i) => Value::Int(*i),
                    Literal::Float(f) => Value::Float(*f),
                    Literal::String(t) => Value::Text(t.clone()),
                    Literal::Blob(b) => Value::Blob(b.clone()),
                    Literal::Null => Value::Null,
                },
                t => anyhow::bail!("parser error: expected a literal, found {t:?}"),
            };
            let _ = self.advance()?;

            values.push(val);
        }

        Ok(values)
    }

    /// Parses WHERE [[name = 'peter parker' AND/OR age <> 25 AND salary > 25000.67]]
    /// Does not advance the parser position after parsing the last value.
    fn parse_where_clause(&mut self) -> anyhow::Result<Option<Expr>> {
        match self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
        {
            Token::Where => {
                let _ = self.advance()?;
                let res = Ok(Some(self.parse_expr()?));
                let _ = self.advance()?;
                res
            }
            _ => Ok(None),
        }
    }

    /// Parses ORDER BY [[salary ASC/DESC]]
    /// Does NOT advance the parser position after parsing the column.
    fn parse_order_by(&mut self) -> anyhow::Result<Option<(Expr, SortDir)>> {
        match self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
        {
            Token::Order => {
                let _ = self.advance()?;
                match self
                    .peek()
                    .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
                {
                    Token::By => {
                        let _ = self.advance()?;
                        let identity = extract_id(self.peek().ok_or_else(|| {
                            anyhow::anyhow!("parser error: parser exhausted tokens")
                        })?)?;
                        let _ = self.advance();
                        match self.peek().ok_or_else(|| {
                            anyhow::anyhow!("parser error: parser exhausted tokens")
                        })? {
                            Token::Asc => {
                                let _ = self.advance()?;
                                Ok(Some((Expr::Ident(identity), SortDir::Asc)))
                            }
                            Token::Desc => {
                                let _ = self.advance()?;
                                Ok(Some((Expr::Ident(identity), SortDir::Desc)))
                            }
                            t => anyhow::bail!("parser error: expected ASC/DESC, found {t:?}"),
                        }
                    }
                    t => anyhow::bail!("parser error: expected BY, found {t:?}"),
                }
            }
            _ => Ok(None),
        }
    }

    /// Parses LIMIT [[10]]
    /// Does not advance the parser position after parsing the integer.
    fn parse_limit(&mut self) -> anyhow::Result<Option<u64>> {
        match self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
        {
            Token::Limit => {
                let _ = self.advance()?;
                match self
                    .peek()
                    .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
                {
                    Token::Literal(l) => match l {
                        Literal::Int(i) => {
                            if i.is_negative() {
                                anyhow::bail!(
                                    "parser error: this error should never occur but still, cannot limit by negative numbers bro"
                                )
                            }
                            let res = Ok(Some(u64::try_from(*i)?));
                            let _ = self.advance()?;
                            res
                        }
                        _ => anyhow::bail!(
                            "parser error: expected an integer literal, found literal {l:?}"
                        ),
                    },
                    t => anyhow::bail!("parser error: expected an integer literal, found {t:?}"),
                }
            }
            _ => Ok(None),
        }
    }

    /// Parses UPDATE table SET [[name = 'Peter Parker', age = 25]]...
    /// Does not advance the parser position after parsing the last value.
    /// Note: Assignment in SET clauses and Comparison in WHERE clauses are different,
    /// and are parsed differently.
    fn parse_assignment(&mut self) -> anyhow::Result<Vec<Assignment>> {
        let mut assignments = Vec::new();

        let identity = extract_id(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let _ = self.advance()?;
        self.expect(Token::Operation(Operation::Eq))?;
        let expr = self.parse_expr()?;
        let _ = self.advance()?;
        assignments.push(Assignment {
            column: Column {
                name: identity,
                c_type: None,
            },
            value: expr,
        });

        while let t = self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            && *t == Token::Comma
        {
            let _ = self.advance()?;
            let identity = extract_id(
                self.peek()
                    .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
            )?;
            let _ = self.advance()?;
            self.expect(Token::Operation(Operation::Eq))?;
            let expr = self.parse_expr()?;
            let _ = self.advance()?;
            assignments.push(Assignment {
                column: Column {
                    name: identity,
                    c_type: None,
                },
                value: expr,
            });
        }

        Ok(assignments)
    }

    fn parse_expr(&mut self) -> anyhow::Result<Expr> {
        self.parse_or_expr()
    }

    fn parse_or_expr(&mut self) -> anyhow::Result<Expr> {
        let and_expr = self.parse_and_expr()?;
        self.advance()?;

        if *self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            == Token::Or
        {
            let _ = self.advance()?;
            let and_expr2 = self.parse_and_expr()?;
            return Ok(Expr::Binary(
                Box::new(and_expr),
                BinOp::And,
                Box::new(and_expr2),
            ));
        }

        Ok(and_expr)
    }

    fn parse_and_expr(&mut self) -> anyhow::Result<Expr> {
        let not_expr = self.parse_not_expr()?;
        self.advance()?;

        if *self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            == Token::Or
        {
            let _ = self.advance()?;
            let not_expr2 = self.parse_not_expr()?;
            return Ok(Expr::Binary(
                Box::new(not_expr),
                BinOp::And,
                Box::new(not_expr2),
            ));
        }

        Ok(not_expr)
    }

    fn parse_not_expr(&mut self) -> anyhow::Result<Expr> {
        self.expect(Token::Not)?;
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> anyhow::Result<Expr> {
        let arith_expr1 = self.parse_arith_expr()?;
        let _ = self.advance()?;
        let op1 = extract_comp_op(
            self.peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?,
        )?;
        let arith_expr2 = self.parse_arith_expr()?;
        let _ = self.advance()?;

        let mut expr = Expr::Binary(
            Box::new(arith_expr1),
            op1.to_bin_op(),
            Box::new(arith_expr2),
        );

        loop {
            let token = self
                .peek()
                .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?;
            if !is_comp_op(token) {
                break;
            }

            let op = extract_comp_op(token)?;
            if !op.is_comp_op() {
                anyhow::bail!("parser error: expected a comparison operator, found {op1:?}")
            }
            let _ = self.advance()?;
            let arith_expr = self.parse_arith_expr()?;
            let _ = self.advance()?;

            expr = Expr::Binary(Box::new(expr), op.to_bin_op(), Box::new(arith_expr))
        }
        let _ = self.advance()?;

        Ok(expr)
    }

    fn parse_arith_expr(&mut self) -> anyhow::Result<Expr> {
        let term1 = self.parse_term()?;
        let _ = self.advance()?;
        let op = match self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
        {
            Token::Operation(o) => match o {
                Operation::Add => o.to_bin_op(),
                Operation::Sub => o.to_bin_op(),
                _ => anyhow::bail!("parser error: expected + or -, found {o:?}"),
            },
            t => anyhow::bail!("parser error: expected + or -, found {t:?}"),
        };

        let term2 = self.parse_term()?;

        Ok(Expr::Binary(Box::new(term1), op, Box::new(term2)))
    }

    fn parse_term(&mut self) -> anyhow::Result<Expr> {
        let factor1 = self.parse_factor()?;
        let _ = self.advance()?;
        let op = match self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
        {
            Token::Operation(o) => match o {
                Operation::Div => o.to_bin_op(),
                _ => anyhow::bail!("parser error: expected * or /, found {o:?}"),
            },
            Token::Star => BinOp::Mul,
            t => anyhow::bail!("parser error: expected * or /, found {t:?}"),
        };

        let factor2 = self.parse_factor()?;

        Ok(Expr::Binary(Box::new(factor1), op, Box::new(factor2)))
    }

    fn parse_factor(&mut self) -> anyhow::Result<Expr> {
        if *self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
            == Token::Operation(Operation::Sub)
        {
            let sub = UnaryOp::Neg;
            let _ = self.advance()?;
            let primary = self.parse_primary()?;
            return Ok(Expr::Unary(sub, Box::new(primary)));
        }

        self.parse_primary()
    }

    fn parse_primary(&mut self) -> anyhow::Result<Expr> {
        match self
            .peek()
            .ok_or_else(|| anyhow::anyhow!("parser error: parser exhausted tokens"))?
        {
            Token::Id(s) => Ok(Expr::Ident(s.clone())),
            Token::Literal(l) => Ok(Expr::Literal(l.clone())),
            Token::LParen => {
                let _ = self.advance()?;
                let expr = self.parse_expr()?;
                let _ = self.advance()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            t => anyhow::bail!(
                "parser error: expected to find an identity, literal or an expression: found {t:?}"
            ),
        }
    }
}

pub fn parse(tokens: Vec<Token>) -> anyhow::Result<Vec<Stmt>> {
    let mut parser = Parser::new(tokens);
    parser.parse_statements()
}

fn extract_id(token: &Token) -> anyhow::Result<String> {
    match token {
        Token::Id(s) => Ok(s.to_owned()),
        t => anyhow::bail!("parser error: expected identity, found {t:?}"),
    }
}

fn extract_type(token: &Token) -> anyhow::Result<TypeName> {
    match token {
        Token::DataType(t) => Ok(*t),
        _ => anyhow::bail!("parser error: expected a data type, found {token:?}"),
    }
}

fn extract_comp_op(token: &Token) -> anyhow::Result<Operation> {
    match token {
        Token::Operation(o) => match o {
            Operation::Lt
            | Operation::Lteq
            | Operation::Gt
            | Operation::Gteq
            | Operation::Eq
            | Operation::Ne => Ok(*o),
            _ => anyhow::bail!("parser error: expected a comparison operation, found {o:?}"),
        },
        t => anyhow::bail!("parser error: expected a comparison operation, found {t:?}"),
    }
}

fn is_comp_op(token: &Token) -> bool {
    *token == Token::Operation(Operation::Lt)
        || *token == Token::Operation(Operation::Lteq)
        || *token == Token::Operation(Operation::Gt)
        || *token == Token::Operation(Operation::Gteq)
        || *token == Token::Operation(Operation::Eq)
        || *token == Token::Operation(Operation::Ne)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::lexer::tokenize;

    fn parse_sql(sql: &str) -> anyhow::Result<Vec<Stmt>> {
        parse(tokenize(sql.to_string())?)
    }

    fn parse_expr_fragment(fragment: &str) -> anyhow::Result<Expr> {
        let tokens = tokenize(fragment.to_string())?;
        Parser::new(tokens).parse_expr()
    }

    fn col(name: &str, c_type: Option<TypeName>) -> Column {
        Column {
            name: name.to_string(),
            c_type,
        }
    }

    fn select_cols(pairs: &[(&str, &str)]) -> HashMap<Column, Column> {
        pairs
            .iter()
            .map(|(name, alias)| (col(name, None), col(alias, None)))
            .collect()
    }

    fn star_cols() -> HashMap<Column, Column> {
        let mut m = HashMap::new();
        m.insert(col("*", None), col("*", None));
        m
    }

    fn ident(s: &str) -> Expr {
        Expr::Ident(s.to_string())
    }

    fn int(i: i64) -> Expr {
        Expr::Literal(Literal::Int(i))
    }

    fn bin(l: Expr, op: BinOp, r: Expr) -> Expr {
        Expr::Binary(Box::new(l), op, Box::new(r))
    }

    // ---- CREATE TABLE ----

    #[test]
    fn create_table_single_column() {
        let stmts = parse_sql("CREATE TABLE t (id INTEGER);").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::CreateTable {
                table: "t".to_string(),
                columns: vec![col("id", Some(TypeName::Integer))],
            }]
        );
    }

    #[test]
    fn create_table_multiple_typed_columns() {
        let stmts =
            parse_sql("CREATE TABLE employees (id INTEGER, name TEXT, salary REAL, photo BLOB);")
                .unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::CreateTable {
                table: "employees".to_string(),
                columns: vec![
                    col("id", Some(TypeName::Integer)),
                    col("name", Some(TypeName::Text)),
                    col("salary", Some(TypeName::Real)),
                    col("photo", Some(TypeName::Blob)),
                ],
            }]
        );
    }

    #[test]
    fn create_table_rejects_missing_table_name() {
        assert!(parse_sql("CREATE TABLE (id INTEGER);").is_err());
    }

    #[test]
    fn create_table_rejects_a_column_with_no_type() {
        assert!(parse_sql("CREATE TABLE t (id);").is_err());
    }

    #[test]
    fn create_table_rejects_an_empty_column_list() {
        assert!(parse_sql("CREATE TABLE t ();").is_err());
    }

    #[test]
    fn create_table_rejects_a_missing_closing_paren() {
        assert!(parse_sql("CREATE TABLE t (id INTEGER;").is_err());
    }

    // ---- CREATE INDEX ----

    #[test]
    fn create_plain_index() {
        let stmts = parse_sql("CREATE INDEX idx ON employees (salary);").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::CreateIndex {
                index: "idx".to_string(),
                table: "employees".to_string(),
                on_columns: vec![col("salary", None)],
                unique: false,
            }]
        );
    }

    #[test]
    fn create_unique_index() {
        let stmts = parse_sql("CREATE UNIQUE INDEX idx ON employees (salary);").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::CreateIndex {
                index: "idx".to_string(),
                table: "employees".to_string(),
                on_columns: vec![col("salary", None)],
                unique: true,
            }]
        );
    }

    #[test]
    fn create_index_on_multiple_columns() {
        let stmts = parse_sql("CREATE INDEX idx ON t (a, b, c);").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::CreateIndex {
                index: "idx".to_string(),
                table: "t".to_string(),
                on_columns: vec![col("a", None), col("b", None), col("c", None)],
                unique: false,
            }]
        );
    }

    #[test]
    fn create_index_rejects_a_missing_on() {
        assert!(parse_sql("CREATE INDEX idx employees (salary);").is_err());
    }

    // ---- INSERT ----

    #[test]
    fn insert_with_explicit_column_list() {
        let stmts = parse_sql("INSERT INTO employees (id, name) VALUES (1, 'Ada');").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Insert {
                table: "employees".to_string(),
                into_columns: Some(vec![col("id", None), col("name", None)]),
                values: vec![Value::Int(1), Value::Text("Ada".to_string())],
            }]
        );
    }

    #[test]
    fn insert_without_column_list() {
        let stmts = parse_sql("INSERT INTO employees VALUES (1, 'Ada');").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Insert {
                table: "employees".to_string(),
                into_columns: None,
                values: vec![Value::Int(1), Value::Text("Ada".to_string())],
            }]
        );
    }

    #[test]
    fn insert_accepts_mixed_literal_types_including_null() {
        let stmts =
            parse_sql("INSERT INTO t VALUES (1, 2.5, 'hi', NULL);").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Insert {
                table: "t".to_string(),
                into_columns: None,
                values: vec![
                    Value::Int(1),
                    Value::Float(2.5),
                    Value::Text("hi".to_string()),
                    Value::Null,
                ],
            }]
        );
    }

    #[test]
    fn insert_accepts_a_negative_literal_value() {
        let stmts = parse_sql("INSERT INTO t VALUES (-1);").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Insert {
                table: "t".to_string(),
                into_columns: None,
                values: vec![Value::Int(-1)],
            }]
        );
    }

    #[test]
    fn insert_rejects_arity_mismatch_between_columns_and_values() {
        assert!(parse_sql("INSERT INTO t (id, name) VALUES (1);").is_err());
    }

    #[test]
    fn insert_rejects_a_missing_values_keyword() {
        assert!(parse_sql("INSERT INTO t (id) (1);").is_err());
    }

    // ---- SELECT ----

    #[test]
    fn select_star() {
        let stmts = parse_sql("SELECT * FROM employees;").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Select {
                table: "employees".to_string(),
                select_columns: star_cols(),
                where_clause: None,
                order_by: None,
                limit: None,
            }]
        );
    }

    #[test]
    fn select_specific_columns() {
        let stmts = parse_sql("SELECT id, name FROM employees;").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Select {
                table: "employees".to_string(),
                select_columns: select_cols(&[("id", "id"), ("name", "name")]),
                where_clause: None,
                order_by: None,
                limit: None,
            }]
        );
    }

    #[test]
    fn select_with_alias() {
        let stmts = parse_sql("SELECT name AS unc FROM employees;").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Select {
                table: "employees".to_string(),
                select_columns: select_cols(&[("name", "unc")]),
                where_clause: None,
                order_by: None,
                limit: None,
            }]
        );
    }

    #[test]
    fn select_rejects_a_wildcard_combined_with_named_columns() {
        assert!(parse_sql("SELECT *, name FROM employees;").is_err());
    }

    #[test]
    fn select_rejects_a_missing_from() {
        assert!(parse_sql("SELECT id;").is_err());
    }

    #[test]
    fn select_with_simple_where() {
        let stmts = parse_sql("SELECT id FROM employees WHERE id = 1;").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Select {
                table: "employees".to_string(),
                select_columns: select_cols(&[("id", "id")]),
                where_clause: Some(bin(ident("id"), BinOp::Eq, int(1))),
                order_by: None,
                limit: None,
            }]
        );
    }

    #[test]
    fn select_with_where_not() {
        let stmts = parse_sql("SELECT id FROM employees WHERE NOT id = 1;").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Select {
                table: "employees".to_string(),
                select_columns: select_cols(&[("id", "id")]),
                where_clause: Some(Expr::Unary(
                    UnaryOp::Not,
                    Box::new(bin(ident("id"), BinOp::Eq, int(1)))
                )),
                order_by: None,
                limit: None,
            }]
        );
    }

    #[test]
    fn select_with_where_and() {
        let stmts = parse_sql("SELECT id FROM employees WHERE id = 1 AND age = 2;").unwrap();
        let Stmt::Select { where_clause, .. } = &stmts[0] else {
            panic!("expected a Select statement");
        };
        assert_eq!(
            *where_clause,
            Some(bin(
                bin(ident("id"), BinOp::Eq, int(1)),
                BinOp::And,
                bin(ident("age"), BinOp::Eq, int(2)),
            ))
        );
    }

    #[test]
    fn select_with_where_and_binds_tighter_than_or() {
        // a = 1 OR b = 2 AND c = 3  ==  a = 1 OR (b = 2 AND c = 3)
        let stmts =
            parse_sql("SELECT id FROM t WHERE a = 1 OR b = 2 AND c = 3;").unwrap();
        let Stmt::Select { where_clause, .. } = &stmts[0] else {
            panic!("expected a Select statement");
        };
        assert_eq!(
            *where_clause,
            Some(bin(
                bin(ident("a"), BinOp::Eq, int(1)),
                BinOp::Or,
                bin(
                    bin(ident("b"), BinOp::Eq, int(2)),
                    BinOp::And,
                    bin(ident("c"), BinOp::Eq, int(3)),
                ),
            ))
        );
    }

    #[test]
    fn select_with_parenthesized_where() {
        let stmts = parse_sql("SELECT id FROM t WHERE (id = 1);").unwrap();
        let Stmt::Select { where_clause, .. } = &stmts[0] else {
            panic!("expected a Select statement");
        };
        assert_eq!(*where_clause, Some(bin(ident("id"), BinOp::Eq, int(1))));
    }

    #[test]
    fn select_where_accepts_a_bare_expression_with_no_comparison() {
        // WHERE flag  (no comparison operator at all; grammar makes it optional)
        let stmts = parse_sql("SELECT id FROM t WHERE flag;").unwrap();
        let Stmt::Select { where_clause, .. } = &stmts[0] else {
            panic!("expected a Select statement");
        };
        assert_eq!(*where_clause, Some(ident("flag")));
    }

    #[test]
    fn select_where_rejects_a_chained_comparison() {
        // a = b = c is not defined by the grammar.
        assert!(parse_sql("SELECT id FROM t WHERE a = b = c;").is_err());
    }

    #[test]
    fn select_with_arithmetic_projection() {
        // `select_columns: HashMap<Column, Column>` has no way to hold an
        // expression, only a bare name — this can't actually succeed until
        // that type is restructured to hold `Expr`s (see bug report), so for
        // now this documents the current, honest failure rather than a
        // fictional AST shape.
        assert!(parse_sql("SELECT salary + bonus FROM employees;").is_err());
    }

    #[test]
    fn select_with_order_by_defaults_to_ascending_when_direction_omitted() {
        let stmts = parse_sql("SELECT id FROM t ORDER BY id;").unwrap();
        let Stmt::Select { order_by, .. } = &stmts[0] else {
            panic!("expected a Select statement");
        };
        assert_eq!(*order_by, Some((ident("id"), SortDir::Asc)));
    }

    #[test]
    fn select_with_order_by_desc() {
        let stmts = parse_sql("SELECT id FROM t ORDER BY id DESC;").unwrap();
        let Stmt::Select { order_by, .. } = &stmts[0] else {
            panic!("expected a Select statement");
        };
        assert_eq!(*order_by, Some((ident("id"), SortDir::Desc)));
    }

    #[test]
    fn select_with_limit() {
        let stmts = parse_sql("SELECT id FROM t LIMIT 10;").unwrap();
        let Stmt::Select { limit, .. } = &stmts[0] else {
            panic!("expected a Select statement");
        };
        assert_eq!(*limit, Some(10));
    }

    #[test]
    fn select_rejects_a_non_integer_limit() {
        assert!(parse_sql("SELECT id FROM t LIMIT 'x';").is_err());
    }

    #[test]
    fn select_rejects_a_negative_limit() {
        assert!(parse_sql("SELECT id FROM t LIMIT -1;").is_err());
    }

    #[test]
    fn select_with_where_order_by_and_limit_together() {
        let stmts =
            parse_sql("SELECT id FROM t WHERE id = 1 ORDER BY id DESC LIMIT 5;").unwrap();
        let Stmt::Select {
            where_clause,
            order_by,
            limit,
            ..
        } = &stmts[0]
        else {
            panic!("expected a Select statement");
        };
        assert_eq!(*where_clause, Some(bin(ident("id"), BinOp::Eq, int(1))));
        assert_eq!(*order_by, Some((ident("id"), SortDir::Desc)));
        assert_eq!(*limit, Some(5));
    }

    #[test]
    fn select_order_by_rejects_a_non_identifier_column() {
        assert!(parse_sql("SELECT id FROM t ORDER BY 5;").is_err());
    }

    // ---- UPDATE ----

    #[test]
    fn update_single_assignment_with_where() {
        let stmts = parse_sql("UPDATE employees SET name = 'Bob' WHERE id = 1;").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Update {
                table: "employees".to_string(),
                set: vec![Assignment {
                    column: col("name", None),
                    value: Expr::Literal(Literal::String("Bob".to_string())),
                }],
                where_clause: Some(bin(ident("id"), BinOp::Eq, int(1))),
            }]
        );
    }

    #[test]
    fn update_multiple_assignments() {
        let stmts =
            parse_sql("UPDATE employees SET name = 'Bob', age = 30 WHERE id = 1;").unwrap();
        let Stmt::Update { set, .. } = &stmts[0] else {
            panic!("expected an Update statement");
        };
        assert_eq!(
            *set,
            vec![
                Assignment {
                    column: col("name", None),
                    value: Expr::Literal(Literal::String("Bob".to_string())),
                },
                Assignment {
                    column: col("age", None),
                    value: int(30),
                },
            ]
        );
    }

    #[test]
    fn update_without_where() {
        let stmts = parse_sql("UPDATE employees SET name = 'Bob';").unwrap();
        let Stmt::Update { where_clause, .. } = &stmts[0] else {
            panic!("expected an Update statement");
        };
        assert_eq!(*where_clause, None);
    }

    // ---- DELETE ----

    #[test]
    fn delete_with_where() {
        let stmts = parse_sql("DELETE FROM employees WHERE id = 1;").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Delete {
                table: "employees".to_string(),
                where_clause: Some(bin(ident("id"), BinOp::Eq, int(1))),
            }]
        );
    }

    #[test]
    fn delete_without_where() {
        let stmts = parse_sql("DELETE FROM employees;").unwrap();
        assert_eq!(
            stmts,
            vec![Stmt::Delete {
                table: "employees".to_string(),
                where_clause: None,
            }]
        );
    }

    // ---- multiple statements ----

    #[test]
    fn multiple_semicolon_separated_statements_all_parse() {
        let stmts = parse_sql("SELECT id FROM t; DELETE FROM t WHERE id = 1;").unwrap();
        assert_eq!(stmts.len(), 2);
    }

    // ---- expression precedence and associativity (isolated, via parse_expr) ----

    #[test]
    fn multiplication_binds_tighter_than_addition() {
        // a + b * c == a + (b * c)
        let expr = parse_expr_fragment("a + b * c").unwrap();
        assert_eq!(
            expr,
            bin(ident("a"), BinOp::Add, bin(ident("b"), BinOp::Mul, ident("c")))
        );
    }

    #[test]
    fn subtraction_is_left_associative() {
        // a - b - c == (a - b) - c
        let expr = parse_expr_fragment("a - b - c").unwrap();
        assert_eq!(
            expr,
            bin(bin(ident("a"), BinOp::Sub, ident("b")), BinOp::Sub, ident("c"))
        );
    }

    #[test]
    fn parens_override_default_precedence() {
        // (a + b) * c
        let expr = parse_expr_fragment("(a + b) * c").unwrap();
        assert_eq!(
            expr,
            bin(bin(ident("a"), BinOp::Add, ident("b")), BinOp::Mul, ident("c"))
        );
    }

    #[test]
    fn unary_negation_binds_before_addition() {
        // -a + b == (-a) + b
        let expr = parse_expr_fragment("-a + b").unwrap();
        assert_eq!(
            expr,
            bin(
                Expr::Unary(UnaryOp::Neg, Box::new(ident("a"))),
                BinOp::Add,
                ident("b")
            )
        );
    }

    #[test]
    fn full_arithmetic_precedence_chain() {
        // a + b / c - d * e  ==  (a + (b / c)) - (d * e)
        let expr = parse_expr_fragment("a + b / c - d * e").unwrap();
        assert_eq!(
            expr,
            bin(
                bin(ident("a"), BinOp::Add, bin(ident("b"), BinOp::Div, ident("c"))),
                BinOp::Sub,
                bin(ident("d"), BinOp::Mul, ident("e")),
            )
        );
    }

    #[test]
    fn not_wraps_the_comparison_it_precedes() {
        let expr = parse_expr_fragment("NOT a = 1").unwrap();
        assert_eq!(
            expr,
            Expr::Unary(UnaryOp::Not, Box::new(bin(ident("a"), BinOp::Eq, int(1))))
        );
    }

    #[test]
    fn a_single_identifier_is_a_valid_expression_on_its_own() {
        let expr = parse_expr_fragment("a").unwrap();
        assert_eq!(expr, ident("a"));
    }
}

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

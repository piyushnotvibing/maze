use crate::sql::lexer::TypeName;

pub mod lexer;
pub mod ast;
pub mod parser;

/*
    Statements that I will support:
    1. CREATE TABLE/INDEX
    2. INSERT
    3. SELECT
    4. DELETE
    5. UPDATE

    No ALTER TABLE, no transactions, no triggers/views, no GRANT, REVOKE
    
    Grammar:

    <statement>       ::= <create_table>; | <create_index>; | <insert>; | <select>; | <update>; | <delete>;

    <create_table>    ::= "CREATE" "TABLE" <ident> "(" <column_def> { "," <column_def> } ")"
    <column_def>      ::= <ident> <type_name> [ "PRIMARY" "KEY" | "UNIQUE" | "NOT" "NULL" | "UNIQUE" "NOT" "NULL" | "NOT" "NULL" "UNIQUE" ]
    <type_name>       ::= "INTEGER" | "TEXT" | "REAL" | "BLOB"

    <create_index>    ::= "CREATE" [ "UNIQUE" ] "INDEX" <ident> "ON" <ident> "(" <ident> { "," <ident> } ")"

    <insert>          ::= "INSERT" "INTO" <ident> [ "(" <ident> { "," <ident> } ")" ] "VALUES" "(" <literal> { "," <literal> } ")"

    <select>          ::= "SELECT" <select_list> "FROM" <ident> [ <where_clause> ] [ <order_by_clause> ] [ <limit_clause> ]
    <select_list>     ::= "*" | <ident> [ "AS" <ident> ] { "," <ident> [ "AS" <ident> ] }
    <where_clause>    ::= "WHERE" <expr>
    <order_by_clause> ::= "ORDER" "BY" <ident> [ "ASC" | "DESC" ]
    <limit_clause>    ::= "LIMIT" <int_literal>

    <update>          ::= "UPDATE" <ident> "SET" <assignment> { "," <assignment> } [ <where_clause> ]
    <assignment>      ::= <ident> "=" <expr>

    <delete>          ::= "DELETE" "FROM" <ident> [ <where_clause> ]

    <expr>            ::= <or_expr>
    <or_expr>         ::= <and_expr> { "OR" <and_expr> }
    <and_expr>        ::= <not_expr> { "AND" <not_expr> }
    <not_expr>        ::= [ "NOT" ] <comparison>
    <comparison>      ::= <arith_expr> [ <comp_op> <arith_expr> ]
    <comp_op>         ::= "=" | "!=" | "<>" | "<" | "<=" | ">" | ">="

    <arith_expr>      ::= <term> { ( "+" | "-" ) <term> }
    <term>            ::= <factor> { ( "*" | "/" ) <factor> }
    <factor>          ::= [ "-" ] <primary>
    <primary>         ::= <ident> | <literal> | "(" <expr> ")"

    <literal>         ::= <int_literal> | <float_literal> | <string_literal> | "NULL"

    -> { X } becomes while <lookahead matches X's start> { ... } — e.g. <and_expr> { "OR" <and_expr> } is 
        "parse one and_expr, then loop consuming OR and_expr as long as the next token is OR."
    -> [ X ] becomes if <lookahead matches X's start> { ... } — an optional clause you either consume or skip.
    -> A | B becomes a match/if on the current token

    if you notice, NULL is a literal here, and it could end up in an arithmetic expression
    something like SELECT * FROM employee WHERE salary = 30000 + NULL; will parse, and it should evaluate like
        => salary = 30000 + NULL
        => salary = NULL
        => UNKNOWN
    the WHERE filter evaluates to UNKNOWN, which returns 0 rows. 

    -> a column definition can be 
        -> col <type> UNIQUE NOT NULL
        -> col <type> NOT NULL UNIQUE
        -> col <type> PRIMARY KEY
*/

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct Column {
    pub name: String,
    pub alias: Option<String>,
    pub c_type: Option<TypeName>,
    pub constraints: ColumnConstraints,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ColumnConstraints {
    None,
    PrimaryKey,
    Unique,
    NotNull,
    UniqueNotNull
} 
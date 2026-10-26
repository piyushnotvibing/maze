use maze::sql::lexer::tokenize;

fn main() {
    match tokenize("SELECT name AS unc FROM employees WHERE age >= 30.6 AND name = 'Piyush Udhao''s Dog' ORDER BY name LIMIT 10;".to_string()) {
        Ok(v) => println!("{v:?}"),
        Err(e) => panic!("{e}"),
    }
}

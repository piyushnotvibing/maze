use maze::sql::lexer::tokenize;

fn main() {
    match tokenize("INSERT INTO employee VALUES (1, 'PETER PARKER', 25, 261.23, X'JDCNAKNSC8328473829JDKSD');".to_string()) {
        Ok(v) => println!("{v:?}"),
        Err(e) => panic!("{e}"),
    }
}

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Blob(Vec<u8>),
    Text(String),
}

impl Value {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Value::Null => Vec::new(),
            Value::Int(i) => i.to_be_bytes().to_vec(),
            Value::Float(f) => f.to_be_bytes().to_vec(),
            Value::Blob(b) => b.clone(),
            Value::Text(t) => t.as_bytes().to_vec(),
        }
    }
}

impl Ord for Value {
    // can panic if one of the values is an abnormal floating point number (NaN, infinity)
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, Value::Int(_)) => Ordering::Less,
            (Value::Null, Value::Float(_)) => Ordering::Less,
            (Value::Null, Value::Blob(_)) => Ordering::Less,
            (Value::Null, Value::Text(_)) => Ordering::Less,

            (Value::Int(_), Value::Null) => Ordering::Greater,
            (Value::Int(a), Value::Int(b)) => a.cmp(b),
            (Value::Int(i), Value::Float(f)) => {
                order_f64_i64(*f, *i).expect("tried comparing to NaN")
            }
            (Value::Int(_), Value::Text(_)) => Ordering::Less,
            (Value::Int(_), Value::Blob(_)) => Ordering::Less,

            (Value::Float(_), Value::Null) => Ordering::Greater,
            (Value::Float(f), Value::Int(i)) => {
                order_f64_i64(*f, *i).expect("tried comparing to NaN")
            }
            (Value::Float(f1), Value::Float(f2)) => {
                f1.partial_cmp(f2).expect("tried to compare a NaN value")
            }
            (Value::Float(_), Value::Text(_)) => Ordering::Less,
            (Value::Float(_), Value::Blob(_)) => Ordering::Less,

            (Value::Text(_), Value::Null) => Ordering::Greater,
            (Value::Text(_), Value::Int(_)) => Ordering::Greater,
            (Value::Text(_), Value::Float(_)) => Ordering::Greater,
            (Value::Text(t1), Value::Text(t2)) => t1.cmp(t2),
            (Value::Text(_), Value::Blob(_)) => Ordering::Less,

            (Value::Blob(_), Value::Null) => Ordering::Greater,
            (Value::Blob(_), Value::Int(_)) => Ordering::Greater,
            (Value::Blob(_), Value::Float(_)) => Ordering::Greater,
            (Value::Blob(_), Value::Text(_)) => Ordering::Greater,
            (Value::Blob(b1), Value::Blob(b2)) => b1.cmp(b2),
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for Value {}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DataType {
    Null,
    Int,
    Float,
    Blob(u64),
    Text(u64),
}

impl From<&Value> for DataType {
    fn from(value: &Value) -> Self {
        match value {
            Value::Null => DataType::Null,
            Value::Int(_) => DataType::Int,
            Value::Float(_) => DataType::Float,
            Value::Blob(b) => DataType::Blob(b.len() as u64),
            Value::Text(t) => DataType::Text(t.len() as u64),
        }
    }
}

/*
    ----------------------------------------------------------------
    | Tag                     ->        Value                      |
    | ------------------------------------------------------------ |
    | 0                       ->        Null                       |
    | 6                       ->        Signed 64 bit int          |
    | 7                       ->        64 bit float               |
    | >= 12 && tag % 2 == 0   ->        BLOB                       |
    | >= 13 && tag % 2 == 1   ->        Text                       |
    ----------------------------------------------------------------
*/
impl DataType {
    pub fn new(code: u64) -> anyhow::Result<Self> {
        match code {
            0 => Ok(DataType::Null),
            6 => Ok(DataType::Int),
            7 => Ok(DataType::Float),
            _ => {
                let code_is_even = code.is_multiple_of(2);
                if code >= 12 && code_is_even {
                    Ok(DataType::Blob((code - 12) / 2))
                } else if code >= 13 && !code_is_even {
                    Ok(DataType::Text((code - 13) / 2))
                } else {
                    anyhow::bail!("invalid code, no corresponding data type")
                }
            }
        }
    }

    pub fn encode(&self) -> anyhow::Result<u64> {
        match self {
            DataType::Null => Ok(0),
            DataType::Int => Ok(6),
            DataType::Float => Ok(7),
            DataType::Blob(blob_len) => 2u64
                .checked_mul(*blob_len)
                .ok_or_else(|| anyhow::anyhow!("blob size too big for u64"))?
                .checked_add(12)
                .ok_or_else(|| anyhow::anyhow!("blob size too big for u64")),
            DataType::Text(text_len) => 2u64
                .checked_mul(*text_len)
                .ok_or_else(|| anyhow::anyhow!("text size too big for u64"))?
                .checked_add(13)
                .ok_or_else(|| anyhow::anyhow!("text size too big for u64")),
        }
    }
}

fn order_f64_i64(f: f64, i: i64) -> anyhow::Result<Ordering> {
    if f.is_nan() || f.is_infinite() {
        anyhow::bail!("tried comparing with NaN")
    }
    let floor = f.floor() as i64;
    if floor > i || (floor == i && (f - f.floor() > 0.0)) {
        Ok(Ordering::Greater)
    } else if floor == i && (f - f.floor() == 0.0) {
        Ok(Ordering::Equal)
    } else {
        Ok(Ordering::Less)
    }
}
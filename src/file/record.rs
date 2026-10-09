use crate::file::varint;

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

#[derive(Debug, Clone, PartialEq)]
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
    fn new(code: u64) -> anyhow::Result<Self> {
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

    fn code(&self) -> anyhow::Result<u64> {
        match self {
            DataType::Null => Ok(0),
            DataType::Int => Ok(6),
            DataType::Float => Ok(7),
            DataType::Blob(blob_len) => 2u64
                .checked_mul(*blob_len as u64)
                .ok_or_else(|| anyhow::anyhow!("blob size too big for u64"))?
                .checked_add(12)
                .ok_or_else(|| anyhow::anyhow!("blob size too big for u64")),
            DataType::Text(text_len) => 2u64
                .checked_mul(*text_len as u64)
                .ok_or_else(|| anyhow::anyhow!("text size too big for u64"))?
                .checked_add(13)
                .ok_or_else(|| anyhow::anyhow!("text size too big for u64")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordHeader {
    // The column tags of each column in the schema, varint encoded.
    pub column_codes: Vec<u64>,
}

impl RecordHeader {
    fn build(mut buf: &[u8]) -> anyhow::Result<(Self, usize)> {
        let (header_len, bytes_read) =
            varint::decode(buf).ok_or_else(|| anyhow::anyhow!("invalid header length varint"))?;
        let header_len = usize::try_from(header_len)?;
        buf = &buf[bytes_read..];

        let mut data_types_len = header_len
            .checked_sub(bytes_read)
            .ok_or_else(|| anyhow::anyhow!("header is smaller than its own length's varint?"))?;
        let mut column_codes = Vec::new();
        while data_types_len != 0 {
            let (data_type, bytes_read) =
                varint::decode(buf).ok_or(anyhow::anyhow!("invalid serial type varint"))?;
            column_codes.push(data_type);
            buf = &buf[bytes_read..];
            data_types_len = data_types_len
                .checked_sub(bytes_read)
                .ok_or_else(|| anyhow::anyhow!("data_types_len is incorrect"))?;
        }

        Ok((RecordHeader { column_codes }, header_len))
    }

    fn encode(&self) -> Vec<u8> {
        let types = self
            .column_codes
            .iter()
            .flat_map(|&t| varint::encode(t))
            .collect::<Vec<u8>>();

        // usually headers are < 128 bytes, so header_len varint is only 1 byte.
        let mut header_len = types.len() + 1;
        // if more than one byte is required, this will fix it.
        loop {
            let need = varint::encode(header_len as u64).len() + types.len();
            if need == header_len {
                break;
            }
            header_len = need;
        }

        let mut bytes = varint::encode(header_len as u64);
        bytes.extend(types);
        bytes
    }
}

#[derive(Debug, Clone)]
pub struct Record {
    pub header: RecordHeader,
    pub body: Vec<Value>,
}

impl Record {
    pub fn new(values: Vec<Value>) -> anyhow::Result<Self> {
        let column_codes = values
            .iter()
            .map(|value| DataType::from(value).code())
            .collect::<anyhow::Result<Vec<u64>>>()?;
        Ok(Record {
            header: RecordHeader { column_codes },
            body: values,
        })
    }

    pub fn build(mut buf: &[u8]) -> anyhow::Result<Self> {
        let (header, header_len) = RecordHeader::build(buf)?;
        buf = buf
            .get(header_len..)
            .ok_or_else(|| anyhow::anyhow!("record's header is smaller than the record wow"))?;

        let mut body = Vec::new();
        let mut offset = 0usize;
        for &column_code in header.column_codes.iter() {
            let column_type = DataType::new(column_code)?;
            match column_type {
                DataType::Null => body.push(Value::Null), // If value is null, no need to increment offset.
                DataType::Int => body.push(Value::Int(i64::from_be_bytes(
                    take(buf, &mut offset, 8)?.try_into()?,
                ))),
                DataType::Float => body.push(Value::Float(f64::from_be_bytes(
                    take(buf, &mut offset, 8)?.try_into()?,
                ))),
                DataType::Blob(blob_len) => body.push(Value::Blob(
                    (take(buf, &mut offset, usize::try_from(blob_len)?))?.to_vec(),
                )),
                DataType::Text(text_len) => body.push(Value::Text(String::from_utf8(
                    (take(buf, &mut offset, usize::try_from(text_len)?))?.to_vec(),
                )?)),
            }
        }

        if offset != buf.len() {
            anyhow::bail!("corrupted buffer")
        }

        Ok(Record { header, body })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = self.header.encode();
        self.body
            .iter()
            .for_each(|value| bytes.extend_from_slice(&value.encode()));

        bytes
    }

    pub fn decode(buf: &[u8]) -> anyhow::Result<Vec<Value>> {
        let record = Record::build(buf)?;
        Ok(record.body)
    }
}

fn take<'a>(buf: &'a [u8], offset: &mut usize, n: usize) -> anyhow::Result<&'a [u8]> {
    let end = offset
        .checked_add(n)
        .ok_or_else(|| anyhow::anyhow!("length overflow"))?;
    let target = buf
        .get(*offset..end)
        .ok_or_else(|| anyhow::anyhow!("record body truncated"))?;
    *offset = end;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_roundtrip() {
        let values = vec![
            Value::Int(-5),
            Value::Text("Ada".into()),
            Value::Null,
            Value::Blob(vec![1, 2, 3, 4]),
            Value::Float(1.5),
            Value::Text("x".into()),
        ];
        let rec = Record::new(values.clone()).unwrap();
        assert_eq!(Record::decode(&rec.encode()).unwrap(), values);
    }

    #[test]
    fn record_with_130_nulls_to_test_header_len_logic() {
        let values = vec![
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
        ];
        let rec = Record::new(values.clone()).unwrap();
        assert_eq!(Record::decode(&rec.encode()).unwrap(), values);
    }
}

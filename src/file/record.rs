use crate::file::varint;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Float(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Value::Null => Vec::new(),
            Value::Int(i) => i.to_be_bytes().to_vec(),
            Value::Float(f) => f.to_be_bytes().to_vec(),
            Value::Text(t) => t.as_bytes().to_vec(),
            Value::Blob(b) => b.clone(),
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
    pub fn get_tag(&self) -> anyhow::Result<u64> {
        match self {
            Value::Null => Ok(0),
            Value::Int(_) => Ok(6),
            Value::Float(_) => Ok(7),
            Value::Text(t) => {
                let tag = t
                    .len()
                    .checked_mul(2)
                    .ok_or_else(|| anyhow::anyhow!("text size is too long"))?
                    .checked_add(13)
                    .ok_or_else(|| anyhow::anyhow!("text size is too long"))?;
                Ok(tag as u64)
            }
            Value::Blob(items) => {
                let tag = items
                    .len()
                    .checked_mul(2)
                    .ok_or_else(|| anyhow::anyhow!("blob size is too long"))?
                    .checked_add(12)
                    .ok_or_else(|| anyhow::anyhow!("blob size is too long"))?;
                Ok(tag as u64)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecordHeader {
    // The column tags of each column in the schema, varint encoded.
    pub column_types: Vec<u64>,
}

impl RecordHeader {
    fn build(mut buf: &[u8]) -> anyhow::Result<(Self, usize)> {
        let (header_len, bytes_read) =
            varint::decode(buf).ok_or_else(|| anyhow::anyhow!("invalid header length varint"))?;
        let header_len = usize::try_from(header_len)?;
        buf = &buf[bytes_read..];

        let mut serial_type_len = usize::try_from(header_len)?
            .checked_sub(bytes_read)
            .ok_or_else(|| anyhow::anyhow!("header is smaller than its own length's varint?"))?;
        let mut column_types = Vec::new();
        while serial_type_len != 0 {
            let (serial_type, bytes_read) =
                varint::decode(buf).ok_or(anyhow::anyhow!("invalid serial type varint"))?;
            column_types.push(serial_type);
            buf = &buf[bytes_read..];
            serial_type_len = serial_type_len
                .checked_sub(bytes_read)
                .ok_or_else(|| anyhow::anyhow!("serial_type_len is incorrect"))?;
        }

        Ok((RecordHeader { column_types }, header_len))
    }

    fn encode(&self) -> Vec<u8> {
        let types = self
            .column_types
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
        let column_types = values
            .iter()
            .map(Value::get_tag)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Record {
            header: RecordHeader { column_types },
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
        for &column_type in header.column_types.iter() {
            match column_type {
                0 => body.push(Value::Null), // If value is null, no need to increment offset.
                6 => body.push(Value::Int(i64::from_be_bytes(
                    take(buf, &mut offset, 8)?.try_into()?,
                ))),
                7 => body.push(Value::Float(f64::from_be_bytes(
                    take(buf, &mut offset, 8)?.try_into()?,
                ))),
                _ => {
                    let column_type_is_even = column_type % 2 == 0;
                    if column_type >= 12 && column_type_is_even {
                        let bytes_to_read = (usize::try_from(column_type)? - 12) / 2;
                        body.push(Value::Blob(
                            (take(buf, &mut offset, bytes_to_read))?.to_vec(),
                        ));
                    } else if column_type >= 13 && !column_type_is_even {
                        let bytes_to_read = (usize::try_from(column_type)? - 13) / 2;
                        body.push(Value::Text(String::from_utf8(
                            (take(buf, &mut offset, bytes_to_read))?.to_vec(),
                        )?));
                    } else {
                        anyhow::bail!("invalid serial type")
                    }
                }
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

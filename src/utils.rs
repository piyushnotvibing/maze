pub fn read_word_u64_be(buf: &[u8], at: usize) -> anyhow::Result<u64> {
    let b = buf
        .get(at..at + 8)
        .ok_or_else(|| anyhow::anyhow!("short read at {at}"))?;
    Ok(u64::from_be_bytes(b.try_into().unwrap())) // length is exactly 8, can't fail
}

pub fn read_word_u32_be(buf: &[u8], at: usize) -> anyhow::Result<u32> {
    let b = buf
        .get(at..at + 4)
        .ok_or_else(|| anyhow::anyhow!("short read at {at}"))?;
    Ok(u32::from_be_bytes(b.try_into().unwrap()))
}

pub fn read_word_u16_be(buf: &[u8], at: usize) -> anyhow::Result<u16> {
    let b = buf
        .get(at..at + 2)
        .ok_or_else(|| anyhow::anyhow!("short read at {at}"))?;
    Ok(u16::from_be_bytes(b.try_into().unwrap()))
}

pub fn read_word_u8_be(buf: &[u8], at: usize) -> anyhow::Result<u8> {
    let b = buf
        .get(at..at + 1)
        .ok_or_else(|| anyhow::anyhow!("short read at {at}"))?;
    Ok(u8::from_be_bytes(b.try_into().unwrap()))
}

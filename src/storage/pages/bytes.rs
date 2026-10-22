pub fn read_word_u64_be(buf: &[u8], at: usize) -> anyhow::Result<u64> {
    let end = at
        .checked_add(std::mem::size_of::<u64>())
        .ok_or_else(|| anyhow::anyhow!("word out of bounds"))?;
    let b = buf
        .get(at..end)
        .ok_or_else(|| anyhow::anyhow!("short read at {at}"))?;
    Ok(u64::from_be_bytes(b.try_into().unwrap())) // length is exactly 8, can't fail
}

pub fn read_word_u32_be(buf: &[u8], at: usize) -> anyhow::Result<u32> {
    let end = at
        .checked_add(std::mem::size_of::<u32>())
        .ok_or_else(|| anyhow::anyhow!("word out of bounds"))?;
    let b = buf
        .get(at..end)
        .ok_or_else(|| anyhow::anyhow!("short read at {at}"))?;
    Ok(u32::from_be_bytes(b.try_into().unwrap()))
}

pub fn read_word_u16_be(buf: &[u8], at: usize) -> anyhow::Result<u16> {
    let end = at
        .checked_add(std::mem::size_of::<u16>())
        .ok_or_else(|| anyhow::anyhow!("word out of bounds"))?;
    let b = buf
        .get(at..end)
        .ok_or_else(|| anyhow::anyhow!("short read at {at}"))?;
    Ok(u16::from_be_bytes(b.try_into().unwrap()))
}

pub fn read_word_u8_be(buf: &[u8], at: usize) -> anyhow::Result<u8> {
    let end = at
        .checked_add(std::mem::size_of::<u8>())
        .ok_or_else(|| anyhow::anyhow!("word out of bounds"))?;
    let b = buf
        .get(at..end)
        .ok_or_else(|| anyhow::anyhow!("short read at {at}"))?;
    Ok(u8::from_be_bytes(b.try_into().unwrap()))
}

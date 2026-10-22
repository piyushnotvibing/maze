pub type VarInt = Vec<u8>;

const LAST_56_BIT_NUM: u64 = 0x00ff_ffff_ffff_ffff;

pub fn encode(mut val: u64) -> VarInt {
    if val < 128 {
        return vec![val as u8];
    }
    // if val needs more than 56 bits, need 9th byte.
    if val > LAST_56_BIT_NUM {
        let mut res = vec![0u8; 9];
        // the last, 9th byte contains the 8 LSBits of val as is.
        res[8] = (val & 0xff) as u8; // could've written this just as `val as u8`, but this is more readable.

        val >>= 8;
        for i in (0..8).rev() {
            res[i] = (val & 0x7f) as u8 | 0x80;
            val >>= 7;
        }

        return res;
    }

    let mut groups: [u8; 8] = [0; 8];
    let mut n = 0usize;
    loop {
        groups[n] = (val & 0x7f) as u8;
        n += 1;
        val >>= 7;
        if val == 0 {
            break;
        }
    }

    let mut res = VarInt::with_capacity(n);
    for i in (0..n).rev() {
        res.push(if i == 0 { groups[i] } else { groups[i] | 0x80 });
    }

    res
}

// decode() takes in any buffer, not just a known varint.
pub fn decode(buf: &[u8]) -> Option<(u64, usize)> {
    let mut res = 0u64;
    for i in 0..8 {
        let group = *buf.get(i)?;
        res <<= 7;
        res |= (group & 0x7f) as u64;
        if group & 0x80 == 0 {
            return Some((res, i + 1));
        }
    }

    let ninth_group = *buf.get(8)?;
    Some((((res << 8) | ninth_group as u64), 9))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        assert_eq!(encode(0), vec![0x00]);
        assert_eq!(encode(127), vec![0x7f]);
        assert_eq!(encode(128), vec![0x81, 0x00]);
        assert_eq!(encode(300), vec![0x82, 0x2c]);

        for v in [
            0,
            1,
            127,
            128,
            300,
            16383,
            16384,
            LAST_56_BIT_NUM,
            LAST_56_BIT_NUM + 1,
            u64::MAX,
        ] {
            let enc = encode(v);
            assert!(enc.len() <= 9);
            assert_eq!(decode(&enc), Some((v, enc.len())));
        }
    }
}

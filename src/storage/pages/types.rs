#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct PageNumber(u32);

impl std::fmt::Display for PageNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl PageNumber {
    pub fn get(&self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for PageNumber {
    type Error = anyhow::Error;

    fn try_from(raw: u32) -> anyhow::Result<Self> {
        match raw {
            0 => anyhow::bail!("page number cannot be 0"),
            n => Ok(Self(n)),
        }
    }
}

impl PartialEq<u32> for PageNumber {
    fn eq(&self, other: &u32) -> bool {
        self.0 == *other
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct RowID(u64);

impl std::fmt::Display for RowID {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl RowID {
    pub fn get(&self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for RowID {
    type Error = anyhow::Error;

    fn try_from(raw: u64) -> anyhow::Result<Self> {
        match raw {
            0 => anyhow::bail!("row id cannot be 0"),
            n => Ok(Self(n)),
        }
    }
}

impl PartialEq<u64> for RowID {
    fn eq(&self, other: &u64) -> bool {
        self.0 == *other
    }
}
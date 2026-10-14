#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
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

use serde::{Deserialize, Deserializer, de};

/// "64MiB" "100KiB" "1GiB" "4096" のような大きさの指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Size(pub u64);

impl Size {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (num, unit) = s.split_at(split);
        let n: u64 = num.parse().ok()?;
        let mul: u64 = match unit.trim() {
            "" | "B" => 1,
            "KiB" => 1 << 10,
            "MiB" => 1 << 20,
            "GiB" => 1 << 30,
            _ => return None,
        };
        n.checked_mul(mul).map(Size)
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Num(u64),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Num(n) => Ok(Size(n)),
            Raw::Str(s) => Size::parse(&s).ok_or_else(|| {
                de::Error::custom(format!("大きさの指定が不正: {s:?}（例: \"64MiB\"）"))
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Size;

    #[test]
    fn parse() {
        assert_eq!(Size::parse("64MiB"), Some(Size(64 << 20)));
        assert_eq!(Size::parse("100KiB"), Some(Size(100 << 10)));
        assert_eq!(Size::parse("4096"), Some(Size(4096)));
        assert_eq!(Size::parse("1GiB"), Some(Size(1 << 30)));
        assert_eq!(Size::parse("1MB"), None);
        assert_eq!(Size::parse("MiB"), None);
    }
}

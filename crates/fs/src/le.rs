//! Little-endian field access on byte slices. Out-of-range fields read as zero, so callers
//! must check lengths where a zero would be misread as valid.

pub(crate) fn u8_at(b: &[u8], at: usize) -> u8 {
    b.get(at).copied().unwrap_or(0)
}

pub(crate) fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(array(b, at))
}

pub(crate) fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(array(b, at))
}

pub(crate) fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(array(b, at))
}

fn array<const N: usize>(b: &[u8], at: usize) -> [u8; N] {
    at.checked_add(N)
        .and_then(|end| b.get(at..end))
        .and_then(|s| <[u8; N]>::try_from(s).ok())
        .unwrap_or([0; N])
}

/// Decodes UTF-16LE code units, replacing unpaired surrogates.
pub(crate) fn utf16(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Makes a name safe to join into a `/`-separated path. The flag is `false` if it changed.
pub(crate) fn sanitize_name(name: String) -> (String, bool) {
    if name.is_empty() || name == "." || name == ".." {
        return ("_".to_owned(), false);
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c.is_control())
    {
        let s = name
            .chars()
            .map(|c| {
                if c == '/' || c == '\\' || c.is_control() {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        return (s, false);
    }
    (name, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_reads_zero() {
        let b = [1, 2, 3];
        assert_eq!(u16_at(&b, 1), 0x0302);
        assert_eq!(u16_at(&b, 2), 0);
        assert_eq!(u64_at(&b, usize::MAX), 0);
        assert_eq!(u8_at(&b, 3), 0);
    }

    #[test]
    fn sanitizes_separators() {
        assert_eq!(sanitize_name("a/b".into()), ("a_b".into(), false));
        assert_eq!(sanitize_name("..".into()), ("_".into(), false));
        assert_eq!(sanitize_name("ok.jpg".into()), ("ok.jpg".into(), true));
    }
}

//! Timestamps as recorded by FAT and exFAT (local time, optionally with a UTC offset) and
//! NTFS (UTC).

/// A local date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Timestamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millis: u16,
    /// Offset of the local time from UTC in minutes, if recorded (exFAT; `Some(0)` for NTFS,
    /// which records UTC).
    pub utc_offset_minutes: Option<i16>,
}

impl Timestamp {
    /// Decodes the packed date and time shared by FAT and exFAT. Returns `None` for an
    /// unset (zero) or out-of-range date.
    ///
    /// Reference: Microsoft "FAT32 File System Specification" (fatgen103), "Date and Time
    /// Formats"; Microsoft "exFAT file system specification" 7.4.8 Timestamp Fields.
    // The casts take masked bit fields.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn from_dos(date: u16, time: u16) -> Option<Self> {
        let day = (date & 0x1F) as u8;
        let month = ((date >> 5) & 0x0F) as u8;
        let year = 1980 + (date >> 9);
        let second = ((time & 0x1F) * 2) as u8;
        let minute = ((time >> 5) & 0x3F) as u8;
        let hour = (time >> 11) as u8;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
            return None;
        }
        Some(Self {
            year,
            month,
            day,
            hour,
            minute,
            second: second.min(59),
            millis: 0,
            utc_offset_minutes: None,
        })
    }

    /// Adds a count of 10 ms units (0..=199) to the two-second resolution.
    ///
    /// Reference: fatgen103 "FAT Directory Structure" (DIR_CrtTimeTenth); exFAT
    /// specification 7.4.9 10msIncrement Fields.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn with_10ms(mut self, increment: u8) -> Self {
        if increment <= 199 {
            let extra = u16::from(increment) * 10;
            self.second = (u16::from(self.second) + extra / 1000).min(59) as u8;
            self.millis = extra % 1000;
        }
        self
    }

    /// Reference: exFAT specification 7.4.10 UtcOffset Fields (bit 7: OffsetValid; bits
    /// 0-6: signed offset in 15-minute units).
    pub(crate) fn with_exfat_utc_offset(mut self, raw: u8) -> Self {
        if raw & 0x80 != 0 {
            // Sign-extend the 7-bit value.
            let v = i16::from(((raw << 1) as i8) >> 1);
            self.utc_offset_minutes = Some(v * 15);
        }
        self
    }
}

/// 100-nanosecond intervals per second, and days from 1601-01-01 (the FILETIME epoch) to
/// 1970-01-01.
const FILETIME_PER_SECOND: u64 = 10_000_000;
const DAYS_1601_TO_1970: i64 = 134_774;

impl Timestamp {
    /// Decodes a Windows FILETIME (100 ns intervals since 1601-01-01 00:00 UTC), as NTFS
    /// records it. Returns `None` for zero and for dates past the year 9999.
    ///
    /// Reference: Microsoft "FILETIME structure" (minwinbase.h) and "File Times"
    /// (learn.microsoft.com); the day-to-date conversion is the proleptic Gregorian
    /// "civil_from_days" algorithm described by Howard Hinnant in "chrono-Compatible
    /// Low-Level Date Algorithms" (public domain).
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn from_filetime(ft: u64) -> Option<Self> {
        if ft == 0 {
            return None;
        }
        let secs = ft / FILETIME_PER_SECOND;
        let millis = ((ft % FILETIME_PER_SECOND) / 10_000) as u16;
        let days = i64::try_from(secs / 86_400).ok()? - DAYS_1601_TO_1970;
        let rem = secs % 86_400;
        let (year, month, day) = civil_from_days(days);
        if !(1601..=9999).contains(&year) {
            return None;
        }
        Some(Self {
            year: year as u16,
            month,
            day,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
            millis,
            utc_offset_minutes: Some(0),
        })
    }
}

/// Days since 1970-01-01 to a (year, month, day) in the proleptic Gregorian calendar.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_dos_date_time() {
        // 2008-12-25 04:21:44
        let date = ((2008 - 1980) << 9) | (12 << 5) | 25;
        let time = (4 << 11) | (21 << 5) | 22;
        let t = Timestamp::from_dos(date, time).unwrap().with_10ms(155);
        assert_eq!((t.year, t.month, t.day), (2008, 12, 25));
        assert_eq!((t.hour, t.minute, t.second, t.millis), (4, 21, 45, 550));
        assert_eq!(Timestamp::from_dos(0, 0), None);
    }

    #[test]
    fn decodes_utc_offset() {
        let t = Timestamp::from_dos(0x21, 0).unwrap();
        assert_eq!(
            t.with_exfat_utc_offset(0x80 | 36).utc_offset_minutes,
            Some(540)
        );
        assert_eq!(
            t.with_exfat_utc_offset(0x80 | 0x7F).utc_offset_minutes,
            Some(-15)
        );
        assert_eq!(t.with_exfat_utc_offset(0x40).utc_offset_minutes, None);
    }

    #[test]
    fn decodes_filetime() {
        // 2025-08-14 15:30:22.123 UTC
        let unix = 1_755_185_422u64;
        let ft = (unix + 11_644_473_600) * 10_000_000 + 1_230_000;
        let t = Timestamp::from_filetime(ft).unwrap();
        assert_eq!((t.year, t.month, t.day), (2025, 8, 14));
        assert_eq!((t.hour, t.minute, t.second, t.millis), (15, 30, 22, 123));
        assert_eq!(t.utc_offset_minutes, Some(0));
        // Leap day, and the epoch itself.
        let t = Timestamp::from_filetime((951_782_400 + 11_644_473_600) * 10_000_000).unwrap();
        assert_eq!((t.year, t.month, t.day), (2000, 2, 29));
        let t = Timestamp::from_filetime(1).unwrap();
        assert_eq!((t.year, t.month, t.day, t.hour), (1601, 1, 1, 0));
        assert_eq!(Timestamp::from_filetime(0), None);
        assert_eq!(Timestamp::from_filetime(u64::MAX), None);
    }
}

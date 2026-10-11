//! Timestamps as recorded by FAT and exFAT (local time, optionally with a UTC offset).

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
    /// Offset of the local time from UTC in minutes, if recorded (exFAT only).
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
}

//! Implements wrappers for various Windows time structures.
use windows::Win32::{
    Foundation::{FILETIME, SYSTEMTIME},
    System::Time::SystemTimeToFileTime,
};

/// Wrapper for [FILETIME](https://learn.microsoft.com/en-us/windows/win32/api/minwinbase/ns-minwinbase-filetime)
#[derive(Copy, Clone, Default)]
#[repr(transparent)]
pub struct FileTime(pub(crate) FILETIME);

const SECONDS_BETWEEN_1601_AND_1970: i64 = 11_644_473_600;
const NS_IN_SECOND: i64 = 1_000_000_000;
const MS_IN_SECOND: i64 = 1_000;

impl FileTime {
    /// Converts to a unix timestamp with millisecond granularity.
    pub fn as_unix_timestamp(&self) -> i64 {
        self.as_quad() / 10_000 - (SECONDS_BETWEEN_1601_AND_1970 * MS_IN_SECOND)
    }

    /// Converts to a unix timestamp with nanosecond granularity.
    pub fn as_unix_timestamp_nanos(&self) -> i128 {
        self.as_quad() as i128 * 100
            - (SECONDS_BETWEEN_1601_AND_1970 as i128 * NS_IN_SECOND as i128)
    }

    /// Converts to OffsetDateTime
    #[cfg(feature = "time_rs")]
    pub fn as_date_time(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp_nanos(self.as_unix_timestamp_nanos()).unwrap()
    }

    fn as_quad(&self) -> i64 {
        let mut quad = self.0.dwHighDateTime as i64;
        quad <<= 32;
        quad |= self.0.dwLowDateTime as i64;
        quad
    }

    #[cfg(any(feature = "time_rs", feature = "serde"))]
    pub(crate) fn from_quad(quad: i64) -> Self {
        let mut file_time: FileTime = Default::default();
        file_time.0.dwHighDateTime = (quad >> 32) as u32;
        file_time.0.dwLowDateTime = (quad & 0xffffffff) as u32;
        file_time
    }

    pub(crate) fn from_slice(slice: &[u8; std::mem::size_of::<FileTime>()]) -> Self {
        let ptr = slice.as_ptr() as *const FileTime;
        let mut file_time: FileTime = Default::default();
        unsafe {
            file_time.0.dwHighDateTime = (*ptr).0.dwHighDateTime;
            file_time.0.dwLowDateTime = (*ptr).0.dwLowDateTime;
        }
        file_time
    }
}

#[cfg(feature = "time_rs")]
impl From<FileTime> for time::OffsetDateTime {
    fn from(file_time: FileTime) -> Self {
        file_time.as_date_time()
    }
}

#[cfg(feature = "serde")]
impl serde::ser::Serialize for FileTime {
    #[cfg(feature = "time_rs")]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_date_time().serialize(serializer)
    }

    #[cfg(not(feature = "time_rs"))]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_unix_timestamp().serialize(serializer)
    }
}

/// Wrapper for [SYSTEMTIME](https://learn.microsoft.com/en-us/windows/win32/api/minwinbase/ns-minwinbase-systemtime)
#[derive(Copy, Clone, Default)]
#[repr(transparent)]
pub struct SystemTime(pub(crate) SYSTEMTIME);

impl SystemTime {
    /// Converts to a unix timestamp with millisecond granularity.
    pub fn as_unix_timestamp(&self) -> i64 {
        let file_time: FileTime = Default::default();
        unsafe {
            _ = SystemTimeToFileTime(&self.0 as *const _, &file_time.0 as *const _ as *mut _);
        }
        file_time.as_unix_timestamp()
    }

    /// Converts to a unix timestamp with nanosecond granularity.
    pub fn as_unix_timestamp_nanos(&self) -> i128 {
        let file_time: FileTime = Default::default();
        unsafe {
            _ = SystemTimeToFileTime(&self.0 as *const _, &file_time.0 as *const _ as *mut _);
        }
        file_time.as_unix_timestamp_nanos()
    }

    /// Converts to OffsetDateTime
    #[cfg(feature = "time_rs")]
    pub fn as_date_time(&self) -> time::OffsetDateTime {
        time::OffsetDateTime::from_unix_timestamp_nanos(self.as_unix_timestamp_nanos()).unwrap()
    }

    pub(crate) fn from_slice(slice: &[u8; std::mem::size_of::<SystemTime>()]) -> Self {
        let ptr = slice.as_ptr() as *const SystemTime;
        let mut system_time: SystemTime = Default::default();
        unsafe {
            system_time.0.wYear = (*ptr).0.wYear;
            system_time.0.wMonth = (*ptr).0.wMonth;
            system_time.0.wDayOfWeek = (*ptr).0.wDayOfWeek;
            system_time.0.wDay = (*ptr).0.wDay;
            system_time.0.wHour = (*ptr).0.wHour;
            system_time.0.wMinute = (*ptr).0.wMinute;
            system_time.0.wMilliseconds = (*ptr).0.wMilliseconds;
        }
        system_time
    }
}

#[cfg(feature = "time_rs")]
impl From<SystemTime> for time::OffsetDateTime {
    fn from(file_time: SystemTime) -> Self {
        file_time.as_date_time()
    }
}

#[cfg(feature = "serde")]
impl serde::ser::Serialize for SystemTime {
    #[cfg(feature = "time_rs")]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_date_time().serialize(serializer)
    }

    #[cfg(not(feature = "time_rs"))]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_unix_timestamp().serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_time(high: u32, low: u32) -> FileTime {
        FileTime(FILETIME {
            dwHighDateTime: high,
            dwLowDateTime: low,
        })
    }

    #[test]
    fn as_quad_combines_high_and_low_dwords() {
        // The 64-bit FILETIME is (dwHighDateTime << 32) | dwLowDateTime.
        // Regression guard: the old code OR-ed dwHighDateTime in twice, which
        // for these inputs would yield 0x1234_5678_1234_5678 and silently drop
        // the low dword (i.e. all sub-second precision).
        let ft = file_time(0x1234_5678, 0x9ABC_DEF0);
        assert_eq!(ft.as_quad(), 0x1234_5678_9ABC_DEF0_i64);
    }

    #[test]
    fn low_dword_carries_sub_second_precision() {
        // Two FILETIMEs that differ only in the low dword must produce
        // timestamps that differ by exactly that amount. Under the old bug the
        // low dword was ignored, so this difference would be 0.
        let base = file_time(0x01D5_0000, 0);
        let plus_half_second = file_time(0x01D5_0000, 5_000_000); // 5,000,000 * 100ns = 0.5s
        assert_eq!(
            plus_half_second.as_unix_timestamp_nanos() - base.as_unix_timestamp_nanos(),
            500_000_000,
        );
    }

    #[cfg(any(feature = "time_rs", feature = "serde"))]
    #[test]
    fn from_quad_as_quad_round_trip() {
        let quad = 0x01D5_1234_ABCD_4321_i64;
        assert_eq!(FileTime::from_quad(quad).as_quad(), quad);
    }

    #[test]
    fn as_unix_timestamp_matches_known_value() {
        // 2021-01-01T00:00:00Z == unix 1_609_459_200 s.
        // FILETIME quad = (unix_secs + SECONDS_BETWEEN_1601_AND_1970) * 10_000_000.
        let quad = (1_609_459_200 + SECONDS_BETWEEN_1601_AND_1970) * 10_000_000;
        let ft = file_time((quad >> 32) as u32, (quad & 0xffff_ffff) as u32);
        assert_eq!(ft.as_unix_timestamp(), 1_609_459_200 * MS_IN_SECOND);
        assert_eq!(
            ft.as_unix_timestamp_nanos(),
            1_609_459_200_i128 * NS_IN_SECOND as i128
        );
    }
}

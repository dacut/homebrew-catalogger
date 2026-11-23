//! The time the site was generated, formatted in UTC.

use {
    anyhow::{Context, Result},
    std::time::{SystemTime, UNIX_EPOCH},
};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A moment in time, to the second, in UTC.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Timestamp {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
}

impl Timestamp {
    /// The generation time: `$SOURCE_DATE_EPOCH` if set (the reproducible-builds convention), otherwise now.
    pub fn generation_time() -> Result<Self> {
        match std::env::var("SOURCE_DATE_EPOCH") {
            Ok(value) if !value.trim().is_empty() => {
                let seconds = value.trim().parse().with_context(|| format!("invalid SOURCE_DATE_EPOCH {value:?}"))?;
                Ok(Self::from_unix(seconds))
            }
            _ => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).context("the system clock is before 1970")?;
                Ok(Self::from_unix(i64::try_from(now.as_secs()).context("the system clock is out of range")?))
            }
        }
    }

    /// Converts seconds since the Unix epoch.
    pub fn from_unix(seconds: i64) -> Self {
        let days = seconds.div_euclid(86_400);
        let secs_of_day = seconds.rem_euclid(86_400) as u32;
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: secs_of_day / 3600,
            minute: secs_of_day / 60 % 60,
            second: secs_of_day % 60,
        }
    }

    /// Machine-readable form for `<time datetime>`, e.g. `2026-11-22T18:04:05Z`.
    pub fn iso8601(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// Human-readable form, e.g. `November 22, 2026, 18:04 UTC`.
    pub fn display(&self) -> String {
        format!(
            "{} {}, {}, {:02}:{:02} UTC",
            MONTHS[self.month as usize - 1],
            self.day,
            self.year,
            self.hour,
            self.minute
        )
    }
}

/// Converts days since 1970-01-01 to a proleptic Gregorian (year, month, day), using Howard Hinnant's
/// `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // day of era, [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // year of era, [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year starting March 1, [0, 365]
    let mp = (5 * doy + 2) / 153; // month starting March, [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 {
        mp + 3
    } else {
        mp - 9
    } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_unix_time() {
        for (seconds, iso) in [
            (0, "1970-01-01T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_795_392_245, "2026-11-23T00:04:05Z"),
            (-86_400, "1969-12-31T00:00:00Z"),
        ] {
            assert_eq!(Timestamp::from_unix(seconds).iso8601(), iso, "{seconds}");
        }
    }

    #[test]
    fn displays_for_people() {
        assert_eq!(Timestamp::from_unix(1_795_392_245).display(), "November 23, 2026, 00:04 UTC");
        assert_eq!(Timestamp::from_unix(0).display(), "January 1, 1970, 00:00 UTC");
    }
}

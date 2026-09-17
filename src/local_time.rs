//! Wall-clock ↔ UTC conversion for a configured timezone.
//!
//! Everything Mervyn stores is a `DateTime<Utc>`; everything a human types or reads — in chat or
//! in the vault — is a wall-clock time in `scheduler.timezone`. This module is the one place that
//! crosses between them, so there is exactly one rule for the two awkward nights a year.
//!
//! The offset is always resolved **for the date being converted**, never for today: writing
//! `2026-11-15 09:00` in September (BST) must mean 09:00 GMT on that November morning, not 08:00.
//! That is what [`chrono::TimeZone::from_local_datetime`] does and why the naive
//! "apply the current offset" shortcut is wrong.

use chrono::{DateTime, LocalResult, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

/// Local wall time on `date` → UTC.
///
/// Clocks back: the hour repeats, and the **later** instant is taken. Clocks forward: the hour
/// does not exist, and the time is pushed an hour later into the one that does.
pub fn local_to_utc(date: NaiveDate, hour: u32, min: u32, tz: Tz) -> Option<DateTime<Utc>> {
    let naive = date.and_hms_opt(hour, min, 0)?;
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(_, late) => Some(late.with_timezone(&Utc)),
        LocalResult::None => {
            let naive2 = date.and_hms_opt(hour.saturating_add(1), min, 0)?;
            match tz.from_local_datetime(&naive2) {
                LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
                LocalResult::Ambiguous(_, late) => Some(late.with_timezone(&Utc)),
                LocalResult::None => None,
            }
        }
    }
}

/// What a bare date means: local noon. Far enough from either midnight that a day-granularity
/// item cannot slide into the day before or after when the offset changes.
pub fn local_noon(date: NaiveDate, tz: Tz) -> DateTime<Utc> {
    local_to_utc(date, 12, 0, tz).unwrap_or_else(|| {
        date.and_hms_opt(12, 0, 0)
            .expect("noon is a valid time")
            .and_utc()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn london() -> Tz {
        "Europe/London".parse().unwrap()
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_summer_morning_is_an_hour_ahead_of_utc() {
        let dt = local_to_utc(date(2026, 6, 15), 9, 0, london()).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-06-15T08:00:00+00:00");
    }

    #[test]
    fn a_winter_morning_is_utc() {
        let dt = local_to_utc(date(2026, 11, 15), 9, 0, london()).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-11-15T09:00:00+00:00");
    }

    #[test]
    fn the_offset_comes_from_the_target_date_not_from_today() {
        // Booked during BST for a date in GMT: 09:00 must still be 09:00 on that morning's clock.
        let winter = local_to_utc(date(2026, 11, 15), 9, 0, london()).unwrap();
        let summer = local_to_utc(date(2026, 6, 15), 9, 0, london()).unwrap();
        assert_eq!(winter.hour(), 9, "GMT date keeps the wall-clock hour");
        assert_eq!(summer.hour(), 8, "BST date is an hour ahead of UTC");
    }

    #[test]
    fn the_repeated_hour_resolves_to_the_later_instant() {
        // 2026-10-25: clocks go back at 02:00 BST, so 01:30 happens twice.
        let dt = local_to_utc(date(2026, 10, 25), 1, 30, london()).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-10-25T01:30:00+00:00", "the GMT pass, not the BST one");
    }

    #[test]
    fn the_missing_hour_is_pushed_forward() {
        // 2026-03-29: clocks go forward at 01:00 GMT, so 01:30 does not exist.
        let dt = local_to_utc(date(2026, 3, 29), 1, 30, london()).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-03-29T01:30:00+00:00");
    }

    #[test]
    fn a_bare_date_means_local_noon() {
        assert_eq!(
            local_noon(date(2026, 6, 15), london()).to_rfc3339(),
            "2026-06-15T11:00:00+00:00"
        );
        assert_eq!(
            local_noon(date(2026, 11, 15), london()).to_rfc3339(),
            "2026-11-15T12:00:00+00:00"
        );
    }
}

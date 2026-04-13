//! Best-effort dates/times from natural language for intents (no extra model call).

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

/// Scan for `YYYY-MM-DD` anywhere in `text`.
pub fn find_iso_date(text: &str) -> Option<NaiveDate> {
    for w in text.as_bytes().windows(10) {
        if w.len() == 10 && w[4] == b'-' && w[7] == b'-' {
            if let Ok(s) = std::str::from_utf8(w) {
                if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                    return Some(d);
                }
            }
        }
    }
    None
}

/// Month names (longest first where needed) → calendar month number.
const MONTH_PREFIXES: &[(&str, u32)] = &[
    ("september", 9),
    ("february", 2),
    ("november", 11),
    ("december", 12),
    ("january", 1),
    ("october", 10),
    ("august", 8),
    ("april", 4),
    ("march", 3),
    ("june", 6),
    ("july", 7),
    ("may", 5),
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("sept", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

fn english_month_day(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let lower = text.to_lowercase();
    let mut best: Option<(usize, u32, u32)> = None;
    for (name, month) in MONTH_PREFIXES {
        let mut search_from = 0;
        while let Some(rel) = lower[search_from..].find(name) {
            let abs = search_from + rel;
            let rest = &lower[abs + name.len()..];
            let rest = rest.trim_start();
            let day_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(day) = day_str.parse::<u32>() {
                if (1..=31).contains(&day) {
                    let take = best.map(|(pos, _, _)| abs < pos).unwrap_or(true);
                    if take {
                        best = Some((abs, *month, day));
                    }
                }
            }
            search_from = abs + 1;
        }
    }
    let (_, month, day) = best?;
    let mut year = today.year();
    let nd = NaiveDate::from_ymd_opt(year, month, day)?;
    if nd < today {
        year = year.checked_add(1)?;
        NaiveDate::from_ymd_opt(year, month, day)
    } else {
        Some(nd)
    }
}

pub fn naive_date_from_text(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    find_iso_date(text).or_else(|| english_month_day(text, today))
}

/// If `text` contains ` on ` followed by a month name or `YYYY-MM-DD`, drop that clause (scheduling tail).
pub fn strip_date_clause_after_on(text: &str, _today: NaiveDate) -> String {
    let lower = text.to_lowercase();
    if let Some(on_pos) = lower.find(" on ") {
        let after = text[on_pos + 4..].trim_start();
        if after.is_empty() {
            return text.to_string();
        }
        let lower_after = after.to_lowercase();
        let starts_month = MONTH_PREFIXES
            .iter()
            .any(|(n, _)| lower_after.starts_with(*n));
        let starts_iso = after.len() >= 10
            && after.as_bytes().get(4) == Some(&b'-')
            && after.as_bytes().get(7) == Some(&b'-')
            && NaiveDate::parse_from_str(&after[..10], "%Y-%m-%d").is_ok();
        if starts_month || starts_iso {
            return text[..on_pos].trim().to_string();
        }
    }
    text.to_string()
}

fn last_integer_u32(s: &str) -> Option<u32> {
    let bytes = s.as_bytes();
    let mut i = s.len();
    while i > 0 && !bytes[i - 1].is_ascii_digit() {
        i -= 1;
    }
    if i == 0 {
        return None;
    }
    let mut j = i;
    while j > 0 && bytes[j - 1].is_ascii_digit() {
        j -= 1;
    }
    s[j..i].parse().ok()
}

fn first_integer_u32(s: &str) -> Option<u32> {
    let mut in_num = false;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            if !in_num {
                start = i;
                in_num = true;
            }
        } else if in_num {
            return s[start..i].parse().ok();
        }
    }
    if in_num {
        s[start..].parse().ok()
    } else {
        None
    }
}

/// Parses patterns like `… 9 til 11` / `… 9 till 11` (hours, 24h-style).
fn hours_around_til(text: &str) -> Option<(u32, u32)> {
    let lower = text.to_lowercase();
    for needle in [" til ", " till "] {
        if let Some(i) = lower.find(needle) {
            let left = text[..i].trim();
            let right = text[i + needle.len()..].trim();
            let h1 = last_integer_u32(left)?;
            let h2 = first_integer_u32(right)?;
            if h1 < 24 && h2 < 24 && h1 < h2 {
                return Some((h1, h2));
            }
        }
    }
    None
}

/// Default reminder due: ISO or English month/day at 09:00 UTC, else tomorrow 09:00 UTC.
pub fn due_datetime_from_reminder_text(text: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    let today = now.date_naive();
    if let Some(d) = naive_date_from_text(text, today) {
        if let Some(naive) = d.and_hms_opt(9, 0, 0) {
            return naive.and_utc();
        }
    }
    (now + Duration::days(1))
        .date_naive()
        .and_hms_opt(9, 0, 0)
        .expect("valid time")
        .and_utc()
}

fn has_whole_word(text: &str, word: &str) -> bool {
    let w = word.to_lowercase();
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|part| part == w)
}

/// Local wall time on `date` in `tz` → UTC (handles DST gaps/ambiguous with sensible picks).
fn naive_local_to_utc(date: NaiveDate, hour: u32, min: u32, tz: Tz) -> Option<DateTime<Utc>> {
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

/// `H:MM` or `H:MM am/pm` after ` at ` if possible, else first `H:MM` in `text`.
fn scan_h_colon_m_fragment(s: &str) -> Option<(u32, u32, Option<bool>)> {
    let bytes = s.as_bytes();
    for i in 1..bytes.len() {
        if bytes[i] != b':' {
            continue;
        }
        let mut j = i;
        while j > 0 && bytes[j - 1].is_ascii_digit() {
            j -= 1;
        }
        if j == i {
            continue;
        }
        if !(1..=2).contains(&(i - j)) {
            continue;
        }
        let left = s.get(j..i)?.parse::<u32>().ok()?;
        if left > 23 {
            continue;
        }
        let mut k = i + 1;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            k += 1;
        }
        if k == i + 1 {
            continue;
        }
        let right_str = s.get(i + 1..k)?;
        if right_str.len() > 2 {
            continue;
        }
        let min = right_str.parse::<u32>().ok()?;
        if min > 59 {
            continue;
        }
        let rest = s.get(k..).unwrap_or("").trim_start();
        let lower = rest.to_lowercase();
        let ampm = if lower == "am" || lower.starts_with("am ") || lower.starts_with("a.m.") {
            Some(false)
        } else if lower == "pm" || lower.starts_with("pm ") || lower.starts_with("p.m.") {
            Some(true)
        } else {
            None
        };
        return Some((left, min, ampm));
    }
    None
}

fn parse_clock_in_text(text: &str) -> Option<(u32, u32, Option<bool>)> {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.find(" at ") {
        if let Some(v) = scan_h_colon_m_fragment(text[pos + 4..].trim_start()) {
            return Some(v);
        }
    }
    scan_h_colon_m_fragment(text)
}

fn to_24h_clock(
    h: u32,
    m: u32,
    ampm: Option<bool>,
    date: NaiveDate,
    local_now: DateTime<Tz>,
    tz: Tz,
) -> Option<(u32, u32)> {
    if m > 59 {
        return None;
    }
    if let Some(pm) = ampm {
        if h > 23 {
            return None;
        }
        if pm {
            let h24 = if h == 12 { 12 } else { h + 12 };
            return Some((h24, m));
        }
        let h24 = if h == 12 { 0 } else { h };
        return Some((h24, m));
    }

    if h > 23 {
        return None;
    }
    if h >= 13 {
        return Some((h, m));
    }
    if h == 12 {
        return Some((12, m));
    }

    if date > local_now.date_naive() {
        return Some((h + 12, m));
    }

    let now_utc = local_now.with_timezone(&Utc);
    let am_utc = naive_local_to_utc(date, h, m, tz)?;
    let pm_utc = naive_local_to_utc(date, h + 12, m, tz)?;
    if am_utc > now_utc {
        Some((h, m))
    } else if pm_utc > now_utc {
        Some((h + 12, m))
    } else {
        Some((h + 12, m))
    }
}

/// Event start/end: ISO / month-day / **today** / **tomorrow** / `H:MM` (see `tz`); else start = now + 24h.
///
/// Clock times without `am`/`pm` use `tz` local wall date `date`; ambiguous 1–11 pick AM if still
/// in the future today, else PM. Default time when no clock is **09:00 UTC** for ISO/month-only
/// phrases, **09:00 local** for today/tomorrow/tonight or time-only-same-day.
pub fn event_timing_from_text(text: &str, now: DateTime<Utc>, tz: Tz) -> (DateTime<Utc>, Option<DateTime<Utc>>) {
    let local_now = now.with_timezone(&tz);
    let today_utc = now.date_naive();

    let iso = find_iso_date(text);
    let month_day = naive_date_from_text(text, today_utc);
    let has_tomorrow = has_whole_word(text, "tomorrow");
    let has_today = has_whole_word(text, "today") || has_whole_word(text, "tonight");

    let mut anchor: Option<NaiveDate> = None;
    let mut nine_is_local = false;

    if let Some(d) = iso {
        anchor = Some(d);
        nine_is_local = has_today || has_tomorrow;
    } else if let Some(d) = month_day {
        anchor = Some(d);
        nine_is_local = has_today || has_tomorrow;
    } else if has_tomorrow {
        anchor = Some(
            local_now
                .date_naive()
                .checked_add_signed(Duration::days(1))
                .unwrap_or(local_now.date_naive()),
        );
        nine_is_local = true;
    } else if has_today {
        anchor = Some(local_now.date_naive());
        nine_is_local = true;
    }

    let clock = parse_clock_in_text(text);

    if anchor.is_none() {
        if clock.is_some() {
            anchor = Some(local_now.date_naive());
            nine_is_local = true;
        } else {
            return (now + Duration::hours(24), None);
        }
    }

    let date = anchor.expect("set above");

    if let Some((h1, h2)) = hours_around_til(text) {
        let Some(start_utc) = naive_local_to_utc(date, h1, 0, tz) else {
            return (now + Duration::hours(24), None);
        };
        let Some(end_utc) = naive_local_to_utc(date, h2, 0, tz) else {
            return (now + Duration::hours(24), None);
        };
        return (start_utc, Some(end_utc));
    }

    if let Some((h, min, ampm)) = clock {
        if let Some((h24, m)) = to_24h_clock(h, min, ampm, date, local_now, tz) {
            if let Some(start_utc) = naive_local_to_utc(date, h24, m, tz) {
                return (start_utc, None);
            }
        }
    }

    if nine_is_local {
        let Some(start_utc) = naive_local_to_utc(date, 9, 0, tz) else {
            return (now + Duration::hours(24), None);
        };
        return (start_utc, None);
    }

    let Some(start_naive) = date.and_hms_opt(9, 0, 0) else {
        return (now + Duration::hours(24), None);
    };
    (start_naive.and_utc(), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dentist_april_8_nine_til_eleven_utc() {
        let now = DateTime::parse_from_rfc3339("2026-04-05T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let text = "I have a dentist appointment on April 8th 9 til 11";
        let (start, end) = event_timing_from_text(text, now, chrono_tz::UTC);
        assert_eq!(start, NaiveDate::from_ymd_opt(2026, 4, 8).unwrap().and_hms_opt(9, 0, 0).unwrap().and_utc());
        assert_eq!(
            end,
            Some(
                NaiveDate::from_ymd_opt(2026, 4, 8)
                    .unwrap()
                    .and_hms_opt(11, 0, 0)
                    .unwrap()
                    .and_utc()
            )
        );
    }

    #[test]
    fn no_date_falls_back_to_now_plus_24h() {
        let now = DateTime::parse_from_rfc3339("2026-04-05T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (start, end) = event_timing_from_text("dentist sometime", now, chrono_tz::UTC);
        assert!(end.is_none());
        assert_eq!(start, now + Duration::hours(24));
    }

    /// BST: 15:50 local → 14:50 UTC on 2026-04-14.
    #[test]
    fn today_at_three_fifty_uses_scheduler_tz() {
        let now = DateTime::parse_from_rfc3339("2026-04-14T14:46:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let text = "I have to wash my capybara at 3:50 today";
        let (start, end) = event_timing_from_text(text, now, chrono_tz::Europe::London);
        assert!(end.is_none());
        let want = DateTime::parse_from_rfc3339("2026-04-14T14:50:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(start, want);
    }

    #[test]
    fn tomorrow_at_noon_explicit_pm() {
        let now = DateTime::parse_from_rfc3339("2026-04-14T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (start, end) = event_timing_from_text(
            "Call mum tomorrow at 12:30 pm",
            now,
            chrono_tz::Europe::London,
        );
        assert!(end.is_none());
        let want = DateTime::parse_from_rfc3339("2026-04-15T11:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(start, want);
    }
}

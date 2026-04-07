//! Best-effort dates/times from natural language for intents (no extra model call).

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};

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

/// Event start/end from user text: parsed date + optional `H til H` hours; else start = now + 24h.
pub fn event_timing_from_text(text: &str, now: DateTime<Utc>) -> (DateTime<Utc>, Option<DateTime<Utc>>) {
    let today = now.date_naive();
    let Some(date) = naive_date_from_text(text, today) else {
        return (now + Duration::hours(24), None);
    };

    if let Some((h1, h2)) = hours_around_til(text) {
        let Some(start_naive) = date.and_hms_opt(h1, 0, 0) else {
            return (now + Duration::hours(24), None);
        };
        let Some(end_naive) = date.and_hms_opt(h2, 0, 0) else {
            return (now + Duration::hours(24), None);
        };
        return (start_naive.and_utc(), Some(end_naive.and_utc()));
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
        let (start, end) = event_timing_from_text(text, now);
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
        let (start, end) = event_timing_from_text("dentist sometime", now);
        assert!(end.is_none());
        assert_eq!(start, now + Duration::hours(24));
    }
}

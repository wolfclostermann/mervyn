//! Vault Markdown parsing via pulldown-cmark (task lists + ATX headings).
//! Leading YAML front matter (Obsidian-style `---` … `---`) is stripped and parsed with [`serde_yaml`];
//! the Markdown body is what pulldown sees. Parsed front matter is reserved for future use.

use std::borrow::Cow;
use std::hash::Hasher;
use std::ops::Range;

use chrono::NaiveDate;
use chrono_tz::Tz;
use fnv::FnvHasher;
use pulldown_cmark::{Event as MdEvent, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_yaml::Value as YamlValue;

use super::marker;
use crate::local_time::{local_noon, local_to_utc};
use crate::storage::{Event, Recurrence, Reminder, WorklogEntry};

/// Obsidian/Jekyll-style YAML block at the top of a file, as `(parsed, body_offset)` where
/// `body_offset` is a **byte offset into `raw`** — `&raw[body_offset..]` is the Markdown body.
/// Returning an offset rather than an owned string is what lets write-back map a parsed item's
/// span back onto a real position in the file; joining lines would also silently normalise CRLF.
///
/// Tolerant: unclosed fence → the whole file is the body. Closing line may be `---` or `...`.
/// A BOM is skipped. Invalid YAML still yields a stripped body, with `None` for the metadata.
pub fn front_matter(raw: &str) -> (Option<YamlValue>, usize) {
    let bom_len = if raw.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let after_bom = &raw[bom_len..];
    let start = bom_len + (after_bom.len() - after_bom.trim_start().len());
    let s = &raw[start..];
    if !s.starts_with("---") {
        return (None, bom_len);
    }

    let mut cursor = 0usize;
    let mut yaml_start: Option<usize> = None;
    loop {
        let line_end = s[cursor..].find('\n').map_or(s.len(), |i| cursor + i);
        let line = s[cursor..line_end].trim_end_matches('\r');
        let next = if line_end < s.len() { line_end + 1 } else { s.len() };

        match yaml_start {
            // The opening fence has to be a bare `---` on its own line.
            None => {
                if line.trim() != "---" {
                    return (None, bom_len);
                }
                yaml_start = Some(next);
            }
            Some(ys) => {
                let t = line.trim();
                if t == "---" || t == "..." {
                    let meta = serde_yaml::from_str(&s[ys..cursor]).ok();
                    return (meta, start + next);
                }
            }
        }

        if line_end == s.len() {
            return (None, bom_len);
        }
        cursor = next;
    }
}

/// [`front_matter`] as a borrowed body slice, for callers that do not need offsets.
#[allow(dead_code)] // Read-only convenience; the sync path wants the offset.
pub fn strip_yaml_front_matter(raw: &str) -> (Option<YamlValue>, Cow<'_, str>) {
    let (meta, offset) = front_matter(raw);
    (meta, Cow::Borrowed(&raw[offset..]))
}

/// FNV-1a 64-bit — stable across Rust versions for vault-derived primary keys.
pub(super) fn stable_vault_row_id(prefix: &[u8], key: &str) -> u64 {
    let mut h = FnvHasher::default();
    h.write(prefix);
    h.write(key.as_bytes());
    h.finish()
}

/// `HH:MM`, 24-hour. Deliberately strict: anything else is treated as prose, not a time.
fn parse_hh_mm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    if h.is_empty() || h.len() > 2 || m.len() != 2 {
        return None;
    }
    let h: u32 = h.parse().ok()?;
    let m: u32 = m.parse().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}

/// Split `09:00–10:30` into its halves. The range separator is a dash with **no** spaces, which
/// is what keeps it distinct from the ` — ` that separates a heading's date from its title.
fn split_time_range(s: &str) -> (&str, Option<&str>) {
    for sep in ['\u{2013}', '\u{2014}', '-'] {
        if let Some((a, b)) = s.split_once(sep) {
            return (a, Some(b));
        }
    }
    (s, None)
}

/// `YYYY-MM-DD`, optionally followed by `HH:MM` or `HH:MM–HH:MM` in **local** wall-clock time.
///
/// Tolerant: a trailing token that is not a time is ignored rather than failing the whole parse,
/// so a line someone typed loosely still yields its date.
fn parse_date_and_times(s: &str) -> Option<(NaiveDate, Option<(u32, u32)>, Option<(u32, u32)>)> {
    let mut parts = s.trim().split_whitespace();
    let date = NaiveDate::parse_from_str(parts.next()?, "%Y-%m-%d").ok()?;
    let Some(rest) = parts.next() else {
        return Some((date, None, None));
    };
    let (start_str, end_str) = split_time_range(rest);
    let Some(start) = parse_hh_mm(start_str) else {
        return Some((date, None, None));
    };
    let end = end_str.and_then(parse_hh_mm);
    Some((date, Some(start), end))
}

/// A parsed date and optional local time as the UTC instant it denotes.
fn at_local(date: NaiveDate, time: Option<(u32, u32)>, tz: Tz) -> chrono::DateTime<chrono::Utc> {
    match time {
        Some((h, m)) => local_to_utc(date, h, m, tz).unwrap_or_else(|| local_noon(date, tz)),
        None => local_noon(date, tz),
    }
}

/// Known cadences become their own variant so a rendered reminder parses back to what it was.
fn parse_recurrence(s: &str) -> Recurrence {
    match s.trim().to_lowercase().as_str() {
        "daily" => Recurrence::Daily,
        "weekly" => Recurrence::Weekly,
        "monthly" => Recurrence::Monthly,
        _ => Recurrence::Custom(s.trim().to_string()),
    }
}

fn parse_reminder_body(
    s: &str,
) -> (String, Option<(NaiveDate, Option<(u32, u32)>)>, Option<Recurrence>) {
    let mut recurrence = None;
    let mut due = None;
    let mut body_part = s.to_string();

    if let Some(idx) = body_part.rfind(" — recurs ") {
        let tail = body_part[idx + " — recurs ".len()..].trim();
        recurrence = Some(parse_recurrence(tail));
        body_part.truncate(idx);
    }

    if let Some(idx) = body_part.rfind(" — due ") {
        let when = body_part[idx + " — due ".len()..].trim().to_string();
        if let Some((date, time, _end)) = parse_date_and_times(&when) {
            due = Some((date, time));
        }
        body_part.truncate(idx);
    }

    let body = body_part.trim().to_string();
    (body, due, recurrence)
}

/// A markdown event paired with its byte range **in the raw file** — front matter included, so a
/// range can be spliced back into the bytes on disk without any further bookkeeping.
type Ev<'a> = (MdEvent<'a>, Range<usize>);

/// An item as found in a file: the parsed row, plus what write-back needs to know about its id.
///
/// `item.id` comes from the line's marker when it has one. That is the whole point of the marker:
/// an edited line keeps its identity, where the hash fallback would mint a new id and orphan the
/// row it came from.
#[derive(Debug, Clone)]
pub struct FoundItem<T> {
    pub item: T,
    /// False when the id was derived from [`stable_vault_row_id`] because the line carried no
    /// marker — i.e. this item is a candidate for marker back-fill.
    pub had_marker: bool,
    /// Byte offset in the raw file at which the marker should be spliced when it is missing.
    pub marker_at: usize,
    /// Written before the marker: a space for a list item, a newline for an event heading (whose
    /// marker lives on its own line underneath).
    pub marker_sep: &'static str,
}

impl<T> FoundItem<T> {
    /// The text to splice at [`marker_at`](Self::marker_at), or `None` if already marked.
    pub fn backfill(&self, id: u64) -> Option<(usize, String)> {
        (!self.had_marker).then(|| (self.marker_at, format!("{}{}", self.marker_sep, marker::render(id))))
    }
}

fn spanned_events(raw: &str, options: Options) -> Vec<Ev<'_>> {
    let (_meta, body_offset) = front_matter(raw);
    Parser::new_ext(&raw[body_offset..], options)
        .into_offset_iter()
        .map(|(e, r)| (e, (r.start + body_offset)..(r.end + body_offset)))
        .collect()
}

/// End of the first line of `range`, *before* any line terminator — the point a trailing marker
/// is spliced at. CRLF is handled explicitly: inserting between the `\r` and the `\n` would put
/// the marker on a line of its own and leave a stray carriage return behind it.
fn first_line_end(raw: &str, range: &Range<usize>) -> usize {
    let end = match raw[range.start..range.end].find('\n') {
        Some(i) => range.start + i,
        None => range.end,
    };
    if raw[range.start..end].ends_with('\r') {
        end - 1
    } else {
        end
    }
}

/// The line following `line_end`, if there is one, as `(content, end)`.
fn next_line(raw: &str, line_end: usize) -> Option<(&str, usize)> {
    let start = match raw[line_end..].find('\n') {
        Some(i) => line_end + i + 1,
        None => return None,
    };
    if start >= raw.len() {
        return None;
    }
    let end = raw[start..].find('\n').map_or(raw.len(), |i| start + i);
    Some((raw[start..end].trim_end_matches('\r'), end))
}

/// Walk events until `end` is true; does not consume the matching event. Always advances on other events.
fn collect_plain_until(events: &[Ev<'_>], i: &mut usize, mut end: impl FnMut(&MdEvent<'_>) -> bool) -> String {
    let mut s = String::new();
    while *i < events.len() && !end(&events[*i].0) {
        match &events[*i].0 {
            MdEvent::Text(t) => s.push_str(t),
            MdEvent::Code(c) => s.push_str(c),
            MdEvent::SoftBreak => s.push(' '),
            MdEvent::HardBreak => s.push(' '),
            _ => {}
        }
        *i += 1;
    }
    s
}

fn consume_if(events: &[Ev<'_>], i: &mut usize, pred: impl FnOnce(&MdEvent<'_>) -> bool) {
    if *i < events.len() && pred(&events[*i].0) {
        *i += 1;
    }
}

fn is_h2_start(ev: &MdEvent<'_>) -> bool {
    matches!(
        ev,
        MdEvent::Start(Tag::Heading {
            level: HeadingLevel::H2,
            ..
        })
    )
}

/// GitHub-style task list items → [`Reminder`], with the position of each id marker.
pub fn find_reminders(raw: &str, tz: Tz) -> Vec<FoundItem<Reminder>> {
    let events = spanned_events(raw, Options::ENABLE_TASKLISTS);
    let mut i = 0;
    let mut out = Vec::new();

    while i < events.len() {
        if matches!(events[i].0, MdEvent::Start(Tag::Item)) {
            let item_range = events[i].1.clone();
            i += 1;
            let done = if i < events.len() {
                if let MdEvent::TaskListMarker(checked) = events[i].0 {
                    i += 1;
                    Some(checked)
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(done) = done {
                let text = collect_plain_until(&events, &mut i, |e| matches!(e, MdEvent::End(TagEnd::Item)));
                consume_if(&events, &mut i, |e| matches!(e, MdEvent::End(TagEnd::Item)));
                let (body, due, recurrence) = parse_reminder_body(text.trim());
                let due = match due {
                    Some((date, time)) => at_local(date, time, tz),
                    None => chrono::Utc::now(),
                };

                // Only the item's own first line: a marker further down belongs to a nested item.
                let line_end = first_line_end(raw, &item_range);
                let found = marker::find(&raw[item_range.start..line_end]);
                let had_marker = found.is_some();
                let id = found.map_or_else(
                    || stable_vault_row_id(b"rem:", &format!("{body}|{due}|{done}")),
                    |f| f.id,
                );

                out.push(FoundItem {
                    item: Reminder { id, body, due, recurrence, done },
                    had_marker,
                    marker_at: line_end,
                    marker_sep: " ",
                });
            } else {
                while i < events.len() && !matches!(events[i].0, MdEvent::End(TagEnd::Item)) {
                    i += 1;
                }
                consume_if(&events, &mut i, |e| matches!(e, MdEvent::End(TagEnd::Item)));
            }
        } else {
            i += 1;
        }
    }

    out
}

/// [`find_reminders`] without the write-back bookkeeping.
#[allow(dead_code)] // Sibling of `parse_worklog`; sync itself needs the marker positions.
pub fn parse_reminders(text: &str, tz: Tz) -> Vec<Reminder> {
    find_reminders(text, tz).into_iter().map(|f| f.item).collect()
}

fn take_paragraph(events: &[Ev<'_>], i: &mut usize) -> Option<String> {
    if !matches!(events.get(*i).map(|e| &e.0), Some(MdEvent::Start(Tag::Paragraph))) {
        return None;
    }
    *i += 1;
    let mut s = String::new();
    while *i < events.len() {
        match &events[*i].0 {
            MdEvent::End(TagEnd::Paragraph) => {
                *i += 1;
                return Some(s);
            }
            MdEvent::Text(t) => s.push_str(t),
            MdEvent::Code(c) => s.push_str(c),
            MdEvent::SoftBreak => s.push('\n'),
            MdEvent::HardBreak => s.push('\n'),
            _ => {}
        }
        *i += 1;
    }
    Some(s)
}

fn parse_tags_line(s: &str) -> Option<Vec<String>> {
    let t = s.trim();
    let rest = t
        .strip_prefix("Tags:")
        .or_else(|| t.strip_prefix("tags:"))?;
    let tags: Vec<String> = rest
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    Some(tags)
}

/// Last `Tags:` / `tags:` in a list item (CommonMark may merge it into the final bullet via a soft break).
fn split_item_body_and_tags(body: &str) -> (String, Option<Vec<String>>) {
    let body = body.trim();
    let pos = body.rfind("Tags:").or_else(|| body.rfind("tags:"));
    let Some(pos) = pos else {
        return (body.to_string(), None);
    };
    let tail = body[pos..].trim_start();
    let Some(parsed) = parse_tags_line(tail) else {
        return (body.to_string(), None);
    };
    let main = body[..pos].trim().to_string();
    (main, Some(parsed))
}

/// `## YYYY-MM-DD — title` sections, with the position of each id marker.
///
/// An event's marker sits on its own line directly under the heading — a heading is a leaf block,
/// so there is nowhere inline to put it without it becoming part of the title.
pub fn find_events(raw: &str, tz: Tz) -> Vec<FoundItem<Event>> {
    let events = spanned_events(raw, Options::empty());
    let mut i = 0;
    let mut out = Vec::new();

    while i < events.len() {
        if is_h2_start(&events[i].0) {
            let heading_range = events[i].1.clone();
            i += 1;
            let heading = collect_plain_until(&events, &mut i, |e| {
                matches!(e, MdEvent::End(TagEnd::Heading(HeadingLevel::H2)))
            });
            consume_if(&events, &mut i, |e| {
                matches!(e, MdEvent::End(TagEnd::Heading(HeadingLevel::H2)))
            });
            let heading = heading.trim();
            let Some((date_str, title)) = heading.split_once(" — ") else {
                continue;
            };
            let Some((date, time, end_time)) = parse_date_and_times(date_str) else {
                continue;
            };
            let start = at_local(date, time, tz);
            let end = end_time.map(|(h, m)| at_local(date, Some((h, m)), tz));
            let mut desc_lines: Vec<String> = Vec::new();
            let mut tags: Vec<String> = Vec::new();

            while i < events.len() && !is_h2_start(&events[i].0) {
                if let Some(para) = take_paragraph(&events, &mut i) {
                    let trimmed = para.trim_end();
                    if let Some(t) = parse_tags_line(trimmed) {
                        tags = t;
                    } else if !trimmed.is_empty() {
                        desc_lines.push(trimmed.to_string());
                    }
                } else {
                    i += 1;
                }
            }

            let description = if desc_lines.is_empty() {
                None
            } else {
                Some(desc_lines.join("\n"))
            };

            let heading_end = first_line_end(raw, &heading_range);
            let found = next_line(raw, heading_end).and_then(|(line, _)| {
                let t = line.trim();
                marker::find(t).filter(|_| marker::strip(t).trim().is_empty())
            });
            let had_marker = found.is_some();
            let id = found.map_or_else(
                || stable_vault_row_id(b"evt:", &format!("{title}|{start}")),
                |f| f.id,
            );

            out.push(FoundItem {
                item: Event {
                    id,
                    title: title.trim().to_string(),
                    description,
                    start,
                    end,
                    tags,
                },
                had_marker,
                marker_at: heading_end,
                marker_sep: "\n",
            });
        } else {
            i += 1;
        }
    }

    out
}

/// [`find_events`] without the write-back bookkeeping.
#[allow(dead_code)] // Sibling of `parse_worklog`; sync itself needs the marker positions.
pub fn parse_events(text: &str, tz: Tz) -> Vec<Event> {
    find_events(text, tz).into_iter().map(|f| f.item).collect()
}

fn collect_list_item_text(events: &[Ev<'_>], i: &mut usize) -> String {
    if !matches!(events.get(*i).map(|e| &e.0), Some(MdEvent::Start(Tag::Item))) {
        return String::new();
    }
    *i += 1;
    let s = collect_plain_until(events, i, |e| matches!(e, MdEvent::End(TagEnd::Item)));
    consume_if(events, i, |e| matches!(e, MdEvent::End(TagEnd::Item)));
    s.trim().to_string()
}

/// `## YYYY-MM-DD` sections; list items as worklog bullets; optional `Tags:` paragraph.
///
/// No marker handling: the worklog is read-only for Mervyn. `worklog.md` is a symlink into the
/// worklog clone, and writing into that working tree would break `git pull --ff-only`.
/// See `docs/two-way-vault-sync.md`.
pub fn parse_worklog(text: &str, tz: Tz) -> Vec<WorklogEntry> {
    let events = spanned_events(text, Options::empty());
    let mut i = 0;
    let mut out = Vec::new();

    while i < events.len() {
        if is_h2_start(&events[i].0) {
            i += 1;
            let heading = collect_plain_until(&events, &mut i, |e| {
                matches!(e, MdEvent::End(TagEnd::Heading(HeadingLevel::H2)))
            });
            consume_if(&events, &mut i, |e| {
                matches!(e, MdEvent::End(TagEnd::Heading(HeadingLevel::H2)))
            });
            let Ok(day) = NaiveDate::parse_from_str(heading.trim(), "%Y-%m-%d") else {
                continue;
            };
            let day_start = local_noon(day, tz);
            let mut bullets: Vec<String> = Vec::new();
            let mut tags: Vec<String> = Vec::new();

            while i < events.len() && !is_h2_start(&events[i].0) {
                match &events[i].0 {
                    MdEvent::Start(Tag::List(_)) => {
                        i += 1;
                        while i < events.len() && !matches!(events[i].0, MdEvent::End(TagEnd::List(_))) {
                            if matches!(events[i].0, MdEvent::Start(Tag::Item)) {
                                let body = collect_list_item_text(&events, &mut i);
                                if !body.is_empty() {
                                    let (main, tag_opt) = split_item_body_and_tags(&body);
                                    if let Some(t) = tag_opt {
                                        tags = t;
                                    }
                                    if !main.is_empty() {
                                        bullets.push(main);
                                    }
                                }
                            } else {
                                i += 1;
                            }
                        }
                        consume_if(&events, &mut i, |e| matches!(e, MdEvent::End(TagEnd::List(_))));
                    }
                    MdEvent::Start(Tag::Paragraph) => {
                        if let Some(para) = take_paragraph(&events, &mut i) {
                            let trimmed = para.trim();
                            if let Some(t) = parse_tags_line(trimmed) {
                                tags = t;
                            }
                        }
                    }
                    _ => i += 1,
                }
            }

            for body in bullets {
                let key = format!("{day}|{body}");
                let id = stable_vault_row_id(b"wlog:", &key);
                out.push(WorklogEntry {
                    id,
                    timestamp: day_start,
                    body,
                    tags: tags.clone(),
                    project: None,
                });
            }
        } else {
            i += 1;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::vault::write::splice;

    fn london() -> Tz {
        "Europe/London".parse().unwrap()
    }

    /// Splice in every missing marker, the way the reconciler does.
    fn backfill_reminders(raw: &str) -> String {
        let mut edits: Vec<(usize, String)> = find_reminders(raw, london())
            .iter()
            .filter_map(|f| f.backfill(f.item.id))
            .collect();
        splice(raw, &mut edits)
    }

    fn backfill_events(raw: &str) -> String {
        let mut edits: Vec<(usize, String)> = find_events(raw, london())
            .iter()
            .filter_map(|f| f.backfill(f.item.id))
            .collect();
        splice(raw, &mut edits)
    }

    #[test]
    fn a_reminder_time_is_local_wall_clock_on_the_date_written() {
        // Written while it is BST, for a date that is GMT: 09:00 stays 09:00 that morning.
        let winter = &find_reminders("- [ ] Pay tax — due 2026-11-15 09:00\n", london())[0].item;
        assert_eq!(winter.due.to_rfc3339(), "2026-11-15T09:00:00+00:00");

        let summer = &find_reminders("- [ ] Pay tax — due 2026-06-15 09:00\n", london())[0].item;
        assert_eq!(summer.due.to_rfc3339(), "2026-06-15T08:00:00+00:00");
    }

    #[test]
    fn a_bare_due_date_means_local_noon() {
        let r = &find_reminders("- [ ] Pay tax — due 2026-06-15\n", london())[0].item;
        assert_eq!(r.due.to_rfc3339(), "2026-06-15T11:00:00+00:00");
    }

    #[test]
    fn a_trailing_token_that_is_not_a_time_is_ignored_rather_than_failing_the_date() {
        let r = &find_reminders("- [ ] Pay tax — due 2026-06-15 sometime\n", london())[0].item;
        assert_eq!(r.due.to_rfc3339(), "2026-06-15T11:00:00+00:00");
        assert_eq!(r.body, "Pay tax");
    }

    #[test]
    fn recurrence_words_map_to_their_own_variants() {
        let cases = [
            ("daily", Recurrence::Daily),
            ("weekly", Recurrence::Weekly),
            ("monthly", Recurrence::Monthly),
            ("yearly", Recurrence::Custom("yearly".into())),
        ];
        for (word, expected) in cases {
            let line = format!("- [ ] Pay tax — due 2026-06-15 — recurs {word}\n");
            assert_eq!(find_reminders(&line, london())[0].item.recurrence, Some(expected));
        }
    }

    #[test]
    fn an_event_heading_carries_an_optional_time_and_range() {
        let e = &find_events("## 2026-09-23 14:30 — Hospital\n", london())[0].item;
        assert_eq!(e.start.to_rfc3339(), "2026-09-23T13:30:00+00:00");
        assert!(e.end.is_none());

        let ranged = &find_events("## 2026-09-23 14:30–16:00 — Hospital\n", london())[0].item;
        assert_eq!(ranged.start.to_rfc3339(), "2026-09-23T13:30:00+00:00");
        assert_eq!(ranged.end.unwrap().to_rfc3339(), "2026-09-23T15:00:00+00:00");
        assert_eq!(ranged.title, "Hospital");
    }

    #[test]
    fn an_event_heading_without_a_time_still_parses_as_before() {
        let e = &find_events("## 2026-04-05 — **Gig** at Tap\nDoors 7pm\n", london())[0].item;
        assert_eq!(e.title, "Gig at Tap");
        assert_eq!(e.start.to_rfc3339(), "2026-04-05T11:00:00+00:00");
    }

    #[test]
    fn an_unmarked_reminder_gains_a_marker_carrying_the_id_it_synced_under() {
        let raw = "- [ ] Pay tax — due 2026-04-10\n";
        let id = find_reminders(raw, london())[0].item.id;

        let marked = backfill_reminders(raw);
        assert_eq!(
            marked,
            format!("- [ ] Pay tax — due 2026-04-10 {}\n", marker::render(id))
        );

        // The bootstrap is what makes the migration free: the row keeps the id it already has.
        let reparsed = &find_reminders(&marked, london())[0];
        assert_eq!(reparsed.item.id, id);
        assert!(reparsed.had_marker);
    }

    #[test]
    fn a_marked_reminder_keeps_its_id_when_the_line_is_edited() {
        // The bug this phase exists to fix: ticking the box used to rehash the line into a new
        // id, leaving the unticked row in redb to fire for ever.
        let before = "- [ ] Pay tax — due 2026-04-10 <!--mv:2a-->\n";
        let after = "- [x] Pay the tax bill — due 2026-04-11 <!--mv:2a-->\n";

        let a = &find_reminders(before, london())[0].item;
        let b = &find_reminders(after, london())[0].item;

        assert_eq!(a.id, b.id, "id must survive an edit");
        assert!(!a.done && b.done, "the edit itself must still be read");
        assert_eq!(b.body, "Pay the tax bill");
    }

    #[test]
    fn an_unmarked_edit_still_rehashes_which_is_why_the_backfill_runs_once_up_front() {
        let a = &find_reminders("- [ ] Pay tax — due 2026-04-10\n", london())[0].item;
        let b = &find_reminders("- [x] Pay tax — due 2026-04-10\n", london())[0].item;
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn a_marker_does_not_leak_into_the_reminder_body_or_its_fields() {
        let r = &find_reminders("- [ ] Pay tax — due 2026-04-10 — recurs yearly <!--mv:ff-->\n", london())[0].item;
        assert_eq!(r.body, "Pay tax");
        assert_eq!(r.id, 255);
        assert!(matches!(r.recurrence, Some(Recurrence::Custom(ref s)) if s == "yearly"));
        assert_eq!(r.due, local_noon(NaiveDate::from_ymd_opt(2026, 4, 10).unwrap(), london()));
    }

    #[test]
    fn backfill_is_idempotent() {
        let raw = "- [ ] One — due 2026-04-10\n- [x] Two\n";
        let once = backfill_reminders(raw);
        let twice = backfill_reminders(&once);
        assert_eq!(once, twice, "a second pass must not add a second marker");
    }

    #[test]
    fn backfill_splices_before_the_carriage_return_on_crlf_files() {
        let raw = "- [ ] Pay tax\r\n- [ ] Call bank\r\n";
        let marked = backfill_reminders(raw);
        for line in marked.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(line.ends_with("-->"), "marker not at end of line: {line:?}");
            assert!(!line.contains('\r'), "stray carriage return in {line:?}");
        }
        assert_eq!(find_reminders(&marked, london()).len(), 2);
        assert!(find_reminders(&marked, london()).iter().all(|f| f.had_marker));
    }

    #[test]
    fn an_event_marker_sits_under_the_heading_and_stays_out_of_the_description() {
        let raw = "## 2026-04-05 — **Gig** at Tap\nDoors 7pm\n";
        let id = find_events(raw, london())[0].item.id;

        let marked = backfill_events(raw);
        assert_eq!(
            marked,
            format!("## 2026-04-05 — **Gig** at Tap\n{}\nDoors 7pm\n", marker::render(id))
        );

        let e = &find_events(&marked, london())[0];
        assert!(e.had_marker);
        assert_eq!(e.item.id, id);
        assert_eq!(e.item.title, "Gig at Tap");
        assert_eq!(e.item.description.as_deref(), Some("Doors 7pm"));
    }

    #[test]
    fn a_marked_event_keeps_its_id_when_retitled() {
        let before = "## 2026-04-05 — Gig\n<!--mv:7b-->\nDoors 7pm\n";
        let after = "## 2026-04-05 — Gig at the Tap\n<!--mv:7b-->\nDoors 8pm\n";
        assert_eq!(find_events(before, london())[0].item.id, find_events(after, london())[0].item.id);
        assert_eq!(find_events(after, london())[0].item.title, "Gig at the Tap");
    }

    #[test]
    fn a_comment_under_a_heading_that_is_not_a_marker_is_left_alone() {
        let raw = "## 2026-04-05 — Gig\n<!-- ask about parking -->\nDoors 7pm\n";
        let f = &find_events(raw, london())[0];
        assert!(!f.had_marker, "an ordinary comment must not be read as an id");
        assert_eq!(f.item.description.as_deref(), Some("Doors 7pm"));
    }

    #[test]
    fn backfill_leaves_everything_the_parser_does_not_model_byte_identical() {
        let raw = "---\ntitle: Reminders\ntags: [inbox]\n---\n\n# Reminders\n\nSome prose Mervyn knows nothing about.\n\n> [!note] a callout\n> with a second line\n\n- [ ] Pay tax — due 2026-04-10\n\n*Emphasis and a [link](https://example.com) at the end.*\n";
        let marked = backfill_reminders(raw);

        let id = find_reminders(raw, london())[0].item.id;
        let expected = raw.replace(
            "- [ ] Pay tax — due 2026-04-10",
            &format!("- [ ] Pay tax — due 2026-04-10 {}", marker::render(id)),
        );
        assert_eq!(marked, expected, "only the reminder line may change");
    }

    #[test]
    fn front_matter_offset_points_at_body_and_keeps_crlf() {
        let raw = "---\r\ntitle: Reminders\r\n---\r\n- [ ] Do thing\r\n";
        let (meta, off) = front_matter(raw);
        assert_eq!(
            meta.as_ref().unwrap()["title"],
            serde_yaml::Value::String("Reminders".into())
        );
        // The body is a slice of the original bytes, so CRLF survives the round trip.
        assert_eq!(&raw[off..], "- [ ] Do thing\r\n");
    }

    #[test]
    fn front_matter_absent_leaves_whole_file_as_body() {
        let raw = "# Reminders\n\n- [ ] Do thing\n";
        let (meta, off) = front_matter(raw);
        assert!(meta.is_none());
        assert_eq!(off, 0);
        assert_eq!(&raw[off..], raw);
    }

    #[test]
    fn front_matter_bom_is_skipped_but_body_offset_stays_valid() {
        let raw = "\u{feff}---\ntitle: x\n---\nbody\n";
        let (meta, off) = front_matter(raw);
        assert!(meta.is_some());
        assert_eq!(&raw[off..], "body\n");
    }

    #[test]
    fn front_matter_unclosed_fence_is_all_body() {
        let raw = "---\ntitle: x\nno closing fence\n";
        let (meta, off) = front_matter(raw);
        assert!(meta.is_none());
        assert_eq!(&raw[off..], raw);
    }

    #[test]
    fn front_matter_dots_close_the_block() {
        let raw = "---\ntitle: x\n...\nbody\n";
        let (meta, off) = front_matter(raw);
        assert!(meta.is_some());
        assert_eq!(&raw[off..], "body\n");
    }

    #[test]
    fn front_matter_invalid_yaml_still_strips_the_block() {
        let raw = "---\n: : not yaml : :\n---\nbody\n";
        let (meta, off) = front_matter(raw);
        assert!(meta.is_none(), "invalid YAML parses to None");
        assert_eq!(&raw[off..], "body\n", "but the block is still stripped");
    }

    #[test]
    fn reminders_skip_yaml_front_matter() {
        let md = "---\ntitle: Reminders\nfoo: bar\n---\n\n- [ ] Do thing — due 2026-06-01\n";
        let list = parse_reminders(md, london());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].body, "Do thing");
        let (meta, _) = strip_yaml_front_matter(md);
        assert!(meta.is_some());
        assert_eq!(meta.as_ref().unwrap()["title"], serde_yaml::Value::String("Reminders".into()));
    }

    #[test]
    fn reminders_tasklist_matches_line_parser_case() {
        let md = "# Reminders\n\n- [ ] Pay tax — due 2026-04-10 — recurs yearly\n";
        let list = parse_reminders(md, london());
        assert_eq!(list.len(), 1);
        assert!(!list[0].done);
        assert_eq!(list[0].body, "Pay tax");
        assert!(matches!(
            list[0].recurrence,
            Some(Recurrence::Custom(ref s)) if s == "yearly"
        ));
    }

    #[test]
    fn events_heading_with_inline_formatting() {
        let md = "## 2026-04-05 — **Gig** at Tap\nDoors 7pm\n";
        let evs = parse_events(md, london());
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].title, "Gig at Tap");
        assert_eq!(evs[0].description.as_deref(), Some("Doors 7pm"));
    }

    #[test]
    fn worklog_list_and_tags() {
        let md = "## 2026-03-30\n- Line one\n- Line two\nTags: a, b\n";
        let w = parse_worklog(md, london());
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(w[1].tags, vec!["a".to_string(), "b".to_string()]);
    }
}

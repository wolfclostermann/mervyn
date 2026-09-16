//! Vault Markdown parsing via pulldown-cmark (task lists + ATX headings).
//! Leading YAML front matter (Obsidian-style `---` … `---`) is stripped and parsed with [`serde_yaml`];
//! the Markdown body is what pulldown sees. Parsed front matter is reserved for future use.

use std::borrow::Cow;
use std::hash::Hasher;

use chrono::{NaiveDate, Utc};
use fnv::FnvHasher;
use pulldown_cmark::{Event as MdEvent, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_yaml::Value as YamlValue;

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
pub fn strip_yaml_front_matter(raw: &str) -> (Option<YamlValue>, Cow<'_, str>) {
    let (meta, offset) = front_matter(raw);
    (meta, Cow::Borrowed(&raw[offset..]))
}

fn markdown_body(raw: &str) -> Cow<'_, str> {
    let (_meta, body) = strip_yaml_front_matter(raw);
    body
}

/// FNV-1a 64-bit — stable across Rust versions for vault-derived primary keys.
pub(super) fn stable_vault_row_id(prefix: &[u8], key: &str) -> u64 {
    let mut h = FnvHasher::default();
    h.write(prefix);
    h.write(key.as_bytes());
    h.finish()
}

fn date_at_noon_utc(date: NaiveDate) -> chrono::DateTime<Utc> {
    date.and_hms_opt(12, 0, 0)
        .expect("valid noon")
        .and_utc()
}

fn parse_reminder_body(s: &str) -> (String, Option<NaiveDate>, Option<Recurrence>) {
    let mut recurrence = None;
    let mut due = None;
    let mut body_part = s.to_string();

    if let Some(idx) = body_part.find(" — recurs ") {
        let tail = body_part[idx + " — recurs ".len()..].trim();
        recurrence = Some(Recurrence::Custom(tail.to_string()));
        body_part.truncate(idx);
    }

    if let Some(idx) = body_part.find(" — due ") {
        let date_str = body_part[idx + " — due ".len()..].trim();
        if let Ok(d) = NaiveDate::parse_from_str(
            date_str.split_whitespace().next().unwrap_or(date_str),
            "%Y-%m-%d",
        ) {
            due = Some(d);
        }
        body_part.truncate(idx);
    }

    let body = body_part.trim().to_string();
    (body, due, recurrence)
}

/// Walk events until `end` is true; does not consume the matching event. Always advances on other events.
fn collect_plain_until(events: &[MdEvent<'_>], i: &mut usize, mut end: impl FnMut(&MdEvent<'_>) -> bool) -> String {
    let mut s = String::new();
    while *i < events.len() && !end(&events[*i]) {
        match &events[*i] {
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

fn consume_if(events: &[MdEvent<'_>], i: &mut usize, pred: impl FnOnce(&MdEvent<'_>) -> bool) {
    if *i < events.len() && pred(&events[*i]) {
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

/// GitHub-style task list items → [`Reminder`].
pub fn parse_reminders(text: &str) -> Vec<Reminder> {
    let body = markdown_body(text);
    let events: Vec<MdEvent<'_>> =
        Parser::new_ext(body.as_ref(), Options::ENABLE_TASKLISTS).collect();
    let mut i = 0;
    let mut out = Vec::new();

    while i < events.len() {
        if matches!(events[i], MdEvent::Start(Tag::Item)) {
            i += 1;
            let done = if i < events.len() {
                if let MdEvent::TaskListMarker(checked) = events[i] {
                    i += 1;
                    Some(checked)
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(done) = done {
                let raw = collect_plain_until(&events, &mut i, |e| matches!(e, MdEvent::End(TagEnd::Item)));
                consume_if(&events, &mut i, |e| matches!(e, MdEvent::End(TagEnd::Item)));
                let (body, due, recurrence) = parse_reminder_body(raw.trim());
                let due = match due {
                    Some(d) => date_at_noon_utc(d),
                    None => Utc::now(),
                };
                let norm = format!("{body}|{due}|{done}");
                let id = stable_vault_row_id(b"rem:", &norm);
                out.push(Reminder {
                    id,
                    body,
                    due,
                    recurrence,
                    done,
                });
            } else {
                while i < events.len() && !matches!(events[i], MdEvent::End(TagEnd::Item)) {
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

fn take_paragraph(events: &[MdEvent<'_>], i: &mut usize) -> Option<String> {
    if !matches!(events.get(*i), Some(MdEvent::Start(Tag::Paragraph))) {
        return None;
    }
    *i += 1;
    let mut s = String::new();
    while *i < events.len() {
        match &events[*i] {
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

/// `## YYYY-MM-DD — title` sections; body paragraphs + optional `Tags:` line.
pub fn parse_events(text: &str) -> Vec<Event> {
    let body = markdown_body(text);
    let events: Vec<MdEvent<'_>> = Parser::new_ext(body.as_ref(), Options::empty()).collect();
    let mut i = 0;
    let mut out = Vec::new();

    while i < events.len() {
        if is_h2_start(&events[i]) {
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
            let Ok(date) = NaiveDate::parse_from_str(date_str.trim(), "%Y-%m-%d") else {
                continue;
            };
            let start = date_at_noon_utc(date);
            let mut desc_lines: Vec<String> = Vec::new();
            let mut tags: Vec<String> = Vec::new();

            while i < events.len() && !is_h2_start(&events[i]) {
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
            let key = format!("{title}|{start}");
            let id = stable_vault_row_id(b"evt:", &key);
            out.push(Event {
                id,
                title: title.trim().to_string(),
                description,
                start,
                end: None,
                tags,
            });
        } else {
            i += 1;
        }
    }

    out
}

fn collect_list_item_text(events: &[MdEvent<'_>], i: &mut usize) -> String {
    if !matches!(events.get(*i), Some(MdEvent::Start(Tag::Item))) {
        return String::new();
    }
    *i += 1;
    let s = collect_plain_until(events, i, |e| matches!(e, MdEvent::End(TagEnd::Item)));
    consume_if(events, i, |e| matches!(e, MdEvent::End(TagEnd::Item)));
    s.trim().to_string()
}

/// `## YYYY-MM-DD` sections; list items as worklog bullets; optional `Tags:` paragraph.
pub fn parse_worklog(text: &str) -> Vec<WorklogEntry> {
    let body = markdown_body(text);
    let events: Vec<MdEvent<'_>> = Parser::new_ext(body.as_ref(), Options::empty()).collect();
    let mut i = 0;
    let mut out = Vec::new();

    while i < events.len() {
        if is_h2_start(&events[i]) {
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
            let day_start = date_at_noon_utc(day);
            let mut bullets: Vec<String> = Vec::new();
            let mut tags: Vec<String> = Vec::new();

            while i < events.len() && !is_h2_start(&events[i]) {
                match &events[i] {
                    MdEvent::Start(Tag::List(_)) => {
                        i += 1;
                        while i < events.len() && !matches!(events[i], MdEvent::End(TagEnd::List(_))) {
                            if matches!(events[i], MdEvent::Start(Tag::Item)) {
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
        let list = parse_reminders(md);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].body, "Do thing");
        let (meta, _) = strip_yaml_front_matter(md);
        assert!(meta.is_some());
        assert_eq!(meta.as_ref().unwrap()["title"], serde_yaml::Value::String("Reminders".into()));
    }

    #[test]
    fn reminders_tasklist_matches_line_parser_case() {
        let md = "# Reminders\n\n- [ ] Pay tax — due 2026-04-10 — recurs yearly\n";
        let list = parse_reminders(md);
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
        let evs = parse_events(md);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].title, "Gig at Tap");
        assert_eq!(evs[0].description.as_deref(), Some("Doors 7pm"));
    }

    #[test]
    fn worklog_list_and_tags() {
        let md = "## 2026-03-30\n- Line one\n- Line two\nTags: a, b\n";
        let w = parse_worklog(md);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(w[1].tags, vec!["a".to_string(), "b".to_string()]);
    }
}

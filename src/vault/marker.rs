//! The id marker: `<!--mv:1a2b-->`, an HTML comment carrying a row's primary key.
//!
//! Identity has to be *in the file*. Content hashing — what `md::stable_vault_row_id` does —
//! cannot tell an edit from a new item, so ticking a checkbox in Obsidian currently mints a new
//! id and orphans the old row. A marker fixes that: the line keeps its id however it is edited.
//!
//! An HTML comment was chosen because Obsidian hides it in reading view *and* because the
//! existing parsers already ignore it — `md::collect_plain_until` keeps only text, code and
//! breaks — so markers can be back-filled into the vault before anything else changes.

use std::ops::Range;

const PREFIX: &str = "<!--mv:";
const SUFFIX: &str = "-->";

/// A marker found in some text, and where it sits within it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: u64,
    pub span: Range<usize>,
}

/// Render the marker for `id`. Lower-case hex, unpadded — short enough to stay out of the way on
/// a line the user is reading, wide enough for the FNV ids that vault rows are bootstrapped with.
pub fn render(id: u64) -> String {
    format!("{PREFIX}{id:x}{SUFFIX}")
}

/// The first marker in `s`, if any. Anything between the delimiters that is not a hex `u64` is
/// not a marker — an ordinary HTML comment someone typed is left alone rather than misread.
pub fn find(s: &str) -> Option<Found> {
    let mut from = 0usize;
    while let Some(rel) = s[from..].find(PREFIX) {
        let start = from + rel;
        let body_start = start + PREFIX.len();
        let body_end = body_start + s[body_start..].find(SUFFIX)?;
        if let Ok(id) = u64::from_str_radix(&s[body_start..body_end], 16) {
            return Some(Found {
                id,
                span: start..body_end + SUFFIX.len(),
            });
        }
        from = body_end + SUFFIX.len();
    }
    None
}

/// `s` without its marker, and without the whitespace that separated it. For comparing the text
/// a human typed against the text Mervyn would render.
pub fn strip(s: &str) -> String {
    match find(s) {
        Some(f) => {
            let mut out = String::with_capacity(s.len());
            out.push_str(s[..f.span.start].trim_end());
            out.push_str(&s[f.span.end..]);
            out
        }
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_and_find_round_trip() {
        for id in [0u64, 1, 0xdead_beef, u64::MAX] {
            let line = format!("- [ ] Pay tax {}", render(id));
            assert_eq!(find(&line).unwrap().id, id, "id {id} did not survive");
        }
    }

    #[test]
    fn find_reports_the_span_it_matched() {
        let line = "- [ ] Pay tax <!--mv:1a-->";
        let f = find(line).unwrap();
        assert_eq!(&line[f.span.clone()], "<!--mv:1a-->");
        assert_eq!(f.id, 26);
    }

    #[test]
    fn an_ordinary_comment_is_not_a_marker() {
        assert!(find("- [ ] Pay tax <!-- remember to check the rate -->").is_none());
    }

    #[test]
    fn a_non_hex_payload_is_skipped_and_a_later_real_marker_still_found() {
        let line = "<!--mv:not-hex--> tail <!--mv:ff-->";
        assert_eq!(find(line).unwrap().id, 255);
    }

    #[test]
    fn an_unterminated_marker_is_not_a_marker() {
        assert!(find("- [ ] Pay tax <!--mv:1a").is_none());
    }

    #[test]
    fn strip_removes_the_marker_and_its_separating_space() {
        assert_eq!(strip("- [ ] Pay tax <!--mv:1a-->"), "- [ ] Pay tax");
        assert_eq!(strip("- [ ] Pay tax"), "- [ ] Pay tax");
    }

    #[test]
    fn strip_keeps_text_that_follows_the_marker() {
        assert_eq!(strip("a <!--mv:1--> b"), "a b");
    }
}

//! Normalisation for inbound chat text before it reaches an intent parser.
//!
//! The Slack version of this module also stripped `<@USER>` / `<#CHANNEL|label>` markup.
//! Telegram has no such markup, and that stripper removed *every* `<...>` span - including
//! ordinary angle-bracketed user text - so it was dropped rather than ported.

pub fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

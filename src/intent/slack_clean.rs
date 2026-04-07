//! Normalise Slack message text for storage (mentions, spacing).

/// Remove `<@USER>`, `<!subteam^…>`, `<#CHANNEL|label>`-style segments.
pub fn strip_slack_mentions(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        rest = &rest[start + 1..];
        if let Some(end) = rest.find('>') {
            rest = &rest[end + 1..];
        } else {
            out.push('<');
            out.push_str(rest);
            break;
        }
    }
    out.push_str(rest);
    out
}

pub fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

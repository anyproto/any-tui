//! The emoji picker's table and search. Every emoji with its name and its
//! GitHub shortcodes (the `emojis` crate), ranked by the same fuzzy matcher
//! as the chat picker, plus the `:token` detection the composer uses to
//! open the picker inline.

use crate::fuzzy::fuzzy_match;
use std::sync::OnceLock;

/// One emoji as the picker shows it: the glyph, its CLDR name and its
/// primary `:shortcode:` (empty when it has none).
pub struct Entry {
    pub glyph: &'static str,
    pub name: &'static str,
    pub code: &'static str,
    codes: Vec<&'static str>,
    /// What the query is matched against: every shortcode, then the name.
    hay: String,
}

/// Every emoji, base forms only (no skin-tone variants), in Unicode order —
/// smileys first, flags last — so an empty query lists them sensibly.
pub fn table() -> &'static [Entry] {
    static TABLE: OnceLock<Vec<Entry>> = OnceLock::new();
    TABLE.get_or_init(|| {
        emojis::iter()
            .map(|e| {
                let codes: Vec<&str> = e.shortcodes().collect();
                Entry {
                    glyph: e.as_str(),
                    name: e.name(),
                    code: codes.first().copied().unwrap_or(""),
                    hay: format!("{} {}", codes.join(" "), e.name()),
                    codes,
                }
            })
            .collect()
    })
}

/// Matches for `query`, best first. A shortcode that starts with the query
/// outranks a fuzzy hit (`thumbs` → 👍 before anything with those letters
/// scattered through its name); ties keep table order. An empty query lists
/// everything.
pub fn search(query: &str) -> Vec<&'static Entry> {
    let q = query.trim().to_lowercase();
    let mut scored: Vec<(i32, &'static Entry)> = table()
        .iter()
        .filter_map(|e| {
            let (score, _) = fuzzy_match(&e.hay, &q)?;
            let boost = if q.is_empty() {
                0
            } else if e.hay.split(' ').any(|w| w == q) {
                200 // exact shortcode or name word
            } else if e.hay.split(' ').any(|w| w.starts_with(&q)) {
                100
            } else if e.hay.contains(&q) {
                50
            } else {
                0
            };
            Some((score + boost, e))
        })
        .collect();
    if !q.is_empty() {
        scored.sort_by(|a, b| b.0.cmp(&a.0));
    }
    scored.into_iter().map(|(_, e)| e).collect()
}

/// The emoji whose shortcode is exactly `code` (`smile`, `+1`), if any.
pub fn by_code(code: &str) -> Option<&'static Entry> {
    let c = code.to_lowercase();
    table().iter().find(|e| e.codes.iter().any(|k| *k == c))
}

/// The `:query` token the cursor is at the end of, if any: a colon that opens
/// a word (text start or after whitespace) followed by non-space characters up
/// to the cursor. Returns the colon's char index and the text after it.
/// `cursor` is a char index, as `tui_input` reports it.
pub fn token_at(text: &str, cursor: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let mut start = cursor;
    while start > 0 && !chars[start - 1].is_whitespace() {
        start -= 1;
    }
    if start == cursor || chars[start] != ':' {
        return None;
    }
    Some((start, chars[start + 1..cursor].iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcode_prefix_wins() {
        assert_eq!(search("thumbs")[0].glyph, "👍");
        assert_eq!(search("+1")[0].glyph, "👍");
        assert_eq!(search("heart")[0].code, "heart");
        assert_eq!(search("smile")[0].code, "smile");
        assert_eq!(search("tada")[0].glyph, "🎉");
    }

    #[test]
    fn fuzzy_still_reaches() {
        // No shortcode starts with this, but the letters are in "grinning".
        assert!(search("grnng").iter().any(|e| e.code == "grinning"));
        assert!(search("zzqqxx").is_empty());
        assert!(search("").len() > 1000);
    }

    #[test]
    fn exact_code() {
        assert_eq!(by_code("smile").map(|e| e.glyph), Some("😄"));
        assert_eq!(by_code("+1").map(|e| e.glyph), Some("👍"));
        assert!(by_code("not-an-emoji").is_none());
    }

    #[test]
    fn colon_token_opens_a_word() {
        assert_eq!(token_at(":", 1), Some((0, String::new())));
        assert_eq!(token_at(":smi", 4), Some((0, "smi".into())));
        assert_eq!(token_at("hi :smi", 7), Some((3, "smi".into())));
        assert_eq!(token_at("hi\n:smi", 7), Some((3, "smi".into())));
        // Mid-word colons are not a picker: times, "note:".
        assert_eq!(token_at("at 10:30", 8), None);
        assert_eq!(token_at("note:", 5), None);
        // The cursor has to be inside the token.
        assert_eq!(token_at(":smi ", 5), None);
        assert_eq!(token_at(":smi", 0), None);
        assert_eq!(token_at("héllo :ок", 9), Some((6, "ок".into())));
    }
}

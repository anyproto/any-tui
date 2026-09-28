//! IRC-style composer commands. `parse` turns what was typed into a
//! [`Command`]; it is pure (no roster, no network) so the rules are
//! unit-testable, and `App::send_input` carries each command out.
//!
//! A leading `/word` is a command; an unknown one is refused so a typo never
//! goes out as a message, and `//` sends a literal leading slash. A slash not
//! followed by a bare word (`/usr/bin`, `/ 2`) is ordinary text. `s/a/b/` on
//! its own edits your last message, as in most IRC clients.

use crate::model::action_body;

/// The ASCII faces, markdown-escaped: other clients (any-ui) render message
/// text as markdown, where a bare `\_` would eat the shrug's arm. We unescape
/// on display (`model::md_unescape`), so both sides show the same face.
const SHRUG: &str = r"¯\\\_(ツ)\_/¯";
const TABLEFLIP: &str = "(╯°□°)╯︵ ┻━┻";
const UNFLIP: &str = r"┬─┬ノ( º \_ ºノ)";

/// The idle marker `/away` uses when given no emoji of its own.
pub const DEFAULT_AWAY: &str = "💤";

pub const HELP: &str = "/me /shrug /tableflip /unflip · /dm /msg /accept · /join /hl /unhl /away /back /compact · s/old/new/ · // literal";

#[derive(Debug, PartialEq)]
pub enum Command {
    /// Text to post to the open chat (`/me`, `/shrug`, … already expanded).
    Send(String),
    /// `s/from/to/[g]`: rewrite your newest message in the open chat.
    Edit { from: String, to: String, all: bool },
    /// `/away [emoji]` — shown after your name.
    Away(String),
    Back,
    /// `/hl` lists, `/hl word` adds.
    Highlight(Option<String>),
    Unhighlight(String),
    /// `/join <query>`: open the best fuzzy match.
    Join(String),
    Compact,
    /// `/dm [@name | identity]` — a person, or (empty) the author under the
    /// cursor. Unresolved: the roster lives in the app.
    Dm(String),
    /// `/msg @name text`: send into the DM without switching to it. The
    /// name/text split needs the roster too, so this is the raw rest.
    Msg(String),
    /// `/accept [name]`: approve an incoming DM request.
    Accept(String),
    Help,
}

/// `Err` carries the toast to show; the input is kept for fixing.
pub fn parse(input: &str) -> Result<Command, String> {
    if let Some(edit) = parse_subst(input) {
        return Ok(edit);
    }
    if let Some(rest) = input.strip_prefix("//") {
        return Ok(Command::Send(format!("/{rest}")));
    }
    let Some(rest) = input.strip_prefix('/') else {
        return Ok(Command::Send(input.to_string()));
    };
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let word = &rest[..end];
    if word.is_empty() || !word.chars().all(|c| c.is_ascii_alphabetic()) {
        return Ok(Command::Send(input.to_string()));
    }
    let arg = rest[end..].trim();
    // Appends a face to optional leading text, `/shrug fine` → `fine ¯\_(ツ)_/¯`.
    let face = |f: &str| Command::Send(if arg.is_empty() { f.to_string() } else { format!("{arg} {f}") });
    let need = |what: &str| format!("usage: /{word} {what}");
    Ok(match word {
        // The wire form of an action is the literal `/me …` text.
        "me" if action_body(input).is_some() => Command::Send(input.to_string()),
        "me" => return Err(need("<action>")),
        "shrug" => face(SHRUG),
        "tableflip" => face(TABLEFLIP),
        "unflip" => face(UNFLIP),
        "away" => Command::Away(if arg.is_empty() { DEFAULT_AWAY } else { arg }.to_string()),
        "back" => Command::Back,
        "hl" | "highlight" => Command::Highlight((!arg.is_empty()).then(|| arg.to_string())),
        "unhl" if arg.is_empty() => return Err(need("<word>")),
        "unhl" => Command::Unhighlight(arg.to_string()),
        "join" | "j" if arg.is_empty() => return Err(need("<chat>")),
        "join" | "j" => Command::Join(arg.to_string()),
        "compact" => Command::Compact,
        "dm" | "query" => Command::Dm(arg.to_string()),
        "msg" if arg.is_empty() => return Err(need("@name <text>")),
        "msg" => Command::Msg(arg.to_string()),
        "accept" => Command::Accept(arg.to_string()),
        "help" => Command::Help,
        _ => return Err(format!("unknown command /{word} — /help lists them, // sends a literal slash")),
    })
}

/// `s/from/to/` with an optional trailing `/` and `g`; `\/` escapes the
/// delimiter. Literal text, not a regex. `None` when it isn't one.
fn parse_subst(input: &str) -> Option<Command> {
    let rest = input.strip_prefix("s/")?;
    let mut parts = vec![String::new()];
    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'/') => {
                parts.last_mut()?.push('/');
                chars.next();
            }
            '/' => parts.push(String::new()),
            c => parts.last_mut()?.push(c),
        }
    }
    let all = match parts.as_slice() {
        [_, _] => false,
        [_, _, flags] if flags.is_empty() => false,
        [_, _, flags] if flags == "g" => true,
        _ => return None,
    };
    if parts[0].is_empty() {
        return None;
    }
    Some(Command::Edit {
        from: parts[0].clone(),
        to: parts[1].clone(),
        all,
    })
}

/// True for something shaped like an account identity (base58, ~48 chars),
/// as opposed to a display name.
pub fn looks_like_identity(s: &str) -> bool {
    s.len() >= 40 && s.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Splits `rest` into (identity, remaining text) against `people` —
/// (name, identity) pairs. Takes a leading identity, or the longest name
/// (case-insensitive, `@` optional) that ends at a word boundary.
pub fn resolve_person<'a>(rest: &'a str, people: &[(String, String)]) -> Option<(String, &'a str)> {
    let rest = rest.trim_start();
    let bare = rest.strip_prefix('@').unwrap_or(rest);
    let first = bare.split_whitespace().next().unwrap_or("");
    if looks_like_identity(first) {
        return Some((first.to_string(), bare[first.len()..].trim_start()));
    }
    let lower = bare.to_lowercase();
    people
        .iter()
        .filter(|(n, _)| !n.is_empty())
        .filter(|(n, _)| {
            let n = n.to_lowercase();
            lower.starts_with(&n)
                && lower[n.len()..].chars().next().is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
        })
        .max_by_key(|(n, _)| n.len())
        .map(|(n, id)| {
            // Cut by chars: lowercasing can change a name's byte length.
            let cut = bare.char_indices().nth(n.chars().count()).map_or(bare.len(), |(i, _)| i);
            (id.clone(), bare[cut..].trim_start())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send(s: &str) -> Command {
        Command::Send(s.to_string())
    }

    #[test]
    fn plain_text_and_escapes() {
        assert_eq!(parse("hello").unwrap(), send("hello"));
        assert_eq!(parse("//join #x").unwrap(), send("/join #x"));
        assert_eq!(parse("/usr/bin is a path").unwrap(), send("/usr/bin is a path"));
        assert_eq!(parse("/ 2").unwrap(), send("/ 2"));
        assert_eq!(parse("/").unwrap(), send("/"));
    }

    #[test]
    fn me_and_faces() {
        assert_eq!(parse("/me waves").unwrap(), send("/me waves"));
        assert!(parse("/me").is_err());
        assert_eq!(parse("/shrug").unwrap(), send(SHRUG));
        assert_eq!(parse("/shrug fine").unwrap(), send(&format!("fine {SHRUG}")));
        assert_eq!(parse("/tableflip").unwrap(), send(TABLEFLIP));
    }

    #[test]
    fn unknown_commands_are_refused() {
        assert!(parse("/joinn x").is_err());
        assert!(parse("/shrugg").is_err());
    }

    #[test]
    fn state_commands() {
        assert_eq!(parse("/away").unwrap(), Command::Away(DEFAULT_AWAY.into()));
        assert_eq!(parse("/away 🍕").unwrap(), Command::Away("🍕".into()));
        assert_eq!(parse("/back").unwrap(), Command::Back);
        assert_eq!(parse("/hl").unwrap(), Command::Highlight(None));
        assert_eq!(parse("/hl rust").unwrap(), Command::Highlight(Some("rust".into())));
        assert_eq!(parse("/j sync").unwrap(), Command::Join("sync".into()));
        assert!(parse("/join").is_err());
        assert_eq!(parse("/dm").unwrap(), Command::Dm(String::new()));
        assert_eq!(parse("/query @Anna").unwrap(), Command::Dm("@Anna".into()));
        assert!(parse("/msg").is_err());
    }

    #[test]
    fn substitution() {
        let e = |f: &str, t: &str, all| Command::Edit { from: f.into(), to: t.into(), all };
        assert_eq!(parse("s/teh/the/").unwrap(), e("teh", "the", false));
        assert_eq!(parse("s/teh/the").unwrap(), e("teh", "the", false));
        assert_eq!(parse("s/a/b/g").unwrap(), e("a", "b", true));
        assert_eq!(parse(r"s/a\/b/c/").unwrap(), e("a/b", "c", false));
        assert_eq!(parse("s/a//").unwrap(), e("a", "", false));
        // Not substitutions: plain text goes out as-is.
        assert_eq!(parse("s/a/b/x").unwrap(), send("s/a/b/x"));
        assert_eq!(parse("s//b/").unwrap(), send("s//b/"));
        assert_eq!(parse("s/nothing").unwrap(), send("s/nothing"));
    }

    #[test]
    fn resolves_people() {
        let people = vec![
            ("Anna".to_string(), "idA".to_string()),
            ("Anna Lee".to_string(), "idAL".to_string()),
        ];
        assert_eq!(resolve_person("@anna lee hi", &people), Some(("idAL".into(), "hi")));
        assert_eq!(resolve_person("Anna hi there", &people), Some(("idA".into(), "hi there")));
        assert_eq!(resolve_person("@annabelle", &people), None);
        let id = "A9fBcRQuxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        assert_eq!(resolve_person(&format!("{id} yo"), &people), Some((id.into(), "yo")));
    }
}

use serde::Deserialize;
use serde_json::Value;
use std::cmp::Ordering;

#[derive(Debug, Clone, Deserialize)]
pub struct Space {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Identity {
    pub identity: String,
    #[serde(default)]
    pub name: String,
    /// Spaces this identity is a member of, when the directory knows. Used to
    /// scope `@` completion to the current space; empty means "unknown".
    #[serde(default, rename = "spaceIds")]
    pub space_ids: Vec<String>,
}

/// Reads a server timestamp as unix seconds. The wire shape is an instant,
/// `{"$date": "<RFC 3339>"}`; peers on older builds still materialize the same
/// field as a bare unix-seconds number, and rows written before an upgrade read
/// that way until the SDK re-indexes them, so both forms must parse.
pub fn parse_instant(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    let s = v.get("$date").and_then(|d| d.as_str()).or_else(|| v.as_str())?;
    let dt = chrono::DateTime::parse_from_rfc3339(s).ok()?;
    Some(dt.timestamp_millis() as f64 / 1000.0)
}

/// The body of an IRC-style action (`/me waves` → `waves`), or `None` when
/// `text` isn't one. The wire form is the literal `/me …` text: the server has
/// no action flag, and a client that doesn't know the convention still shows
/// something readable.
pub fn action_body(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("/me")?;
    let body = rest.strip_prefix([' ', '\t'])?.trim_start();
    (!body.is_empty()).then_some(body)
}

/// Drops CommonMark backslash escapes (`\_` → `_`) outside code spans, so
/// text written for markdown renderers (any-ui) reads the same here.
pub fn md_unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' => {
                in_code = !in_code;
                out.push(c);
            }
            '\\' if !in_code && chars.peek().is_some_and(|n| n.is_ascii_punctuation()) => {
                out.push(chars.next().unwrap());
            }
            c => out.push(c),
        }
    }
    out
}

/// The highlight words (`/hl`) that occur in `text` as whole words, case-
/// insensitively, each as the exact substring found — so a renderer can style
/// it where it stands.
pub fn highlight_hits(text: &str, words: &[String]) -> Vec<String> {
    let lower = text.to_lowercase();
    // Lowercasing must not shift byte offsets for the slice back into `text`.
    if lower.len() != text.len() {
        return Vec::new();
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut hits = Vec::new();
    for w in words.iter().filter(|w| !w.is_empty()) {
        let w = w.to_lowercase();
        let mut from = 0;
        while let Some(i) = lower[from..].find(&w).map(|i| i + from) {
            let end = i + w.len();
            let before = lower[..i].chars().next_back().is_none_or(|c| !is_word(c));
            let after = lower[end..].chars().next().is_none_or(|c| !is_word(c));
            if before && after {
                hits.push(text[i..end].to_string());
            }
            from = end;
        }
    }
    hits
}

/// Prefix of a mention link destination (docs/19-links.md § `m`):
/// `any://m/<spaceId>/<identity>`.
const MENTION_URI_PREFIX: &str = "any://m/";

/// Builds the markdown a mention is written as: the link text is the display
/// name at time of writing, the destination the canonical mention URI.
pub fn mention_markdown(name: &str, space_id: &str, identity: &str) -> String {
    format!("[{name}]({MENTION_URI_PREFIX}{space_id}/{identity})")
}

/// Rewrites every `[Name](any://m/<space>/<identity>)` link in `text` to an
/// `@Name` chip, resolving the current display name through `name_of` and
/// falling back to the link text (the name snapshot taken when it was written).
/// Non-mention links are left untouched. Returns the rewritten text plus the
/// chip labels it produced, so a renderer can highlight them.
pub fn render_mentions(text: &str, name_of: impl Fn(&str) -> Option<String>) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut chips = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        // Try to parse `[label](dest)` starting at `open`.
        let after = &rest[open..];
        let parsed = after.find("](").and_then(|close| {
            let label = &after[1..close];
            let dest_start = close + 2;
            let dest_end = after[dest_start..].find(')')? + dest_start;
            let dest = &after[dest_start..dest_end];
            let target = dest.strip_prefix(MENTION_URI_PREFIX)?;
            // `<spaceId>/<identity>`; the identity is the last segment.
            let identity = target.rsplit('/').next().filter(|s| !s.is_empty())?;
            Some((label, identity, dest_end + 1))
        });
        match parsed {
            Some((label, identity, consumed)) if !label.contains('\n') => {
                let name = name_of(identity)
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| label.to_string());
                out.push_str(&rest[..open]);
                out.push('@');
                out.push_str(&name);
                chips.push(format!("@{name}"));
                rest = &rest[open + consumed..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    (out, chips)
}

/// Turns typed `@Name` tokens into mention links for every (name, identity)
/// pair in `roster`, longest name first so "Anna Lee" wins over "Anna". A token
/// must end at a word boundary — `@anna` never rewrites inside `@annabelle`.
/// Names are matched case-insensitively; the link text keeps the roster's form.
pub fn link_mentions(text: &str, space_id: &str, roster: &[(String, String)]) -> String {
    let mut names: Vec<&(String, String)> = roster.iter().filter(|(n, _)| !n.is_empty()).collect();
    names.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()).then(a.0.cmp(&b.0)));

    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let bytes = text.as_bytes();
    'outer: while i < text.len() {
        if bytes[i] == b'@' && (i == 0 || !is_word_char(text[..i].chars().next_back())) {
            let tail = &text[i + 1..];
            for (name, identity) in names.iter() {
                let Some(candidate) = tail.get(..name.len()) else { continue };
                if !candidate.eq_ignore_ascii_case(name) {
                    continue;
                }
                let next = tail[name.len()..].chars().next();
                if is_word_char(next) {
                    continue;
                }
                out.push_str(&mention_markdown(name, space_id, identity));
                i += 1 + name.len();
                continue 'outer;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_word_char(c: Option<char>) -> bool {
    matches!(c, Some(c) if c.is_alphanumeric() || c == '_')
}

/// A chat object inside a space. `unread` drives the sidebar badges and the
/// status bar, and is kept live by the per-space objects subscription.
#[derive(Debug, Clone)]
pub struct Chat {
    pub space_id: String,
    pub space_name: String,
    pub object_id: String,
    pub name: String,
    pub unread: u64,
    /// Unread messages that mention you (`chat.unreadMentions`) — a subset of
    /// `unread`, badged separately because a ping outranks ordinary traffic.
    pub unread_mentions: u64,
    pub unread_reactions: u64,
    pub pos: String,
    /// Preview of the newest message. Spaces routinely hold several chats all
    /// named "general", so the preview is what actually tells them apart.
    pub last_text: Option<String>,
    pub last_creator: String,
    /// Agent name when the newest message was agent-authored — the preview
    /// shows this instead of the human account that signed it.
    pub last_agent: Option<String>,
    pub last_at: f64,
    /// A `/hl` word showed up in an unread message here (client-side, from
    /// the preview stream; cleared once the chat is read).
    pub hl: bool,
}

impl Chat {
    /// A record is a chat when its `id` is a chat-declaring type (`chat_types`,
    /// the `chat_messages` owners — the general-chat root is a bundle root
    /// that is its own type and hosts itself), or when it is a type whose
    /// layout is `chat` (matches even before its owner id has been resolved).
    /// The `chat` sub-document holds the unread counters. Returns None for
    /// non-chats.
    pub fn from_record(
        v: &Value,
        space_id: &str,
        space_name: &str,
        chat_types: &[String],
    ) -> Option<Chat> {
        let id = v.get("id")?.as_str()?.to_string();
        if !Self::is_chat_record(v, chat_types) {
            return None;
        }
        let any = v.get("any");
        let name = any
            .and_then(|a| a.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        // Sidebar order: `miniapp.pos` (absent until the user reorders).
        let pos = v
            .get("miniapp")
            .and_then(|n| n.get("pos"))
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        let chat = v.get("chat");
        Some(Chat {
            space_id: space_id.to_string(),
            space_name: space_name.to_string(),
            object_id: id,
            name,
            unread: chat
                .and_then(|c| c.get("unreadCount"))
                .and_then(|c| c.as_u64())
                .unwrap_or(0),
            unread_mentions: chat
                .and_then(|c| c.get("unreadMentions"))
                .and_then(|c| c.as_u64())
                .unwrap_or(0),
            unread_reactions: chat
                .and_then(|c| c.get("unreadReactionsCount"))
                .and_then(|c| c.as_u64())
                .unwrap_or(0),
            pos,
            last_text: None,
            last_creator: String::new(),
            last_agent: None,
            last_at: 0.0,
            hl: false,
        })
    }

    fn is_chat_record(v: &Value, chat_types: &[String]) -> bool {
        let is_owner = v
            .get("id")
            .and_then(Value::as_str)
            .map(|id| chat_types.iter().any(|c| c == id))
            .unwrap_or(false);
        is_owner
            || v.get("type")
                .and_then(|t| t.get("layout"))
                .and_then(|l| l.get("type"))
                .and_then(|t| t.as_str())
                == Some("chat")
    }

    /// Label for the sidebar. The general chat is named ("General"); an
    /// unnamed chat object (pre-parts daemons' space-level chat) shows as
    /// `space chat`.
    pub fn label(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else {
            "space chat"
        }
    }

    pub fn qualified(&self) -> String {
        format!("{}/{}", self.space_name, self.label())
    }
}

/// Marks a message written by an AI agent. Agent messages are posted under the
/// human account (so `creator` is the account, not a distinct identity) — the
/// presence of this field is the only thing that tells them apart. `done` is
/// false while the agent is still streaming its reply.
#[derive(Debug, Clone)]
pub struct Agent {
    pub name: String,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub id: String,
    /// `_ver.id` — the record's position in the space DAG, stamped at creation
    /// and never bumped by edits. Chats are ordered and paged by it (it's
    /// lex-monotonic); `createdAt` is only for display. Empty when absent.
    pub ver: String,
    pub text: String,
    pub creator: String,
    /// Set when an agent authored this message; see [`Agent`].
    pub agent: Option<Agent>,
    pub created_at: f64,
    pub modified_at: f64,
    pub reply_to: Option<String>,
    /// Identities this message pings — server-derived from `any://m/` links in
    /// the text plus the replied-to author. Never written by clients.
    pub mentions: Vec<String>,
    /// Device-local read flags the SDK materializes on each record. Absent
    /// (false) once read; `unread_reactions` only ever sets on your own
    /// messages. (`unreadMention` also exists; the chat-level counter is what
    /// we badge, so it isn't kept here.)
    pub unread: bool,
    pub unread_reactions: bool,
    /// (emoji, count) pairs, sorted for stable rendering.
    pub reactions: Vec<(String, usize)>,
    /// Attached files and links, in key order. Sending attachments isn't
    /// supported; `o` / `s` open or save the ones a message carries.
    pub attachments: Vec<Attachment>,
}

/// One entry of a message's `attachments` map: `{type, link}`.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    /// "image", "file", "link", … — the sender's label, not a mime.
    pub kind: String,
    pub link: String,
}

/// What an attachment's `link` points at.
#[derive(Debug, Clone, PartialEq)]
pub enum AttachmentTarget {
    /// A files-v2 file: `any://f/<spaceId>/<fileId>` (docs/19-links.md), or
    /// the older `any://<spaceId>/files/<fileId>`.
    File { space_id: String, file_id: String },
    /// An object reference (`any://o/…`) — nothing to download.
    Object,
    /// An ordinary web link.
    Url(String),
    Unknown,
}

impl Attachment {
    pub fn target(&self) -> AttachmentTarget {
        let link = self.link.split(['?', '#']).next().unwrap_or("");
        let file = |sp: &str, id: &str| AttachmentTarget::File {
            space_id: sp.to_string(),
            file_id: id.to_string(),
        };
        if let Some(rest) = link.strip_prefix("any://f/") {
            if let Some((sp, id)) = rest.split_once('/') {
                if !sp.is_empty() && !id.is_empty() && !id.contains('/') {
                    return file(sp, id);
                }
            }
            return AttachmentTarget::Unknown;
        }
        if link.starts_with("any://o/") {
            return AttachmentTarget::Object;
        }
        if let Some(rest) = link.strip_prefix("any://") {
            if let [sp, "files", id] = rest.split('/').collect::<Vec<_>>().as_slice() {
                return file(sp, id);
            }
            return AttachmentTarget::Unknown;
        }
        if link.starts_with("https://") || link.starts_with("http://") {
            return AttachmentTarget::Url(self.link.clone());
        }
        AttachmentTarget::Unknown
    }
}

impl Message {
    pub fn from_record(v: &Value) -> Option<Message> {
        let id = v.get("id")?.as_str()?.to_string();
        let created_at = parse_instant(v.get("createdAt")).unwrap_or(0.0);
        let modified_at = parse_instant(v.get("modifiedAt")).unwrap_or(created_at);
        let ver = v
            .pointer("/_ver/id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let flag = |k: &str| v.get(k).and_then(|b| b.as_bool()).unwrap_or(false);
        let mentions: Vec<String> = v
            .get("mentions")
            .and_then(|m| m.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let mut reactions: Vec<(String, usize)> = v
            .get("reactions")
            .and_then(|r| r.as_object())
            .map(|obj| {
                obj.iter()
                    .map(|(emoji, who)| {
                        let n = who.as_object().map(|o| o.len()).unwrap_or(0);
                        (emoji.clone(), n)
                    })
                    .filter(|(_, n)| *n > 0)
                    .collect()
            })
            .unwrap_or_default();
        reactions.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        // {"a01": {"type": "image", "link": "any://f/<space>/<fileId>"}, …}
        let attachments: Vec<Attachment> = v
            .get("attachments")
            .and_then(|a| a.as_object())
            .map(|obj| {
                obj.values()
                    .map(|a| {
                        let s = |k: &str| a.get(k).and_then(|t| t.as_str()).map(str::to_string);
                        Attachment {
                            kind: s("type").unwrap_or_else(|| "file".to_string()),
                            link: s("link").unwrap_or_default(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(Message {
            id,
            ver,
            mentions,
            unread: flag("unread"),
            unread_reactions: flag("unreadReactions"),
            text: v
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            creator: v
                .get("creator")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            // {"agent": {"name": "bao", "done": true}} on agent-authored messages.
            agent: v.get("agent").and_then(|a| a.as_object()).map(|a| Agent {
                name: a
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("agent")
                    .to_string(),
                done: a.get("done").and_then(|d| d.as_bool()).unwrap_or(true),
            }),
            created_at,
            modified_at,
            reply_to: v
                .get("replyToMessageId")
                .and_then(|t| t.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
            reactions,
            attachments,
        })
    }

    pub fn edited(&self) -> bool {
        self.modified_at > self.created_at + 1.0
    }

    /// Display order: DAG order (`_ver.id`) when both records carry it — the
    /// order the server sorts and pages by — else creation time, then id.
    pub fn cmp_order(a: &Message, b: &Message) -> Ordering {
        let by_ver = if !a.ver.is_empty() && !b.ver.is_empty() {
            a.ver.cmp(&b.ver)
        } else {
            a.created_at.total_cmp(&b.created_at)
        };
        by_ver.then_with(|| a.id.cmp(&b.id))
    }

    /// True when this message pings `me` (a text mention or a reply to me).
    pub fn mentions_me(&self, me: &str) -> bool {
        !me.is_empty() && self.mentions.iter().any(|m| m == me)
    }

    /// Old agent run-start pings post a bare "…" placeholder; it carries no
    /// content and shouldn't show as a message.
    pub fn is_agent_presence_marker(&self) -> bool {
        self.agent.is_some() && self.text.trim() == "…"
    }

    /// e.g. "3 images", "1 image, 2 files". Empty when nothing is attached.
    pub fn attachment_summary(&self) -> String {
        if self.attachments.is_empty() {
            return String::new();
        }
        let mut kinds: Vec<(String, usize)> = Vec::new();
        for a in &self.attachments {
            match kinds.iter_mut().find(|(k, _)| *k == a.kind) {
                Some((_, n)) => *n += 1,
                None => kinds.push((a.kind.clone(), 1)),
            }
        }
        kinds
            .iter()
            .map(|(k, n)| {
                if *n == 1 {
                    format!("1 {k}")
                } else {
                    format!("{n} {k}s")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// What to show in a one-line preview. An image-only message has no text,
    /// so fall back to describing the attachments rather than showing nothing.
    pub fn preview_text(&self) -> String {
        if !self.text.trim().is_empty() {
            return self.text.clone();
        }
        match self.attachment_summary().as_str() {
            "" => String::new(),
            s => format!("📎 {s}"),
        }
    }
}

/// One `bao.status` beat off the account event bus (anybao ADR-025): the
/// serving anyrt's presence, published every 10s and again within a second of
/// any change — a run starting or ending, a new tool call, a status line set.
/// A beat is a full-state envelope, so the latest one per `identity` is the
/// truth and staleness is measured on our own receipt clock.
#[derive(Debug, Clone, PartialEq)]
pub struct BaoBeat {
    /// The serving peer id — one entry per device that runs a serve.
    pub identity: String,
    /// `boot` | `idle` | `working` | `shutdown`.
    pub state: String,
    /// Whether this publisher answers chat. A standby device beats too, at
    /// the same cadence, so liveness alone never means "bao is online".
    /// Absent `role` (a serve from before 2026-09-13) reads as active.
    pub active: bool,
    /// The freshest live run — present iff `working`.
    pub run: Option<BaoRun>,
    /// Bao's own status line, when it set one and it has not decayed.
    pub line: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BaoRun {
    pub id: String,
    /// The user's message preview on chat runs, else the program spec.
    pub title: String,
    /// Tool calls so far; the beat republishes on every new one.
    pub cells: u64,
    /// The newest cell's collapsed code preview (≤48 chars), absent before
    /// the first call.
    pub cell: Option<String>,
}

impl BaoBeat {
    /// Parses a bus envelope; `None` for any other event type or a beat
    /// without an identity.
    pub fn from_envelope(v: &Value) -> Option<BaoBeat> {
        if v.get("type").and_then(Value::as_str) != Some("bao.status") {
            return None;
        }
        let d = v.get("data")?;
        let identity = d.get("identity").and_then(Value::as_str)?.to_string();
        let state = d
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("idle")
            .to_string();
        let active = d.get("role").and_then(Value::as_str) != Some("standby");
        let run = d.get("run").and_then(|r| {
            Some(BaoRun {
                id: r.get("id")?.as_str()?.to_string(),
                title: r
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                cells: r.get("cells").and_then(Value::as_u64).unwrap_or(0),
                cell: r
                    .get("cell")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            })
        });
        let line = d
            .get("line")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Some(BaoBeat {
            identity,
            state,
            active,
            run,
            line,
        })
    }
}

/// What the status bar says about bao — the fold any-ui's status bar does
/// over the same beats (ADR-025 §6).
#[derive(Debug, Clone, PartialEq)]
pub enum BaoPresence {
    /// No beat ever seen: render nothing (a serve beats within 10s of start,
    /// and the bus has no replay, so this is also the pre-serve state).
    Unknown,
    /// Beats were seen, but every publisher is stale or shut down.
    Offline,
    /// Somebody beats fresh, but only standby devices: nothing answers chat.
    NoResponder,
    Idle,
    /// A run is live on some device. `doing` is bao's status line, else the
    /// newest cell's code preview, else the run title.
    Working { doing: String, cells: u64 },
}

/// Folds the beats — each paired with whether it is fresh (received within
/// the offline cutoff) — into the presence fact. A working beat wins over an
/// idle one so two serves in pre-election overlap don't flicker the bar.
pub fn derive_bao_presence(beats: &[(&BaoBeat, bool)]) -> BaoPresence {
    if beats.is_empty() {
        return BaoPresence::Unknown;
    }
    let fresh: Vec<&BaoBeat> = beats
        .iter()
        .filter(|(b, fresh)| *fresh && b.state != "shutdown")
        .map(|(b, _)| *b)
        .collect();
    if fresh.is_empty() {
        return BaoPresence::Offline;
    }
    if let Some(w) = fresh.iter().find(|b| b.state == "working") {
        let run = w.run.as_ref();
        let doing = w
            .line
            .clone()
            .or_else(|| run.and_then(|r| r.cell.clone()))
            .or_else(|| run.map(|r| r.title.clone()).filter(|t| !t.is_empty()))
            .unwrap_or_else(|| "working".to_string());
        return BaoPresence::Working {
            doing,
            cells: run.map(|r| r.cells).unwrap_or(0),
        };
    }
    if fresh.iter().any(|b| b.active) {
        BaoPresence::Idle
    } else {
        BaoPresence::NoResponder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn action_body_parses_me() {
        assert_eq!(action_body("/me waves"), Some("waves"));
        assert_eq!(action_body("/me   waves  hi"), Some("waves  hi"));
        assert_eq!(action_body("/me"), None);
        assert_eq!(action_body("/me "), None);
        assert_eq!(action_body("/meow"), None);
        assert_eq!(action_body("hi /me waves"), None);
    }

    #[test]
    fn md_unescape_outside_code() {
        assert_eq!(md_unescape(r"¯\\\_(ツ)\_/¯"), r"¯\_(ツ)_/¯");
        assert_eq!(md_unescape(r"C:\Users \d"), r"C:\Users \d");
        assert_eq!(md_unescape(r"`a\_b` c\_d"), r"`a\_b` c_d");
    }

    #[test]
    fn highlight_whole_words_any_case() {
        let w = vec!["rust".to_string()];
        assert_eq!(highlight_hits("Rust and rustic, RUST.", &w), vec!["Rust", "RUST"]);
        assert!(highlight_hits("trust", &w).is_empty());
    }

    /// A working beat as the bus delivered it on 2026-09-21 (envelope verbatim
    /// minus the peer ids).
    fn working_beat() -> Value {
        json!({
            "type": "bao.status", "scope": "account", "target": "peer1",
            "data": {
                "identity": "peer1", "role": "active", "state": "working", "winner": "peer1",
                "run": {"id": "run_1", "startedAt": 1789944008.9, "cells": 3,
                        "title": "list the objects in this space, then tell me how many t",
                        "cell": "rows = c.query_objects(\"tui-test\", filter={\"any."}
            },
            "sender": {"identity": "A9fB", "self": true}
        })
    }

    #[test]
    fn beat_parses_and_prefers_line_then_cell_then_title() {
        let b = BaoBeat::from_envelope(&working_beat()).unwrap();
        assert!(b.active && b.state == "working" && b.line.is_none());
        let run = b.run.as_ref().unwrap();
        assert_eq!((run.cells, run.id.as_str()), (3, "run_1"));
        match derive_bao_presence(&[(&b, true)]) {
            BaoPresence::Working { doing, cells } => {
                assert!(doing.starts_with("rows = c.query_objects"));
                assert_eq!(cells, 3);
            }
            other => panic!("{other:?}"),
        }
        // Before the first tool call there is no cell: the run title stands in.
        let mut v = working_beat();
        v["data"]["run"].as_object_mut().unwrap().remove("cell");
        v["data"]["run"]["cells"] = json!(0);
        let b = BaoBeat::from_envelope(&v).unwrap();
        assert!(matches!(derive_bao_presence(&[(&b, true)]),
            BaoPresence::Working { doing, cells: 0 } if doing.starts_with("list the objects")));
        // Bao's own line beats both.
        v["data"]["line"] = json!("counting objects");
        let b = BaoBeat::from_envelope(&v).unwrap();
        assert!(matches!(derive_bao_presence(&[(&b, true)]),
            BaoPresence::Working { doing, .. } if doing == "counting objects"));
    }

    #[test]
    fn presence_folds_freshness_role_and_shutdown() {
        let idle = BaoBeat::from_envelope(&json!({"type": "bao.status",
            "data": {"identity": "p1", "state": "idle", "role": "active"}})).unwrap();
        let standby = BaoBeat::from_envelope(&json!({"type": "bao.status",
            "data": {"identity": "p2", "state": "idle", "role": "standby"}})).unwrap();
        let down = BaoBeat::from_envelope(&json!({"type": "bao.status",
            "data": {"identity": "p1", "state": "shutdown"}})).unwrap();
        let working = BaoBeat::from_envelope(&working_beat()).unwrap();
        assert_eq!(derive_bao_presence(&[]), BaoPresence::Unknown);
        assert_eq!(derive_bao_presence(&[(&idle, true)]), BaoPresence::Idle);
        assert_eq!(derive_bao_presence(&[(&idle, false)]), BaoPresence::Offline);
        assert_eq!(derive_bao_presence(&[(&down, true)]), BaoPresence::Offline);
        assert_eq!(derive_bao_presence(&[(&standby, true)]), BaoPresence::NoResponder);
        // A working device wins over an idle one, whatever the order.
        assert!(matches!(derive_bao_presence(&[(&idle, true), (&working, true)]),
            BaoPresence::Working { .. }));
        // Other event types are not beats; a pre-role beat is active.
        assert!(BaoBeat::from_envelope(&json!({"type": "ui.open_space", "data": {"identity": "x"}})).is_none());
        let old = BaoBeat::from_envelope(&json!({"type": "bao.status",
            "data": {"identity": "p3", "state": "idle"}})).unwrap();
        assert!(old.active);
    }

    /// The general-chat root as the one-type-per-object daemon sends it
    /// (late 2026-09, taken from a live row): a bundle root that is its own
    /// type, so `any.type` is the `__type__` marker — the root's own id
    /// appears nowhere on the row but `id`. No `"chat"` literal anywhere.
    fn general_chat_record() -> Value {
        json!({
            "id": "bafyroot",
            "any": {"type": "__type__", "collections": ["miniapp"], "name": "General"},
            "type": {"xkey": "general_chat", "hidden": true, "layout": {"type": "chat"}},
            "miniapp": {"bundle": "system:general-chat/v1", "pos": "a1"},
            "chat": {"unreadCount": 3, "unreadMentions": 1},
        })
    }

    #[test]
    fn chat_record_matches_on_owner_id() {
        let owners = vec!["bafyroot".to_string()];
        let mut rec = general_chat_record();
        rec["type"]["layout"] = json!({"type": "page"}); // owner id alone must do
        let c = Chat::from_record(&rec, "s", "sp", &owners).unwrap();
        assert_eq!(c.name, "General");
        assert_eq!(c.label(), "General");
        assert_eq!(c.pos, "a1");
        assert_eq!((c.unread, c.unread_mentions, c.unread_reactions), (3, 1, 0));
    }

    #[test]
    fn chat_record_matches_on_chat_layout_without_owners() {
        let c = Chat::from_record(&general_chat_record(), "s", "sp", &[]).unwrap();
        assert_eq!(c.object_id, "bafyroot");
    }

    /// The live row has no `miniapp.pos` until the user reorders the sidebar;
    /// an unnamed chat falls back to a generic label.
    #[test]
    fn chat_record_without_pos_or_name() {
        let owners = vec!["c1".to_string()];
        let rec = json!({"id": "c1", "any": {"type": "__type__"}, "type": {"layout": {"type": "chat"}}});
        let c = Chat::from_record(&rec, "s", "sp", &owners).unwrap();
        assert_eq!(c.label(), "space chat");
        assert_eq!(c.pos, "");
    }

    #[test]
    fn non_chat_type_records_are_skipped() {
        let owners = vec!["bafyroot".to_string()];
        let rec = json!({
            "id": "t1",
            "any": {"type": "__type__"},
            "type": {"xkey": "agent_log", "hidden": true},
        });
        assert!(Chat::from_record(&rec, "s", "sp", &owners).is_none());
        // A type marker row whose id is not an owner is not a chat either.
        let other = json!({"id": "x1", "any": {"type": "__type__"}, "type": {"xkey": "notes"}});
        assert!(Chat::from_record(&other, "s", "sp", &owners).is_none());
        let page = json!({"id": "p1", "any": {"type": "page", "collections": ["miniapp"]}, "miniapp": {"pos": "a0"}});
        assert!(Chat::from_record(&page, "s", "sp", &owners).is_none());
    }

    /// The shape the server actually sends, taken from a live response.
    fn record_with_attachments(text: &str, kinds: &[&str]) -> Value {
        let mut atts = serde_json::Map::new();
        for (i, k) in kinds.iter().enumerate() {
            atts.insert(
                format!("f{i}"),
                json!({"type": k, "link": "any://space.x/files/abc"}),
            );
        }
        json!({
            "id": "m1",
            "text": text,
            "creator": "A8qy",
            "createdAt": 1784141327.0,
            "attachments": Value::Object(atts),
        })
    }

    #[test]
    fn parses_attachment_kinds() {
        let m = Message::from_record(&record_with_attachments("scr", &["image"])).unwrap();
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].kind, "image");
        assert_eq!(m.attachment_summary(), "1 image");
    }

    #[test]
    fn attachment_targets() {
        let t = |link: &str| Attachment { kind: "image".into(), link: link.into() }.target();
        let file = |sp: &str, id: &str| AttachmentTarget::File { space_id: sp.into(), file_id: id.into() };
        assert_eq!(t("any://f/sp.1/NbMAco"), file("sp.1", "NbMAco"));
        assert_eq!(t("any://f/sp.1/NbMAco?variant=thumb"), file("sp.1", "NbMAco"));
        assert_eq!(t("any://sp.1/files/abc"), file("sp.1", "abc"));
        assert_eq!(t("any://o/sp.1/obj"), AttachmentTarget::Object);
        assert_eq!(t("https://x.io/a.png"), AttachmentTarget::Url("https://x.io/a.png".into()));
        assert_eq!(t("any://f/sp.1"), AttachmentTarget::Unknown);
        assert_eq!(t(""), AttachmentTarget::Unknown);
    }

    #[test]
    fn groups_and_pluralises() {
        let m =
            Message::from_record(&record_with_attachments("x", &["image", "image", "image"]))
                .unwrap();
        assert_eq!(m.attachment_summary(), "3 images");
    }

    #[test]
    fn summarises_mixed_kinds() {
        let m = Message::from_record(&record_with_attachments("x", &["image", "file"])).unwrap();
        let s = m.attachment_summary();
        assert!(s.contains("1 image") && s.contains("1 file"), "{s}");
    }

    #[test]
    fn no_attachments_yields_empty_summary() {
        let m = Message::from_record(&json!({"id":"m","text":"hi","createdAt":1.0})).unwrap();
        assert_eq!(m.attachment_summary(), "");
        assert_eq!(m.preview_text(), "hi");
    }

    #[test]
    fn image_only_message_previews_as_attachment() {
        // Otherwise an image-only message shows a blank row in the sidebar.
        let m = Message::from_record(&record_with_attachments("", &["image"])).unwrap();
        assert_eq!(m.preview_text(), "📎 1 image");
    }

    #[test]
    fn parses_instant_and_legacy_number() {
        // The wire shape since 2026-08: an instant object.
        let t = parse_instant(Some(&json!({"$date": "2026-08-31T08:52:40.000Z"}))).unwrap();
        assert_eq!(t, 1788166360.0);
        // Rows from older peers/builds: bare unix seconds.
        assert_eq!(parse_instant(Some(&json!(1784141327.0))), Some(1784141327.0));
        assert_eq!(parse_instant(Some(&json!(1784141327))), Some(1784141327.0));
        assert_eq!(parse_instant(None), None);
        assert_eq!(parse_instant(Some(&json!({"$date": "garbage"}))), None);
    }

    #[test]
    fn message_reads_ver_flags_and_mentions() {
        let m = Message::from_record(&json!({
            "id": "m1", "text": "hi", "creator": "A1",
            "_ver": {"id": "!!.+", "text": "!!.+"},
            "createdAt": {"$date": "2026-08-31T08:52:40.000Z"},
            "modifiedAt": {"$date": "2026-08-31T08:52:40.000Z"},
            "mentions": ["A2", "A3"],
            "unread": true, "unreadMention": true,
        }))
        .unwrap();
        assert_eq!(m.ver, "!!.+");
        assert_eq!(m.created_at, 1788166360.0);
        assert!(!m.edited());
        assert_eq!(m.mentions, vec!["A2", "A3"]);
        assert!(m.unread && !m.unread_reactions);
        assert!(m.mentions_me("A2") && !m.mentions_me("A1") && !m.mentions_me(""));
    }

    #[test]
    fn orders_by_ver_then_falls_back_to_time() {
        let mk = |id: &str, ver: &str, t: f64| {
            Message::from_record(&json!({"id": id, "_ver": {"id": ver}, "createdAt": t})).unwrap()
        };
        // `_ver.id` is lex-monotonic DAG order; it wins over wall-clock time.
        let a = mk("a", "!!(Y", 200.0);
        let b = mk("b", "!!)c", 100.0);
        assert_eq!(Message::cmp_order(&a, &b), Ordering::Less);
        // A record without `_ver` (older build) falls back to time.
        let c = mk("c", "", 50.0);
        assert_eq!(Message::cmp_order(&c, &a), Ordering::Less);
    }

    #[test]
    fn renders_mention_links_as_chips() {
        let names = |id: &str| (id == "A2").then(|| "Zarko".to_string());
        let (out, chips) = render_mentions(
            "Hey [Old Name](any://m/sp1/A2), see [docs](https://x.y) and [Bob](any://m/sp1/A9)",
            names,
        );
        // Resolved to the current name; unknown identity keeps the snapshot;
        // ordinary links are untouched.
        assert_eq!(out, "Hey @Zarko, see [docs](https://x.y) and @Bob");
        assert_eq!(chips, vec!["@Zarko", "@Bob"]);
        // Unbalanced brackets pass through.
        assert_eq!(render_mentions("a [b (c", |_| None).0, "a [b (c");
    }

    #[test]
    fn links_typed_mentions_longest_first() {
        let roster = vec![
            ("Anna".to_string(), "A1".to_string()),
            ("Anna Lee".to_string(), "A2".to_string()),
            ("".to_string(), "A3".to_string()),
        ];
        assert_eq!(
            link_mentions("hi @anna lee and @Anna, @annabelle, me@anna", "sp", &roster),
            "hi [Anna Lee](any://m/sp/A2) and [Anna](any://m/sp/A1), @annabelle, me@anna"
        );
        assert_eq!(link_mentions("no mentions", "sp", &roster), "no mentions");
    }
}

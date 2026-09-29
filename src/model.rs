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
    /// `any.space`, or `any.onetoone` for a DM.
    #[serde(default, rename = "spaceType")]
    pub space_type: String,
    /// The space's icon, same forms as a profile's (see [`icon_glyph`]).
    #[serde(default, rename = "iconCid", deserialize_with = "null_as_default")]
    pub icon_cid: String,
}

impl Space {
    pub fn is_dm(&self) -> bool {
        self.space_type == "any.onetoone"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Identity {
    pub identity: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub name: String,
    /// Spaces this identity is a member of, when the directory knows. Used to
    /// scope `@` completion to the current space; empty means "unknown".
    /// The wire sends `null` for identities it knows no spaces of.
    #[serde(default, rename = "spaceIds", deserialize_with = "null_as_default")]
    pub space_ids: Vec<String>,
    /// The profile icon as stored: an emoji, an `icon:v2:{…}` assignment, or
    /// a picture reference (not shown here). See [`icon_glyph`].
    #[serde(default, rename = "iconCid", deserialize_with = "null_as_default")]
    pub icon_cid: String,
}

/// `null` reads as the type's default, like an absent field does.
fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// A terminal-renderable form of a stored icon, if it has one: an emoji as
/// is, an `icon:v2:` emoji assignment's grapheme, or a pack glyph we can
/// stand in for with a Unicode symbol. Pictures (`any://f/…`, CIDs) and
/// unmapped glyphs give `None` — the name alone is shown then.
pub fn icon_glyph(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(json) = raw.strip_prefix("icon:v2:") {
        let v: Value = serde_json::from_str(json).ok()?;
        let a = v.get("assignment")?;
        return match a.get("kind")?.as_str()? {
            "emoji" => a.get("grapheme")?.as_str().filter(|g| !g.is_empty()).map(str::to_string),
            "pack" => pack_glyph(a.pointer("/ref/glyphId")?.as_str()?).map(str::to_string),
            _ => None,
        };
    }
    // A bare emoji: short and not plain ASCII (which would be a CID, a
    // `bafy…` hash or an `any://` link — pictures, not glyphs).
    let short = raw.chars().count() <= 8 && !raw.contains("://");
    (short && !raw.is_ascii()).then(|| raw.to_string())
}

/// A Unicode stand-in for an icon-pack glyph id (lucide kebab-case, iconoir
/// PascalCase). Variant words (`Solid`, `Off`, `Circle`, …) are dropped, then
/// the id is matched exactly, then by the arrow direction or the main noun it
/// contains. None for anything without a fair single-glyph equivalent.
fn pack_glyph(id: &str) -> Option<&'static str> {
    let mut key: String = id.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
    for suffix in ["solid", "filled", "outline", "fill", "off", "alt", "circle", "square", "2", "3"] {
        if key.len() > suffix.len() + 2
            && let Some(k) = key.strip_suffix(suffix)
        {
            key = k.to_string();
        }
    }
    let exact = match key.as_str() {
        "home" | "house" => "⌂",
        "star" => "★",
        "heart" => "♥",
        "check" => "✓",
        "x" | "xmark" | "cancel" => "✗",
        "sun" => "☀",
        "moon" | "halfmoon" => "☾",
        "cloud" => "☁",
        "flag" => "⚑",
        "music" | "musicnote" => "♪",
        "zap" | "flash" | "lightning" => "⚡",
        "flame" | "fire" => "🔥",
        "rocket" => "🚀",
        "leaf" => "🍃",
        "coffee" | "cup" => "☕",
        "book" | "bookopen" | "openbook" => "📖",
        "user" | "profile" => "☺",
        "globe" | "earth" | "internet" => "🌐",
        "code" | "codebrackets" => "⌨",
        "terminal" => "⌘",
        "bell" => "🔔",
        "mail" | "envelope" => "✉",
        "camera" => "📷",
        "gamepad" => "🎮",
        "anchor" => "⚓",
        "umbrella" => "☂",
        "snowflake" => "❄",
        "diamond" | "gem" => "◆",
        "triangle" => "▲",
        "planet" => "🪐",
        "airplane" | "plane" => "✈",
        "inputoutput" | "repeat" | "refresh" | "sync" => "⇄",
        "lock" => "🔒",
        "key" => "⚷",
        "folder" => "🗀",
        "calendar" => "📅",
        "clock" | "timer" => "◷",
        "chat" | "message" | "chatbubble" => "💬",
        "team" | "users" | "group" | "community" => "☻",
        "sparkles" | "sparks" => "✦",
        "lightbulb" | "bulb" => "💡",
        "trophy" => "🏆",
        "tree" => "🌲",
        "flower" => "✿",
        "car" => "🚗",
        _ => "",
    };
    if !exact.is_empty() {
        return Some(exact);
    }
    // Arrows by direction, diagonal first (`arrowupright` contains `arrowup`).
    for (part, glyph) in [
        ("upright", "↗"),
        ("upleft", "↖"),
        ("downright", "↘"),
        ("downleft", "↙"),
        ("arrowup", "↑"),
        ("arrowdown", "↓"),
        ("arrowleft", "←"),
        ("arrowright", "→"),
    ] {
        if key.contains("arrow") && key.contains(part) {
            return Some(glyph);
        }
    }
    None
}

/// An `icon:v2` icon's colour name (`red`, `teal`, `ice`, …), when it has one.
pub fn icon_color(raw: &str) -> Option<String> {
    let json = raw.trim().strip_prefix("icon:v2:")?;
    let v: Value = serde_json::from_str(json).ok()?;
    v.get("color")?.as_str().filter(|c| !c.is_empty()).map(str::to_string)
}

/// How emoji reach the terminal (`/icons`). Terminals, tmux and mosh each
/// keep their own character-width tables, and they disagree on emoji newer
/// than Unicode 9 and on composed sequences (ZWJ, VS16, skin tones): the
/// cursor drifts, text lands a column off and stale cells stay behind.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EmojiMode {
    /// Icons on; composed emoji reduced to their base, newer ones `◌`.
    Safe,
    /// Icons off; emoji as 2-column text stand-ins (`+1`, `<3`) or `◌`.
    Off,
    /// Everything as sent — for terminals that agree with each other.
    Full,
}

impl EmojiMode {
    pub fn parse(s: &str) -> Option<EmojiMode> {
        match s {
            "safe" => Some(EmojiMode::Safe),
            "off" => Some(EmojiMode::Off),
            "full" => Some(EmojiMode::Full),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            EmojiMode::Safe => "safe",
            EmojiMode::Off => "off",
            EmojiMode::Full => "full",
        }
    }
    /// `/icons` with no argument: safe → off → full → safe.
    pub fn next(self) -> EmojiMode {
        match self {
            EmojiMode::Safe => EmojiMode::Off,
            EmojiMode::Off => EmojiMode::Full,
            EmojiMode::Full => EmojiMode::Safe,
        }
    }
}

/// True for emoji added after Unicode 9 (2016) — the ones width tables
/// disagree on. Ranges, not a per-character list: close enough, and it errs
/// towards `◌`.
pub fn emoji_is_new(c: char) -> bool {
    let u = c as u32;
    // Unicode 8/9 islands inside the supplemental block stay; the rest of
    // it, and everything from U+1FA70, came later.
    let old_supplemental = [
        (0x1F910, 0x1F91E),
        (0x1F920, 0x1F927),
        (0x1F930, 0x1F930),
        (0x1F933, 0x1F93E),
        (0x1F940, 0x1F94B),
        (0x1F950, 0x1F95E),
        (0x1F980, 0x1F991),
        (0x1F9C0, 0x1F9C0),
    ];
    match u {
        0x1F90C..=0x1F9FF => !old_supplemental.iter().any(|&(a, b)| (a..=b).contains(&u)),
        0x1FA70..=0x1FAFF => true,
        0x1F6D5..=0x1F6DF | 0x1F6F7..=0x1F6FF | 0x1F7E0..=0x1F7FF => true,
        _ => false,
    }
}

/// A pictographic emoji, as opposed to a text symbol (`★`, `●`, `↗`).
fn is_pictograph(c: char) -> bool {
    matches!(c as u32, 0x1F000..=0x1FAFF)
}

/// Emoji the TUI itself uses as markers; `off` keeps them.
const UI_EMOJI: [&str; 6] = ["🔒", "📎", "🔗", "✉", "💤", "✦"];

/// What one screen cell's grapheme becomes under `mode`, or None to leave
/// it. The result is never wider than the input — ratatui already laid the
/// line out for the original width, and a narrower symbol leaves the
/// following (blank) cell as padding.
pub fn terminal_safe(sym: &str, mode: EmojiMode) -> Option<String> {
    if mode == EmojiMode::Full || sym.is_ascii() {
        return None;
    }
    // Composed sequences: keep the first component; drop variation
    // selectors, skin tones, tag characters and keycap marks.
    let base: String = sym
        .split('\u{200D}')
        .next()
        .unwrap_or("")
        .chars()
        .filter(|&c| {
            !matches!(c as u32, 0xFE0E | 0xFE0F | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F | 0x20E3)
        })
        .collect();
    let off = mode == EmojiMode::Off && !UI_EMOJI.contains(&base.as_str());
    let out = if let Some(t) = emoji_text(&base).filter(|_| off) {
        t.to_string()
    } else if (off && base.chars().any(is_pictograph)) || base.chars().any(emoji_is_new) {
        "◌".to_string()
    } else if base.is_empty() {
        " ".to_string()
    } else {
        base
    };
    use unicode_width::UnicodeWidthStr;
    (out != sym && out.width() <= sym.width().max(1)).then_some(out)
}

/// IRC-style text for the most common reaction / smiley emoji — exactly two
/// columns, the width the emoji took.
fn emoji_text(e: &str) -> Option<&'static str> {
    Some(match e {
        "👍" => "+1",
        "👎" => "-1",
        "❤" | "💜" | "💙" | "💚" | "🧡" | "💛" | "🖤" => "<3",
        "😂" | "🤣" | "😆" | "😄" | "😁" | "😃" | "😀" => ":D",
        "🙂" | "😊" | "☺" => ":)",
        "😉" => ";)",
        "🙁" | "☹" | "😞" | "😢" | "😭" => ":(",
        "😮" | "😲" | "😯" => ":o",
        "😛" | "😜" | "😝" => ":P",
        "👀" => "oo",
        "🙏" => "ty",
        "🎉" => "o/",
        "👌" => "ok",
        "🔥" => "!!",
        _ => return None,
    })
}

/// How one space is syncing right now (`GET /spaces/{id}/sync-status`, and
/// the `status` frames of `/sync-status/subscribe`). The peer counts are live
/// connections: sync nodes, LAN peers, and internet-wide direct peers (iroh,
/// relayed or hole-punched — any docs/30-global-p2p.md).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SyncStatus {
    #[serde(default, rename = "spaceId")]
    pub space_id: String,
    /// unknown | offline | syncing | synced | error
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub synced: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default, rename = "networkPeers")]
    pub network_peers: u64,
    #[serde(default, rename = "localPeers")]
    pub local_peers: u64,
    #[serde(default, rename = "globalPeers")]
    pub global_peers: u64,
    /// unknown | notpossible | notconnected | connected | restricted
    #[serde(default)]
    pub p2p: String,
}

/// One device of your account, from the registry (`GET /devices`, any
/// docs/23-devices.md). The registry says which devices exist, never which
/// are online — that comes from the p2p layer ([`peer_liveness`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub peer_id: String,
    pub name: String,
    pub os: String,
    pub version: String,
    /// Installed app slugs (`bao`, …).
    pub apps: Vec<String>,
}

/// The registry listing: devices, this device's peer id (`self`), and the
/// server-computed active device per app slug (`active`).
#[derive(Debug, Clone, Default)]
pub struct Devices {
    pub devices: Vec<Device>,
    pub me: String,
    pub active: std::collections::HashMap<String, String>,
}

impl Devices {
    pub fn from_json(v: &Value) -> Devices {
        let str_of = |x: &Value, k: &str| x.get(k).and_then(|s| s.as_str()).unwrap_or("").to_string();
        let devices = v
            .get("devices")
            .and_then(|d| d.as_array())
            .map(|a| {
                a.iter()
                    .map(|d| Device {
                        peer_id: str_of(d, "peerId"),
                        name: str_of(d, "name"),
                        os: str_of(d, "os"),
                        version: str_of(d, "version"),
                        apps: d
                            .get("apps")
                            .and_then(|a| a.as_object())
                            .map(|o| o.keys().cloned().collect())
                            .unwrap_or_default(),
                    })
                    .filter(|d| !d.peer_id.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let active = v
            .get("active")
            .and_then(|a| a.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, p)| p.as_str().map(|p| (k.clone(), p.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Devices { devices, me: str_of(v, "self"), active }
    }
}

/// How one of your own devices is reachable right now, per the p2p layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Liveness {
    pub connected: bool,
    /// "lan" or "p2p" — the path it's connected over (LAN preferred).
    pub via: &'static str,
    /// Unix seconds of the last sign of life, when the layer has one.
    pub last_seen: Option<f64>,
}

/// Every known peer's liveness from `GET /debug/p2p`, keyed by peer id —
/// the id the device registry uses too, so looking a registry row up here
/// tells whether that device of yours is online. A peer in both layers is
/// connected if either says so (LAN preferred as the path). A device not
/// listed was never seen, or has been silent for 30 days.
pub fn peer_liveness(v: &Value) -> std::collections::HashMap<String, Liveness> {
    let mut out: std::collections::HashMap<String, Liveness> = std::collections::HashMap::new();
    for (list, via) in [(v.get("peers"), "lan"), (v.pointer("/global/peers"), "p2p")] {
        for p in list.and_then(|l| l.as_array()).into_iter().flatten() {
            let Some(id) = p.get("peerId").and_then(|i| i.as_str()) else { continue };
            let connected = p.get("connected").and_then(|c| c.as_bool()).unwrap_or(false);
            let seen = parse_instant(p.get("lastSeen"));
            let l = out.entry(id.to_string()).or_insert(Liveness { connected: false, via, last_seen: None });
            if connected && !l.connected {
                l.connected = true;
                l.via = via;
            }
            l.last_seen = match (l.last_seen, seen) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
        }
    }
    out
}

/// Live direct peers of one space, split by whose device they are — from
/// `GET /debug/p2p`, since the sync-status counts don't say. Your own
/// devices find each other through the account record and sync every space,
/// so counting them would light up every space.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DirectPeers {
    /// Other people's devices on the local network.
    pub lan: u64,
    /// Other people's devices over the internet (iroh).
    pub p2p: u64,
    /// Your own other devices, over either.
    pub own: u64,
}

/// Folds `/debug/p2p` into per-space [`DirectPeers`]. Both peer lists count
/// (`peers` is the LAN layer, `global.peers` the iroh one) and a device in
/// both counts once — as LAN when it's connected there. Only `connected`
/// peers count; `sources` containing `account` marks your own device.
pub fn direct_peers(v: &Value) -> std::collections::HashMap<String, DirectPeers> {
    use std::collections::HashMap;
    let list = |p: Option<&Value>| p.and_then(|p| p.as_array()).cloned().unwrap_or_default();
    let connected = |p: &Value| p.get("connected").and_then(|c| c.as_bool()).unwrap_or(false);
    let id = |p: &Value| p.get("peerId").and_then(|i| i.as_str()).unwrap_or("").to_string();
    // peerId -> (on LAN, own device, spaces)
    let mut peers: HashMap<String, (bool, bool, Vec<String>)> = HashMap::new();
    for (p, lan) in list(v.get("peers")).iter().map(|p| (p, true))
        .chain(list(v.pointer("/global/peers")).iter().map(|p| (p, false)).collect::<Vec<_>>())
    {
        if !connected(p) || id(p).is_empty() {
            continue;
        }
        let own = p
            .get("sources")
            .and_then(|s| s.as_array())
            .is_some_and(|s| s.iter().any(|x| x == "account"));
        let spaces: Vec<String> = p
            .get("spaceIds")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let e = peers.entry(id(p)).or_insert((false, false, Vec::new()));
        e.0 |= lan;
        e.1 |= own;
        for sp in spaces {
            if !e.2.contains(&sp) {
                e.2.push(sp);
            }
        }
    }
    let mut out: HashMap<String, DirectPeers> = HashMap::new();
    for (_, (lan, own, spaces)) in peers {
        for sp in spaces {
            let d = out.entry(sp).or_default();
            match (own, lan) {
                (true, _) => d.own += 1,
                (false, true) => d.lan += 1,
                (false, false) => d.p2p += 1,
            }
        }
    }
    out
}

impl SyncStatus {
    /// The status-bar segment: sync state, then the paths it runs over —
    /// `✓ 3 nodes · 1 lan · 2 p2p · 1 own`; `short` drops the words for
    /// narrow bars. With `direct` (from `/debug/p2p`) the direct peers are
    /// split into other people's and your own devices; without it, the
    /// sync-status counts are shown as they are.
    pub fn summary(&self, short: bool, direct: Option<DirectPeers>) -> String {
        let state = match self.state.as_str() {
            "synced" => "✓".to_string(),
            "syncing" if self.total > 0 => format!("⟳ {}/{}", self.synced, self.total),
            "syncing" => "⟳".to_string(),
            "offline" => "✗ offline".to_string(),
            "error" => "✗ sync error".to_string(),
            _ => "?".to_string(),
        };
        let mut paths = Vec::new();
        let mut path = |n: u64, long: &str, s: &str| {
            if n > 0 {
                paths.push(if short { format!("{s}{n}") } else { format!("{n} {long}") });
            }
        };
        path(self.network_peers, "nodes", "n");
        match direct {
            Some(d) => {
                path(d.lan, "lan", "l");
                path(d.p2p, "p2p", "p");
                path(d.own, "own", "o");
            }
            None => {
                path(self.local_peers, "lan", "l");
                path(self.global_peers, "p2p", "p");
            }
        }
        // Say why direct sync can't happen, when the OS or config forbids it.
        match self.p2p.as_str() {
            "restricted" => paths.push("p2p blocked".into()),
            "notpossible" if !short => paths.push("no p2p".into()),
            _ => {}
        }
        if paths.is_empty() {
            state
        } else {
            format!("{state} {}", paths.join(if short { " " } else { " · " }))
        }
    }
}

/// How many leading characters of an identity stand for it on screen.
pub const SHORT_ID_LEN: usize = 7;

/// The on-screen short form of an identity: its first [`SHORT_ID_LEN`]
/// characters. Shown for people with no known name, and in brackets after
/// a name so two people with the same name stay apart.
pub fn short_id(identity: &str) -> String {
    identity.chars().take(SHORT_ID_LEN).collect()
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
    find_ci(text, words, true)
}

/// Every case-insensitive occurrence of `words` in `text`, as the exact
/// substrings found; `whole_word` requires word boundaries on both sides.
pub fn find_ci(text: &str, words: &[String], whole_word: bool) -> Vec<String> {
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
            if !whole_word || (before && after) {
                hits.push(text[i..end].to_string());
            }
            from = end;
        }
    }
    hits
}

/// Web links (`http://…`, `https://…`) in message text, in order, as
/// written. A URL ends at whitespace, a markdown link's `)`, `<>"'` or a
/// closing bracket; trailing sentence punctuation and an unbalanced `)` are
/// not part of it.
pub fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = ["https://", "http://"].iter().filter_map(|p| rest.find(p)).min() {
        let tail = &rest[i..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | ']' | '`'))
            .unwrap_or(tail.len());
        let mut url = &tail[..end];
        loop {
            let trimmed = url.trim_end_matches(['.', ',', ';', ':', '!', '?']);
            // A `)` belongs to the URL only when it closes one opened inside it
            // (wikipedia-style); otherwise it closes the markdown link.
            let trimmed = if trimmed.ends_with(')') && trimmed.matches('(').count() < trimmed.matches(')').count() {
                &trimmed[..trimmed.len() - 1]
            } else {
                trimmed
            };
            if trimmed.len() == url.len() {
                break;
            }
            url = trimmed;
        }
        if url.len() > "https://".len() {
            out.push(url.to_string());
        }
        rest = &tail[end.max(1)..];
    }
    out
}

/// The canonical link to one chat message (docs/19-links.md, kind `o`).
pub fn message_uri(space_id: &str, chat_id: &str, msg_id: &str) -> String {
    format!("any://o/{space_id}/{chat_id}/chat_messages/{msg_id}")
}

/// A whisper: a DM message about a message in another chat. On the wire it
/// is ordinary DM text that opens with a link to that message,
/// `[↪ Anna: shall we…](any://o/<sp>/<chat>/chat_messages/<id>) text` —
/// readable in any client, indexed as a link edge, and private because it
/// lives in the 1-1 space.
#[derive(Debug, Clone, PartialEq)]
pub struct Whisper {
    /// The quote in the link text (`↪ Anna: shall we…`).
    pub label: String,
    pub space_id: String,
    pub chat_id: String,
    pub msg_id: String,
    pub body: String,
}

/// Builds a whisper's text. The label is flattened to one short line with
/// no brackets, so it can't break the link.
pub fn whisper_text(label: &str, space_id: &str, chat_id: &str, msg_id: &str, body: &str) -> String {
    let flat: String = label
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !matches!(c, '[' | ']' | '\\'))
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = if flat.chars().count() > 60 {
        format!("{}…", flat.chars().take(59).collect::<String>())
    } else {
        flat
    };
    format!("[{label}]({}) {body}", message_uri(space_id, chat_id, msg_id))
}

pub fn parse_whisper(text: &str) -> Option<Whisper> {
    let rest = text.strip_prefix('[')?;
    let close = rest.find("](")?;
    let label = &rest[..close];
    let after = &rest[close + 2..];
    let end = after.find(')')?;
    let uri = after[..end].strip_prefix("any://o/")?;
    let [space_id, chat_id, "chat_messages", msg_id] = uri.split('/').collect::<Vec<_>>()[..] else {
        return None;
    };
    Some(Whisper {
        label: label.to_string(),
        space_id: space_id.to_string(),
        chat_id: chat_id.to_string(),
        msg_id: msg_id.to_string(),
        body: after[end + 1..].trim_start().to_string(),
    })
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
        if let Some(w) = parse_whisper(&self.text) {
            return format!("🔒 {}", w.body);
        }
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
    fn identities_tolerate_nulls() {
        let id: Identity = serde_json::from_value(json!({
            "identity": "A1", "name": "Anna", "spaceIds": null, "iconCid": null
        }))
        .unwrap();
        assert_eq!((id.name.as_str(), id.space_ids.len(), id.icon_cid.as_str()), ("Anna", 0, ""));
    }

    #[test]
    fn sync_summary() {
        let st: SyncStatus = serde_json::from_value(json!({
            "spaceId": "sp", "state": "synced", "synced": 5, "total": 5,
            "networkPeers": 3, "localPeers": 0, "globalPeers": 1, "p2p": "connected"
        }))
        .unwrap();
        assert_eq!(st.summary(false, None), "✓ 3 nodes · 1 p2p");
        assert_eq!(st.summary(true, None), "✓ n3 p1");
        let d = DirectPeers { lan: 0, p2p: 2, own: 1 };
        assert_eq!(st.summary(false, Some(d)), "✓ 3 nodes · 2 p2p · 1 own");
        let st = SyncStatus { state: "syncing".into(), synced: 1, total: 3, p2p: "restricted".into(), ..Default::default() };
        assert_eq!(st.summary(false, None), "⟳ 1/3 p2p blocked");
    }

    #[test]
    fn devices_and_liveness() {
        let d = Devices::from_json(&json!({
            "self": "P1",
            "active": {"bao": "P2"},
            "devices": [
                {"peerId": "P1", "name": "x32", "os": "linux", "version": "v1", "apps": {"bao": {}}},
                {"peerId": "P2", "name": "Air", "os": "darwin", "version": "v2", "apps": {"bao": {}}},
            ]
        }));
        assert_eq!((d.me.as_str(), d.active["bao"].as_str(), d.devices.len()), ("P1", "P2", 2));
        assert_eq!(d.devices[1].apps, vec!["bao"]);
        let live = peer_liveness(&json!({
            "peers": [{"peerId": "P2", "connected": false, "lastSeen": {"$date": "2026-09-29T00:00:00Z"}}],
            "global": {"peers": [{"peerId": "P2", "connected": true, "sources": ["account"], "lastSeen": "2026-09-29T01:00:00Z"}]}
        }));
        assert!(live["P2"].connected);
        assert_eq!(live["P2"].via, "p2p");
        assert_eq!(live["P2"].last_seen, Some(1790643600.0));
    }

    #[test]
    fn direct_peers_split_own_devices() {
        // The /debug/p2p shape as `any` serves it (2026-09-29).
        let v = json!({
            "peers": [
                {"peerId": "L1", "connected": true, "sources": ["lan", "global"], "spaceIds": ["s1"]},
            ],
            "global": { "peers": [
                {"peerId": "ME", "connected": true, "sources": ["global", "account"], "spaceIds": ["s1", "s2"]},
                {"peerId": "G1", "connected": true, "sources": ["global"], "spaceIds": ["s1"]},
                {"peerId": "L1", "connected": true, "sources": ["lan", "global"], "spaceIds": ["s1"]},
                {"peerId": "OLD", "connected": false, "sources": ["global"], "spaceIds": ["s2"]},
            ]}
        });
        let d = direct_peers(&v);
        assert_eq!(d["s1"], DirectPeers { lan: 1, p2p: 1, own: 1 });
        // s2: only your own device is live — no one else's.
        assert_eq!(d["s2"], DirectPeers { lan: 0, p2p: 0, own: 1 });
    }

    #[test]
    fn emoji_modes() {
        use EmojiMode::*;
        // Old emoji pass; newer ones become ◌ in safe mode.
        assert_eq!(terminal_safe("🌀", Safe), None);
        assert_eq!(terminal_safe("🧉", Safe).as_deref(), Some("◌"));
        assert_eq!(terminal_safe("🪐", Safe).as_deref(), Some("◌"));
        assert_eq!(terminal_safe("🤡", Safe), None);
        // Composed: the base is kept, the rest dropped.
        assert_eq!(terminal_safe("🤦\u{200D}♀\u{FE0F}", Safe).as_deref(), Some("🤦"));
        assert_eq!(terminal_safe("❤\u{FE0F}", Safe).as_deref(), Some("❤"));
        assert_eq!(terminal_safe("👍\u{1F3FD}", Safe).as_deref(), Some("👍"));
        // Off: 2-column text for the common ones, UI markers kept, symbols kept.
        assert_eq!(terminal_safe("👍", Off).as_deref(), Some("+1"));
        assert_eq!(terminal_safe("❤\u{FE0F}", Off).as_deref(), Some("<3"));
        assert_eq!(terminal_safe("🌀", Off).as_deref(), Some("◌"));
        assert_eq!(terminal_safe("🔒", Off), None);
        assert_eq!(terminal_safe("★", Off), None);
        // Full: untouched.
        assert_eq!(terminal_safe("🧉", Full), None);
        assert_eq!(terminal_safe("a", Safe), None);
    }

    #[test]
    fn icons() {
        assert_eq!(icon_glyph("🧉").as_deref(), Some("🧉"));
        assert_eq!(icon_glyph(" "), None);
        assert_eq!(
            icon_glyph(r#"icon:v2:{"assignment":{"kind":"emoji","grapheme":"🎨"}}"#).as_deref(),
            Some("🎨")
        );
        assert_eq!(
            icon_glyph(r#"icon:v2:{"assignment":{"kind":"pack","ref":{"packId":"io.anyproto.iconoir","glyphId":"Home"}},"color":"blue"}"#).as_deref(),
            Some("⌂")
        );
        assert_eq!(
            icon_glyph(r#"icon:v2:{"assignment":{"kind":"pack","ref":{"packId":"io.anyproto.lucide","glyphId":"a-arrow-down"}}}"#).as_deref(),
            Some("↓")
        );
        let pack = |g: &str| icon_glyph(&format!(r#"icon:v2:{{"assignment":{{"kind":"pack","ref":{{"packId":"p","glyphId":"{g}"}}}}}}"#));
        assert_eq!(pack("PlanetSolid").as_deref(), Some("🪐"));
        assert_eq!(pack("AirplaneOff").as_deref(), Some("✈"));
        assert_eq!(pack("InputOutput").as_deref(), Some("⇄"));
        assert_eq!(pack("ArrowUpRightCircleSolid").as_deref(), Some("↗"));
        assert_eq!(pack("SomethingObscure"), None);
        assert_eq!(icon_color(r#"icon:v2:{"assignment":{},"color":"teal"}"#).as_deref(), Some("teal"));
        // Pictures aren't glyphs.
        assert_eq!(icon_glyph("bafybeieiihixnxabsiacbqxkauywlsbqamrsi6s5lw5rpvyhoycvikz43u"), None);
        assert_eq!(icon_glyph("any://f/sp/file1"), None);
    }

    #[test]
    fn whisper_round_trip() {
        let t = whisper_text("↪ Anna: shall we [ship]\nfriday?", "sp.1", "chat1", "m1", "honestly no");
        assert_eq!(t, "[↪ Anna: shall we ship friday?](any://o/sp.1/chat1/chat_messages/m1) honestly no");
        let w = parse_whisper(&t).unwrap();
        assert_eq!((w.space_id.as_str(), w.chat_id.as_str(), w.msg_id.as_str()), ("sp.1", "chat1", "m1"));
        assert_eq!(w.body, "honestly no");
        assert_eq!(w.label, "↪ Anna: shall we ship friday?");
        // A mention link or an object link is not a whisper.
        assert!(parse_whisper("[Anna](any://m/sp/A1) hi").is_none());
        assert!(parse_whisper("[x](any://o/sp/obj) hi").is_none());
        assert!(parse_whisper("plain").is_none());
    }

    #[test]
    fn urls_in_text() {
        assert_eq!(
            extract_urls("see https://a.io/x, and (https://b.io/y). [t](https://c.io/z)"),
            vec!["https://a.io/x", "https://b.io/y", "https://c.io/z"]
        );
        assert_eq!(
            extract_urls("https://en.wikipedia.org/wiki/Rust_(language)!"),
            vec!["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(extract_urls("http:// nothing https://"), Vec::<String>::new());
        assert_eq!(extract_urls("<http://x.io/a>"), vec!["http://x.io/a"]);
    }

    #[test]
    fn highlight_whole_words_any_case() {
        let w = vec!["rust".to_string()];
        assert_eq!(highlight_hits("Rust and rustic, RUST.", &w), vec!["Rust", "RUST"]);
        assert!(highlight_hits("trust", &w).is_empty());
        assert_eq!(find_ci("Trust me", &w, false), vec!["rust"]);
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

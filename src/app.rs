use crate::api::{Api, FileInfo, SearchResults};
use crate::files;
use crate::commands::{self, Command};
use crate::model::{
    AttachmentTarget, BaoBeat, BaoPresence, Chat, DirectPeers, Devices, Identity, Liveness, Message,
    Space, SyncStatus, derive_bao_presence, direct_peers, peer_liveness,
    extract_urls, highlight_hits, icon_color, icon_glyph, parse_whisper, whisper_text,
    link_mentions, md_unescape, render_mentions,
};
use crate::prefs::{self, HISTORY_MAX, Prefs};
use crate::sse::{Frame, SseReader};
use tui_input::Input;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

/// How many messages the live window holds, and the history page size.
const WINDOW: usize = 150;
const PAGE: usize = 100;

/// How long a chat must stay open before auto-read fires. Long enough that
/// cursoring through the list doesn't clear unread, short enough to be
/// invisible when you actually stop to read.
const DWELL: Duration = Duration::from_millis(1500);

/// How long to wait after the last keystroke before firing a search, so typing
/// doesn't launch a request per character.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(300);
/// Top-N hits fetched per space. The endpoint has no offset, so this is the
/// whole result set; we re-sort it by time client-side.
const SEARCH_LIMIT: usize = 100;

/// Chat stats (message count, files, links) refresh at most this often
/// while messages arrive.
const STATS_EVERY: Duration = Duration::from_secs(3);
/// Messages scanned for the files/links lists, newest first.
const MEDIA_CAP: usize = 5000;

/// A bao publisher is offline after three missed 10s beats (ADR-025 §2).
const BAO_OFFLINE_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub enum Ev {
    Key(ratatui::crossterm::event::KeyEvent),
    Tick,
    /// A fresh `GET /spaces` listing, fired whenever the space-list
    /// subscription reports a change (a space joined or left on any device).
    Spaces(Vec<Space>),
    ChatsSnapshot { space_id: String, chats: Vec<Chat> },
    ChatUpsert(Chat),
    ChatRemoved { object_id: String },
    MsgSnapshot { chat: String, msgs: Vec<Message> },
    MsgUpsert { chat: String, msgs: Vec<Message> },
    MsgRemoved { chat: String, id: String },
    History { chat: String, msgs: Vec<Message>, exhausted: bool },
    Preview { object_id: String, msg: Option<Message> },
    /// Enriched, time-sorted search results for the query identified by `seq`.
    SearchResults { seq: u64, hits: Vec<SearchHit>, note: String },
    SearchFailed { seq: u64, msg: String },
    /// A `bao.status` presence beat off the account event bus.
    BaoBeat(BaoBeat),
    /// Open this chat object once it shows up (a DM we just set up arrives
    /// through the space-list subscription a moment later).
    OpenChat(String),
    /// The current incoming DM requests.
    PendingDms(Vec<Space>),
    /// Name/mime/size of an attached file, for the 📎 line.
    FileInfo { file_id: String, info: FileInfo },
    /// Bytes so far of a running download (`total` 0 = unknown).
    Download { file_id: String, name: String, got: u64, total: u64 },
    DownloadDone { file_id: String },
    /// The open chat's message count and its messages with files/links.
    ChatStats { chat: String, total: u64, media: Vec<Message> },
    /// The other member of a DM space.
    DmPeer { space_id: String, identity: String },
    /// Whispers about the open chat's messages, from your DMs.
    Whispers { chat: String, notes: Vec<WhisperNote> },
    /// One space's sync status (a GET, or a flip off the live stream).
    Sync(SyncStatus),
    /// The sync stream (re)connected or dropped events: re-read what we show.
    SyncResync,
    /// Direct peers per space from `/debug/p2p`; `None` when the server
    /// doesn't serve it (the sync-status counts are shown then).
    Direct(Option<HashMap<String, DirectPeers>>),
    /// `/devices`: the registry plus each peer's liveness (empty when the
    /// server has no `/debug/p2p`).
    Devices(Devices, HashMap<String, Liveness>),
    /// A fresh identities directory (names show up as profiles decrypt).
    Identities(Vec<Identity>),
    /// `/nick` succeeded: your new name.
    Renamed(String),
    Toast(String),
    Error(String),
}

#[derive(PartialEq, Clone, Copy)]
pub enum Focus {
    Sidebar,
    Messages,
}

#[derive(PartialEq, Clone, Copy)]
pub enum Mode {
    Normal,
    Insert,
}

/// Below this width a sidebar plus a message pane leaves too little room for
/// either, so we show one at a time. Phone-sized tmux panes land here.
pub const NARROW_COLS: u16 = 80;

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Layout {
    /// Single pane when the terminal is narrow, split when it's wide.
    Auto,
    Split,
    Single,
}

impl Layout {
    pub fn is_single(&self, width: u16) -> bool {
        match self {
            Layout::Auto => width < NARROW_COLS,
            Layout::Split => false,
            Layout::Single => true,
        }
    }
}

/// One row of the fuzzy picker. Keyed by object id, not by index into `chats`:
/// an arriving message re-sorts that list by recency, and a stale index would
/// silently open the wrong chat.
pub struct PickItem {
    pub object_id: String,
    /// Indices into the label's chars that matched, for highlighting.
    pub indices: Vec<usize>,
}

/// Centred fuzzy chat picker, in the spirit of helix's buffer/file menus.
pub struct Picker {
    pub query: Input,
    pub sel: usize,
    pub items: Vec<PickItem>,
}

/// How wide the search reaches. The `/search` endpoint is per-space, so `Chat`
/// and `Space` both hit the current space (Chat additionally filters hits to
/// the active chat), while `AllSpaces` fans the request out across every space.
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum SearchScope {
    Chat,
    Space,
    AllSpaces,
}

/// A message being rewritten in the composer (`e`).
pub struct Editing {
    pub id: String,
    /// The draft the edit pushed aside, put back when it's sent or cancelled.
    pub draft: String,
    /// (name, identity) of the mentions the message already had: relinked
    /// on save even when the person isn't in this space's roster.
    pub mentions: Vec<(String, String)>,
}

/// Relevance mode passed to the search engine.
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum SearchMode {
    Hybrid,
    Fts,
    Vector,
}

impl SearchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SearchMode::Hybrid => "hybrid",
            SearchMode::Fts => "fts",
            SearchMode::Vector => "vector",
        }
    }
}

/// How results are listed. Either way the "first" one sits at the bottom,
/// next to the prompt, where the cursor starts (fzf-style).
#[derive(PartialEq, Clone, Copy, Debug)]
pub enum SearchOrder {
    /// Best match first — the engine's fused score, merged across spaces.
    Best,
    /// Newest first, like reading the chat backwards.
    Newest,
}

/// One enriched search result. The raw hit gives only id + text; creator and
/// timestamp are backfilled so the row reads like a real message and can be
/// time-sorted and `from:`-filtered.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub chat_id: String,
    pub msg_id: String,
    pub creator: String,
    /// Agent name when the message was agent-authored (shown instead of the
    /// human sender, matching the message list).
    pub agent: Option<String>,
    pub text: String,
    pub created_at: f64,
    pub score: f64,
}

/// A whisper as shown under the message it's about.
#[derive(Debug, Clone)]
pub struct WhisperNote {
    /// The message it's about, in the open chat.
    pub target: String,
    /// The DM space it lives in (its peer is the other party).
    pub dm_space: String,
    pub creator: String,
    pub body: String,
    pub created_at: f64,
}

/// One entry of the chat's files or links list.
#[derive(Debug, Clone)]
pub struct MediaItem {
    pub msg_id: String,
    pub creator: String,
    pub agent: Option<String>,
    pub created_at: f64,
    /// The carrying message's text (mentions rendered), for the preview line.
    pub text: String,
    pub target: AttachmentTarget,
    /// The link as written (a URL or `any://…`).
    pub link: String,
}

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum GalleryKind {
    Files,
    Links,
}

/// The files / links list for the open chat (`F` / `L`); takes over the
/// message pane like search does.
pub struct Gallery {
    pub kind: GalleryKind,
    pub sel: usize,
    pub scroll: usize,
}

/// The full-text/semantic search view. Takes over the message pane; the query
/// lives where the composer normally sits.
pub struct Search {
    pub query: Input,
    pub scope: SearchScope,
    pub mode: SearchMode,
    pub order: SearchOrder,
    /// Hits as the last response ranked them (best first); `results` is
    /// this in display order.
    ranked: Vec<SearchHit>,
    pub results: Vec<SearchHit>,
    /// Cursor over `results`, keyed by message id (the list re-sorts on each run).
    pub sel: Option<String>,
    pub scroll: usize,
    /// What the last response reports (mode / semantic status), for the status bar.
    pub note: String,
    /// True while a debounced query is in flight.
    pub searching: bool,
}


impl Search {
    /// Adopts a best-first hit list and lays it out for display: the first
    /// in the chosen order goes at the bottom, by the prompt. The cursor
    /// holds its message when it survives, else lands on that first one.
    pub fn set_hits(&mut self, ranked: Vec<SearchHit>) {
        let mut shown = ranked.clone();
        match self.order {
            SearchOrder::Best => shown.reverse(),
            SearchOrder::Newest => shown.sort_by(|a, b| {
                a.created_at.total_cmp(&b.created_at).then(a.msg_id.cmp(&b.msg_id))
            }),
        }
        self.ranked = ranked;
        self.sel = self
            .sel
            .take()
            .filter(|id| shown.iter().any(|h| &h.msg_id == id))
            .or_else(|| shown.last().map(|h| h.msg_id.clone()));
        self.results = shown;
        self.scroll = 0;
    }
}

pub struct App {
    pub api: Api,
    pub tx: UnboundedSender<Ev>,
    pub me: String,
    pub version: String,
    pub names: HashMap<String, String>,
    /// The identities directory, kept whole for the per-space `@` roster.
    pub identities: Vec<Identity>,
    pub spaces: Vec<Space>,
    /// One chat-objects subscription per space, keyed by space id, so a space
    /// that appears or disappears at runtime can be wired up or torn down.
    chat_subs: HashMap<String, JoinHandle<()>>,
    pub chats: Vec<Chat>,
    pub sel: usize,
    /// True once the user has moved the cursor themselves.
    pub user_selected: bool,
    pub active: Option<String>,
    /// When the current chat became active, for the auto-read dwell check.
    pub active_since: Instant,
    pub msgs: Vec<Message>,
    /// The message under the cursor. Reply targets this, and it's drawn
    /// highlighted so you can see what you're replying to.
    pub sel_msg: Option<String>,
    pub focus: Focus,
    pub mode: Mode,
    pub layout: Layout,
    /// `z` hides the chat list outright, giving the messages the full width.
    /// Independent of `layout`, which only decides how the two panes share the
    /// screen when the list *is* shown.
    pub sidebar_hidden: bool,
    /// Whether the last frame actually rendered as a single pane; `z` flips
    /// relative to what's on screen, which Auto only knows at draw time.
    pub single_now: bool,
    pub input: Input,
    /// Lines scrolled up from the bottom. 0 == pinned to newest.
    pub scroll: usize,
    pub reply_to: Option<String>,
    /// `e`: the message (yours) the composer is rewriting.
    pub editing: Option<Editing>,
    pub toast: Option<(String, Instant)>,
    pub picker: Option<Picker>,
    pub search: Option<Search>,
    /// Bumped per keystroke; a returning result whose gen is stale is dropped.
    pub search_gen: u64,
    search_task: Option<JoinHandle<()>>,
    /// Message id we want the cursor on once its chat's history reaches it.
    pending_jump: Option<String>,
    pub show_help: bool,
    /// Lines the help overlay is scrolled down (clamped when drawn).
    pub help_scroll: usize,
    /// Set to repaint the whole screen on the next frame (Ctrl-L, or after
    /// the emoji mode changes what's on it).
    pub clear_screen: bool,
    pub auto_read: bool,
    pub quit: bool,
    pub loading: bool,
    pub exhausted: bool,
    /// Total wrapped lines of the message pane, set during render so that
    /// scrolling can be clamped correctly.
    pub view_lines: usize,
    pub view_height: usize,
    /// Where each message sits in the rendered pane: `(id, first line, end
    /// line exclusive)`, set at draw time. Paging scrolls by lines and picks
    /// the cursor from these.
    pub msg_ranges: Vec<(String, usize, usize)>,
    /// The message a page scroll left the cursor on when none fit the view
    /// whole: the render then keeps the paged position instead of pinning
    /// that message's top, so a long message pages like `less`.
    pub paged_onto: Option<String>,
    /// Rendered height in lines of each search hit, set at draw time: a
    /// "page" is however many of them fill one screen.
    pub search_heights: HashMap<String, usize>,
    pub search_view_h: usize,
    /// Visible rows of the files/links list, the help box and the chat
    /// list, as last drawn — their page sizes.
    pub gallery_view_h: usize,
    pub help_rows: usize,
    pub sidebar_h: usize,
    msg_task: Option<JoinHandle<()>>,
    /// One live preview subscription per chat, keyed by object id so we spawn
    /// each once and can abort it when the chat goes away.
    preview_subs: HashMap<String, JoinHandle<()>>,
    last_read_marked: HashMap<String, String>,
    /// Messages whose unread reactions we've already asked to clear.
    reactions_marked: HashSet<String>,
    /// The latest `bao.status` beat per publisher, with when we received it
    /// (the staleness clock — the beats' own timestamps are another
    /// machine's).
    bao: HashMap<String, (BaoBeat, Instant)>,
    /// Counts 1s ticks; drives the status bar's ellipsis while bao works.
    pub ticks: u64,
    /// Settings kept in the daemon's local store (`prefs.rs`).
    pub prefs: Prefs,
    /// False when the local store was unreachable: nothing persists.
    pub prefs_ok: bool,
    /// Sent composer lines, oldest first, for ↑/↓ recall.
    history: Vec<String>,
    /// Where ↑/↓ is in `history`; `None` = editing a fresh line (`hist_draft`).
    hist_pos: Option<usize>,
    hist_draft: String,
    /// A chat to open as soon as it appears in the list (see `Ev::OpenChat`).
    pending_open: Option<String>,
    /// Incoming DM requests, as last fetched.
    pub pending_dms: Vec<Space>,
    /// Attached files' metadata by file id, fetched once per file as
    /// messages carrying them load.
    pub file_infos: HashMap<String, FileInfo>,
    files_requested: HashSet<String>,
    /// Running downloads by file id: (name, bytes so far, total). Drives the
    /// status bar and the attachment line's percentage.
    pub downloads: HashMap<String, (String, u64, u64)>,
    /// The open chat's total message count, once known.
    pub msg_total: Option<u64>,
    /// Files then links carried by the open chat's messages, newest first.
    pub files: Vec<MediaItem>,
    pub links: Vec<MediaItem>,
    stats_dirty: bool,
    stats_at: Instant,
    pub gallery: Option<Gallery>,
    /// DM space id → the other person's identity. A new 1-1 has no name
    /// until the peer's profile decrypts, so it's labelled by the peer.
    pub dm_peers: HashMap<String, String>,
    /// Whispers about the open chat's messages, oldest first.
    pub whispers: Vec<WhisperNote>,
    /// Profile icons that render in a terminal (emoji, mapped pack glyphs),
    /// by identity. Pictures are left out.
    pub icons: HashMap<String, String>,
    /// The `icon:v2` colour name of those icons, where they have one.
    pub icon_colors: HashMap<String, String>,
    /// Sync status per space id — the status bar shows the open chat's.
    pub sync: HashMap<String, SyncStatus>,
    /// Direct peers per space, own devices apart; `None` until read or when
    /// the server has no `/debug/p2p`.
    pub direct: Option<HashMap<String, DirectPeers>>,
    direct_dirty: bool,
    direct_at: Instant,
    /// The `/devices` overlay, while it's up.
    pub devices_view: Option<(Devices, HashMap<String, Liveness>)>,
}

impl App {
    pub fn new(
        api: Api,
        tx: UnboundedSender<Ev>,
        me: String,
        version: String,
        auto_read: bool,
        layout: Layout,
    ) -> App {
        App {
            api,
            tx,
            me,
            version,
            names: HashMap::new(),
            identities: Vec::new(),
            spaces: Vec::new(),
            chat_subs: HashMap::new(),
            chats: Vec::new(),
            sel: 0,
            user_selected: false,
            active: None,
            active_since: Instant::now(),
            sel_msg: None,
            msgs: Vec::new(),
            focus: Focus::Sidebar,
            mode: Mode::Normal,
            layout,
            sidebar_hidden: false,
            single_now: false,
            input: Input::default(),
            scroll: 0,
            reply_to: None,
            editing: None,
            toast: None,
            picker: None,
            search: None,
            search_gen: 0,
            search_task: None,
            pending_jump: None,
            show_help: false,
            help_scroll: 0,
            clear_screen: false,
            auto_read,
            quit: false,
            loading: false,
            exhausted: false,
            view_lines: 0,
            view_height: 0,
            msg_ranges: Vec::new(),
            paged_onto: None,
            search_heights: HashMap::new(),
            search_view_h: 0,
            gallery_view_h: 0,
            help_rows: 0,
            sidebar_h: 0,
            msg_task: None,
            preview_subs: HashMap::new(),
            last_read_marked: HashMap::new(),
            reactions_marked: HashSet::new(),
            bao: HashMap::new(),
            ticks: 0,
            prefs: Prefs::default(),
            prefs_ok: false,
            history: Vec::new(),
            hist_pos: None,
            hist_draft: String::new(),
            pending_open: None,
            pending_dms: Vec::new(),
            file_infos: HashMap::new(),
            files_requested: HashSet::new(),
            downloads: HashMap::new(),
            msg_total: None,
            files: Vec::new(),
            links: Vec::new(),
            stats_dirty: false,
            stats_at: Instant::now(),
            gallery: None,
            dm_peers: HashMap::new(),
            whispers: Vec::new(),
            icons: HashMap::new(),
            icon_colors: HashMap::new(),
            sync: HashMap::new(),
            direct: None,
            direct_dirty: true,
            direct_at: Instant::now() - Duration::from_secs(60),
            devices_view: None,
        }
    }

    /// Adopts what `prefs::load` read at startup.
    pub fn set_loaded(&mut self, l: prefs::Loaded) {
        self.prefs = l.prefs;
        self.history = l.history;
        self.prefs_ok = l.ok;
    }

    fn save_prefs(&self) {
        if !self.prefs_ok {
            return;
        }
        let (api, tx, p) = (self.api.clone(), self.tx.clone(), self.prefs.clone());
        tokio::spawn(async move {
            if let Err(e) = prefs::save_prefs(&api, &p).await {
                let _ = tx.send(Ev::Error(format!("save prefs: {e}")));
            }
        });
    }

    /// Folds a beat in — last write per publisher wins.
    pub fn apply_bao_beat(&mut self, beat: BaoBeat) {
        self.bao
            .insert(beat.identity.clone(), (beat, Instant::now()));
    }

    /// Bao's presence as of now, for the status bar.
    pub fn bao_presence(&self) -> BaoPresence {
        let now = Instant::now();
        let beats: Vec<(&BaoBeat, bool)> = self
            .bao
            .values()
            .map(|(b, at)| (b, now.duration_since(*at) <= BAO_OFFLINE_AFTER))
            .collect();
        derive_bao_presence(&beats)
    }

    /// Resolves a mention identity to its current display name; `None` when
    /// the directory doesn't know it (the renderer then keeps the snapshot).
    fn mention_name(&self, identity: &str) -> Option<String> {
        self.names.get(identity).filter(|n| !n.is_empty()).cloned()
    }

    /// `@`-able people for a space: named identities the directory places in
    /// that space (or everywhere, when it doesn't say). (name, identity) pairs.
    pub fn roster(&self, space_id: &str) -> Vec<(String, String)> {
        self.identities
            .iter()
            .filter(|i| !i.name.is_empty())
            .filter(|i| i.space_ids.is_empty() || i.space_ids.iter().any(|s| s == space_id))
            .map(|i| (i.name.clone(), i.identity.clone()))
            .collect()
    }

    /// Adopts a fresh space listing: subscribes to chats in spaces we haven't
    /// seen, drops chats and subscriptions of spaces that are gone. Called at
    /// startup and whenever the space-list subscription signals a change.
    pub fn set_spaces(&mut self, spaces: Vec<Space>) {
        let live: HashSet<&str> = spaces.iter().map(|s| s.id.as_str()).collect();
        let gone: Vec<String> = self
            .chat_subs
            .keys()
            .filter(|id| !live.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            if let Some(h) = self.chat_subs.remove(&id) {
                h.abort();
            }
            let chats: Vec<String> = self
                .chats
                .iter()
                .filter(|c| c.space_id == id)
                .map(|c| c.object_id.clone())
                .collect();
            for c in chats {
                if self.active.as_deref() == Some(c.as_str()) {
                    self.active = None;
                    self.msgs.clear();
                    if let Some(t) = self.msg_task.take() {
                        t.abort();
                    }
                }
                self.remove_chat(&c);
            }
        }
        for s in spaces.iter().filter(|s| s.is_dm() && !self.dm_peers.contains_key(&s.id)) {
            let (api, tx, me, id) = (self.api.clone(), self.tx.clone(), self.me.clone(), s.id.clone());
            tokio::spawn(async move {
                if let Ok(members) = api.members(&id).await
                    && let Some(peer) = members.into_iter().find(|m| *m != me)
                {
                    let _ = tx.send(Ev::DmPeer { space_id: id, identity: peer });
                }
            });
        }
        let unseen: Vec<String> = spaces
            .iter()
            .filter(|s| !self.sync.contains_key(&s.id))
            .map(|s| s.id.clone())
            .collect();
        if !unseen.is_empty() {
            self.fetch_sync(unseen);
        }
        for s in &spaces {
            if !self.chat_subs.contains_key(&s.id) {
                let h = spawn_chats_sub(self.api.clone(), s.clone(), self.tx.clone());
                self.chat_subs.insert(s.id.clone(), h);
            }
        }
        self.spaces = spaces;
        self.relabel_chats();
    }

    pub fn display_name(&self, identity: &str) -> String {
        if let Some(n) = self.names.get(identity) {
            if !n.is_empty() {
                return n.clone();
            }
        }
        // Fall back to a short form of the account address.
        crate::model::short_id(identity)
    }

    pub fn active_chat(&self) -> Option<&Chat> {
        let id = self.active.as_ref()?;
        self.chats.iter().find(|c| &c.object_id == id)
    }

    pub fn selected_chat(&self) -> Option<&Chat> {
        self.chats.get(self.sel)
    }

    pub fn total_unread(&self) -> u64 {
        self.chats.iter().map(|c| c.unread).sum()
    }

    /// Chats with unread messages other than the one currently open —
    /// this is what the status bar advertises.
    pub fn other_unread(&self) -> Vec<&Chat> {
        let active = self.active.clone().unwrap_or_default();
        let mut v: Vec<&Chat> = self
            .chats
            .iter()
            .filter(|c| c.unread > 0 && c.object_id != active)
            .collect();
        v.sort_by(|a, b| b.unread.cmp(&a.unread));
        v
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    /// Shows or hides the chat list. Hiding it leaves the messages as the only
    /// pane, so focus has to move there — there is nothing else left to focus.
    pub fn toggle_sidebar(&mut self) {
        self.sidebar_hidden = !self.sidebar_hidden;
        if self.sidebar_hidden {
            self.focus = Focus::Messages;
            self.toast("chats hidden (z to show)");
        } else {
            self.toast("chats shown (z to hide)");
        }
    }

    /// Back out of the message pane to the chat list. On a narrow screen this
    /// is what actually swaps the visible pane; with the list hidden it brings
    /// it back, so Esc always means "return to the chats".
    pub fn back_to_list(&mut self) {
        self.sidebar_hidden = false;
        self.focus = Focus::Sidebar;
    }

    /// A space's icon as a terminal glyph: a DM shows the other person's
    /// profile icon, any other space its own. None with `/icons` off.
    /// Also its `icon:v2` colour name, if any.
    pub fn space_icon(&self, space_id: &str) -> Option<(String, Option<String>)> {
        let space = self.spaces.iter().find(|s| s.id == space_id)?;
        if space.is_dm()
            && let Some(peer) = self.dm_peers.get(space_id)
            && let Some(icon) = self.member_icon(peer)
        {
            return Some((icon, self.icon_colors.get(peer).cloned()));
        }
        let glyph = icon_glyph(&space.icon_cid).filter(|g| self.icon_drawable(g))?;
        Some((glyph, icon_color(&space.icon_cid)))
    }

    /// A member's icon, if it's shown under the `/icons` mode.
    pub fn member_icon(&self, identity: &str) -> Option<String> {
        self.icons.get(identity).filter(|g| self.icon_drawable(g)).cloned()
    }

    /// Icons are decoration: with `/icons off` none show, and in `safe` an
    /// emoji terminals may misdraw is left out rather than drawn as `◌`.
    fn icon_drawable(&self, glyph: &str) -> bool {
        match self.prefs.emoji_mode() {
            crate::model::EmojiMode::Off => false,
            crate::model::EmojiMode::Safe => !glyph.chars().any(crate::model::emoji_is_new),
            crate::model::EmojiMode::Full => true,
        }
    }

    /// What a space is called in the list: its name, or for a DM whose name
    /// hasn't arrived yet, `@` and the peer.
    fn space_label(&self, space_id: &str) -> Option<String> {
        let space = self.spaces.iter().find(|s| s.id == space_id)?;
        if !space.name.is_empty() {
            return Some(space.name.clone());
        }
        let peer = self.dm_peers.get(space_id)?;
        Some(format!("@{}", self.display_name(peer)))
    }

    /// True for the first space chat after the DM group — where the chat
    /// list draws its `---` separator (`sort_chats` puts DMs first).
    pub fn starts_space_group(&self, i: usize) -> bool {
        let is_dm = |c: &Chat| self.spaces.iter().any(|s| s.id == c.space_id && s.is_dm());
        i > 0 && !is_dm(&self.chats[i]) && is_dm(&self.chats[i - 1])
    }

    /// Re-labels every chat from the current spaces (a rename, a DM peer
    /// learned, a name arrived) and re-sorts, since the list is alphabetical.
    pub fn relabel_chats(&mut self) {
        let labels: HashMap<String, String> = self
            .spaces
            .iter()
            .filter_map(|s| self.space_label(&s.id).map(|l| (s.id.clone(), l)))
            .collect();
        for c in &mut self.chats {
            if let Some(l) = labels.get(&c.space_id) {
                c.space_name = l.clone();
            }
        }
        let keep = self.selected_chat().map(|c| c.object_id.clone());
        self.sort_chats();
        self.restore_selection(keep);
    }

    pub fn set_dm_peer(&mut self, space_id: String, identity: String) {
        self.dm_peers.insert(space_id, identity);
        self.relabel_chats();
    }

    pub fn set_identities(&mut self, ids: Vec<Identity>) {
        for id in &ids {
            if !id.name.is_empty() {
                self.names.insert(id.identity.clone(), id.name.clone());
            }
            match icon_glyph(&id.icon_cid) {
                Some(g) => self.icons.insert(id.identity.clone(), g),
                None => self.icons.remove(&id.identity),
            };
            match icon_color(&id.icon_cid) {
                Some(c) => self.icon_colors.insert(id.identity.clone(), c),
                None => self.icon_colors.remove(&id.identity),
            };
        }
        self.identities = ids;
        self.relabel_chats();
    }

    /// Re-reads the identities directory in the background.
    pub fn refresh_identities(&self) {
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            if let Ok(ids) = api.identities().await {
                let _ = tx.send(Ev::Identities(ids));
            }
        });
    }

    // ---- chat list -------------------------------------------------------

    pub fn upsert_chat(&mut self, mut chat: Chat) {
        // The subscription task holds the space as it was when it started;
        // label from the current listing instead (spaces get renamed, and a
        // DM gets its name only once the peer's profile decrypts).
        if let Some(label) = self.space_label(&chat.space_id) {
            chat.space_name = label;
        }
        let keep = self.selected_chat().map(|c| c.object_id.clone());
        match self
            .chats
            .iter_mut()
            .find(|c| c.object_id == chat.object_id)
        {
            Some(existing) => {
                // The objects-subscription record carries no message preview, so
                // keep the one the preview subscription supplied rather than
                // blanking the row.
                chat.last_text = existing.last_text.clone();
                chat.last_creator = existing.last_creator.clone();
                chat.last_agent = existing.last_agent.clone();
                chat.last_at = existing.last_at;
                chat.hl = existing.hl && chat.unread > 0;
                *existing = chat.clone();
            }
            None => self.chats.push(chat.clone()),
        }
        self.sort_chats();
        self.restore_selection(keep);
        self.ensure_preview_sub(&chat);
        if self.pending_open.as_deref() == Some(chat.object_id.as_str()) {
            self.pending_open = None;
            self.open_chat_id(&chat.object_id);
        }
    }

    /// Selects and enters the chat `object_id`, or — if it isn't listed yet —
    /// opens it the moment it arrives.
    pub fn open_chat_id(&mut self, object_id: &str) {
        match self.chats.iter().position(|c| c.object_id == object_id) {
            Some(i) => {
                self.sel = i;
                self.user_selected = true;
                self.open_selected();
            }
            None => self.pending_open = Some(object_id.to_string()),
        }
    }

    /// Keeps each chat's sidebar preview live with its own tiny (window-of-1)
    /// message subscription. The objects subscription only fires on unread
    /// changes, so a message that doesn't move unread — e.g. one you send
    /// yourself from another device — would otherwise never refresh the row.
    fn ensure_preview_sub(&mut self, chat: &Chat) {
        if self.preview_subs.contains_key(&chat.object_id) {
            return; // already running
        }
        let handle = spawn_preview_sub(
            self.api.clone(),
            chat.space_id.clone(),
            chat.object_id.clone(),
            self.tx.clone(),
        );
        self.preview_subs.insert(chat.object_id.clone(), handle);
    }

    /// Refreshes the open chat's sidebar preview straight from the messages we
    /// already hold. Viewing a chat keeps its unread at 0, so the unread-change
    /// trigger never fires for it.
    pub fn refresh_active_preview(&mut self) {
        let Some(id) = self.active.clone() else { return };
        let last = self.msgs.last().cloned();
        self.set_preview(&id, last);
    }

    pub fn set_preview(&mut self, object_id: &str, msg: Option<Message>) {
        let keep = self.selected_chat().map(|c| c.object_id.clone());
        // Mention links read as `@Name` in the sidebar too, not raw markdown.
        let preview = msg
            .as_ref()
            .map(|m| md_unescape(&render_mentions(&m.preview_text(), |id| self.mention_name(id)).0));
        // A whisper about the open chat just landed in a DM: refresh soon.
        if msg.as_ref().and_then(|m| parse_whisper(&m.text)).is_some_and(|w| self.active.as_deref() == Some(w.chat_id.as_str())) {
            self.stats_changed();
        }
        let hl = msg.as_ref().is_some_and(|m| {
            (m.creator != self.me || m.agent.is_some())
                && !highlight_hits(&m.text, &self.prefs.highlights).is_empty()
        });
        if let Some(c) = self.chats.iter_mut().find(|c| c.object_id == object_id) {
            match msg {
                // A bare "…" ping isn't worth previewing; leave the prior one.
                Some(m) if m.is_agent_presence_marker() => {}
                Some(m) => {
                    // Only an unread arrival lights the chat up; your own
                    // view of it (or reading it) never does.
                    if hl && c.unread > 0 && m.unread {
                        c.hl = true;
                    }
                    c.last_text = preview;
                    c.last_creator = m.creator.clone();
                    c.last_agent = m.agent.as_ref().map(|a| a.name.clone());
                    c.last_at = m.created_at;
                }
                None => c.last_text = Some(String::new()),
            }
        }
        self.sort_chats();
        self.restore_selection(keep);
    }

    pub fn remove_chat(&mut self, object_id: &str) {
        let keep = self.selected_chat().map(|c| c.object_id.clone());
        self.chats.retain(|c| c.object_id != object_id);
        if let Some(h) = self.preview_subs.remove(object_id) {
            h.abort();
        }
        self.restore_selection(keep);
    }

    /// Groups chats into DMs, then spaces, each group alphabetical by its
    /// label (case-insensitive), then by most recent activity within each
    /// space, so live chats float to the top.
    fn sort_chats(&mut self) {
        let dms: HashSet<&str> = self.spaces.iter().filter(|s| s.is_dm()).map(|s| s.id.as_str()).collect();
        let key = |c: &Chat| (!dms.contains(c.space_id.as_str()), c.space_name.to_lowercase());
        self.chats.sort_by(|a, b| {
            key(a)
                .cmp(&key(b))
                .then(a.space_id.cmp(&b.space_id))
                .then(b.last_at.total_cmp(&a.last_at))
                .then(a.pos.cmp(&b.pos))
                .then(a.object_id.cmp(&b.object_id))
        });
    }

    fn restore_selection(&mut self, keep: Option<String>) {
        // Until the user moves, keep the cursor on the first row: chats stream
        // in from several spaces at once and would otherwise latch onto
        // whichever subscription happened to answer first.
        if !self.user_selected {
            self.sel = 0;
            return;
        }
        if let Some(id) = keep {
            if let Some(i) = self.chats.iter().position(|c| c.object_id == id) {
                self.sel = i;
                return;
            }
        }
        self.sel = self.sel.min(self.chats.len().saturating_sub(1));
    }

    // ---- messages --------------------------------------------------------

    pub fn mark_read_now(&mut self) {
        let Some(chat) = self.active_chat().cloned() else {
            return;
        };
        let Some(last) = self.msgs.last().cloned() else {
            return;
        };
        self.last_read_marked
            .insert(chat.object_id.clone(), last.id.clone());
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match api.mark_read(&chat.space_id, &chat.object_id, &last.id).await {
                Ok(()) => {
                    let _ = tx.send(Ev::Toast(format!("marked {} read", chat.qualified())));
                }
                Err(e) => {
                    let _ = tx.send(Ev::Error(format!("mark read: {e}")));
                }
            }
        });
    }

    pub fn merge_msgs(&mut self, msgs: Vec<Message>) {
        for m in msgs {
            // Agent run-start "…" pings aren't real messages; keep them out of
            // the list so the cursor never lands on an empty row.
            if m.is_agent_presence_marker() {
                continue;
            }
            self.fetch_file_infos(&m);
            match self.msgs.iter_mut().find(|x| x.id == m.id) {
                Some(existing) => *existing = m,
                None => self.msgs.push(m),
            }
        }
        self.msgs.sort_by(Message::cmp_order);
    }

    /// Looks up name/size for each file attached to `m`, once per file.
    fn fetch_file_infos(&mut self, m: &Message) {
        for a in &m.attachments {
            let AttachmentTarget::File { space_id, file_id } = a.target() else { continue };
            if !self.files_requested.insert(file_id.clone()) {
                continue;
            }
            let (api, tx) = (self.api.clone(), self.tx.clone());
            tokio::spawn(async move {
                // A failure just leaves the generic "📎 image" label.
                if let Ok(info) = api.file_info(&space_id, &file_id).await {
                    let _ = tx.send(Ev::FileInfo { file_id, info });
                }
            });
        }
    }

    /// `o` / `s` on the message under the cursor: every attachment it
    /// carries is opened with the desktop's default app, or saved to the
    /// Downloads folder. Web links open in the browser either way (this
    /// client speaks plain HTTP to the daemon only, so it can't fetch them).
    pub fn attachment_action(&mut self, open: bool) {
        let Some(m) = self.selected_message().cloned() else {
            return self.toast("no message selected");
        };
        if m.attachments.is_empty() {
            return self.toast("no attachments on this message");
        }
        let mut wanted = Vec::new();
        for a in &m.attachments {
            match a.target() {
                AttachmentTarget::File { space_id, file_id } => {
                    let info = self.file_infos.get(&file_id).cloned();
                    wanted.push((space_id, file_id, info));
                }
                AttachmentTarget::Url(url) => {
                    if let Err(e) = files::open_external(&url) {
                        self.toast(format!("open: {e}"));
                    } else {
                        self.toast(format!("opened {url} in the browser"));
                    }
                }
                AttachmentTarget::Object => self.toast("that attachment links an object, not a file"),
                AttachmentTarget::Unknown => self.toast(format!("can't handle {}", a.link)),
            }
        }
        if !wanted.is_empty() {
            self.fetch_files(wanted, open);
        }
    }

    /// Downloads one after another (the status bar shows the running one),
    /// then opens each, or reveals all the saved ones in a single
    /// file-manager window.
    fn fetch_files(&self, wanted: Vec<(String, String, Option<FileInfo>)>, open: bool) {
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            let toast = |m: String| {
                let _ = tx.send(Ev::Toast(m));
            };
            let mut saved = Vec::new();
            for (space_id, file_id, info) in wanted {
                if let Some(dest) = fetch_file(&api, &tx, &space_id, &file_id, info, open).await {
                    saved.push(dest);
                }
            }
            if open || saved.is_empty() {
                return;
            }
            toast(match saved.as_slice() {
                [one] => format!("saved {}", one.display()),
                many => format!("saved {} files to {}", many.len(), files::download_dir().display()),
            });
            // Point at them in the file manager; blocking, so off the runtime.
            let paths = saved.clone();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || files::reveal(&paths)).await {
                toast(format!("saved to {} (couldn't show it: {e})", files::download_dir().display()));
            }
        });
    }

    /// Loads the selected chat without touching focus, so moving the cursor in
    /// the list previews chats in place (and, in single-pane mode, makes the
    /// subsequent Enter instant).
    pub fn activate_selected(&mut self) {
        let Some(chat) = self.selected_chat().cloned() else {
            return;
        };
        if self.active.as_deref() == Some(chat.object_id.as_str()) {
            return;
        }
        if let Some(t) = self.msg_task.take() {
            t.abort();
        }
        self.active = Some(chat.object_id.clone());
        self.active_since = Instant::now();
        self.msgs.clear();
        self.sel_msg = None;
        self.scroll = 0;
        self.reply_to = None;
        self.cancel_edit();
        self.exhausted = false;
        self.loading = true;
        self.gallery = None;
        self.msg_total = None;
        self.files.clear();
        self.links.clear();
        self.whispers.clear();
        self.refresh_stats();
        self.refresh_sync();
        self.msg_task = Some(spawn_messages_sub(
            self.api.clone(),
            chat.space_id.clone(),
            chat.object_id.clone(),
            self.tx.clone(),
        ));
    }

    /// Explicitly enter the chat: load it and move into the message pane.
    pub fn open_selected(&mut self) {
        self.activate_selected();
        if self.active.is_some() {
            self.focus = Focus::Messages;
        }
    }

    /// Moves the selection by `d` and previews the chat it lands on. Focus is
    /// left alone, so this works while reading (Ctrl-n/Ctrl-p) and while
    /// browsing the list alike.
    pub fn select_delta(&mut self, d: isize) {
        if self.chats.is_empty() {
            return;
        }
        let last = self.chats.len() - 1;
        let next = (self.sel as isize + d).clamp(0, last as isize) as usize;
        if next == self.sel && self.active.is_some() {
            return;
        }
        self.sel = next;
        self.user_selected = true;
        self.activate_selected();
    }

    /// Pages backwards through history when the user scrolls near the top.
    pub fn load_more(&mut self) {
        if self.loading || self.exhausted {
            return;
        }
        let Some(chat) = self.active_chat().cloned() else {
            return;
        };
        // Page by `_ver.id` range from the oldest message we hold. A record
        // without one (older build) can't anchor a range, so history ends there.
        let before = match self.msgs.first() {
            Some(m) if m.ver.is_empty() => {
                self.exhausted = true;
                return;
            }
            Some(m) => Some(m.ver.clone()),
            None => None,
        };
        self.loading = true;
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            match api
                .messages(&chat.space_id, &chat.object_id, PAGE, before.as_deref())
                .await
            {
                Ok(msgs) => {
                    let exhausted = msgs.len() < PAGE;
                    let _ = tx.send(Ev::History {
                        chat: chat.object_id,
                        msgs,
                        exhausted,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Ev::Error(format!("history: {e}")));
                }
            }
        });
    }

    /// Marks the chat read once the newest message is actually on screen.
    /// Skipped when `--no-auto-read` is set, and never re-sends for the same id.
    ///
    /// Requires a short dwell: moving the cursor down the list previews each
    /// chat, and browsing past unread chats must not silently clear them.
    /// Read state has no undo in the API, so the bias is towards not marking.
    pub fn maybe_mark_read(&mut self) {
        // Only once the cursor is on the newest message: reading history must
        // not clear unread.
        if !self.auto_read || !self.at_newest() {
            return;
        }
        if self.active_since.elapsed() < DWELL {
            return;
        }
        // In single-pane mode the cursor loads chats that aren't on screen.
        // Never mark those read: nobody has seen them.
        if self.single_now && self.focus != Focus::Messages {
            return;
        }
        let Some(chat) = self.active_chat().cloned() else {
            return;
        };
        if chat.unread == 0 && chat.unread_mentions == 0 && chat.unread_reactions == 0 {
            return;
        }
        let Some(last) = self.msgs.last().cloned() else {
            return;
        };
        // `…/read` cuts at the newest message's `_ver.id`, which never covers a
        // reaction on an older message: those need `…/reactions-read` each.
        // Only the loaded (seen) messages, and only once per message.
        let reacted: Vec<String> = self
            .msgs
            .iter()
            .filter(|m| m.unread_reactions && !self.reactions_marked.contains(&m.id))
            .map(|m| m.id.clone())
            .collect();
        let already = self.last_read_marked.get(&chat.object_id) == Some(&last.id);
        if already && reacted.is_empty() {
            return;
        }
        self.last_read_marked
            .insert(chat.object_id.clone(), last.id.clone());
        self.reactions_marked.extend(reacted.iter().cloned());
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if !already {
                if let Err(e) = api
                    .mark_read(&chat.space_id, &chat.object_id, &last.id)
                    .await
                {
                    let _ = tx.send(Ev::Error(format!("mark read: {e}")));
                }
            }
            for id in reacted {
                if let Err(e) = api
                    .reactions_read(&chat.space_id, &chat.object_id, &id)
                    .await
                {
                    let _ = tx.send(Ev::Error(format!("reactions read: {e}")));
                }
            }
        });
    }

    pub fn send_input(&mut self) {
        let raw = self.input.value().trim().to_string();
        if raw.is_empty() {
            return;
        }
        if self.editing.is_some() {
            return self.submit_edit(raw);
        }
        // IRC-style commands; a refused one keeps the input for fixing.
        let cmd = match commands::parse(&raw) {
            Ok(c) => c,
            Err(msg) => {
                self.toast(msg);
                return;
            }
        };
        // Only posting and editing need an open chat; the rest work anywhere.
        let chat = self.active_chat().cloned();
        if chat.is_none() && matches!(cmd, Command::Send(_) | Command::Edit { .. }) {
            self.toast("no chat open");
            return;
        }
        self.input.reset();
        self.remember(raw);
        match cmd {
            Command::Send(text) => self.send_text(&chat.expect("checked above"), text),
            Command::Edit { from, to, all } => {
                self.edit_last(&chat.expect("checked above"), &from, &to, all)
            }
            Command::Away(mark) => {
                self.toast(format!("away {mark} (this device only for now) — /back to clear"));
                self.prefs.away = Some(mark);
                self.save_prefs();
            }
            Command::Back => {
                self.prefs.away = None;
                self.toast("welcome back");
                self.save_prefs();
            }
            Command::Highlight(None) => {
                let t = if self.prefs.highlights.is_empty() {
                    "no highlight words — /hl <word> adds one".to_string()
                } else {
                    format!("highlights: {}", self.prefs.highlights.join(", "))
                };
                self.toast(t);
            }
            Command::Highlight(Some(w)) => {
                if !self.prefs.highlights.iter().any(|h| h.eq_ignore_ascii_case(&w)) {
                    self.prefs.highlights.push(w.clone());
                    self.save_prefs();
                }
                self.toast(format!("highlighting \"{w}\""));
            }
            Command::Unhighlight(w) => {
                let before = self.prefs.highlights.len();
                self.prefs.highlights.retain(|h| !h.eq_ignore_ascii_case(&w));
                if self.prefs.highlights.len() == before {
                    self.toast(format!("\"{w}\" wasn't highlighted"));
                } else {
                    self.save_prefs();
                    self.toast(format!("stopped highlighting \"{w}\""));
                }
            }
            Command::Join(q) => self.join(&q),
            Command::Compact => self.toggle_compact(),
            Command::Nick(name) if name.is_empty() => {
                let me = self.display_name(&self.me);
                self.toast(format!("you are {me} ({}) — /nick <name> to change", crate::model::short_id(&self.me)));
            }
            Command::Nick(name) => {
                let (api, tx) = (self.api.clone(), self.tx.clone());
                tokio::spawn(async move {
                    match api.rename(&name).await {
                        Ok(()) => {
                            // Applied locally at once: the directory can
                            // still answer with the old name for a moment.
                            let _ = tx.send(Ev::Renamed(name.clone()));
                            let _ = tx.send(Ev::Toast(format!("you're now {name}")));
                        }
                        Err(e) => {
                            let _ = tx.send(Ev::Error(format!("rename: {e}")));
                        }
                    }
                });
            }
            Command::Devices => {
                self.mode = Mode::Normal;
                self.show_devices();
            }
            Command::Icons(arg) => {
                let mode = if arg.is_empty() {
                    self.prefs.emoji_mode().next()
                } else {
                    match crate::model::EmojiMode::parse(&arg) {
                        Some(m) => m,
                        None => return self.toast("usage: /icons [safe|off|full]"),
                    }
                };
                self.prefs.icons = mode.as_str().to_string();
                self.prefs.hide_icons = false;
                self.save_prefs();
                self.clear_screen = true;
                self.toast(match mode {
                    crate::model::EmojiMode::Safe => "icons: safe — newer emoji show as ◌ so no terminal misdraws them",
                    crate::model::EmojiMode::Off => "icons: off — no icons, emoji as text (+1, <3, :D)",
                    crate::model::EmojiMode::Full => "icons: full — every emoji as sent (may misdraw over tmux/mosh)",
                });
            }
            Command::Dm(target) => self.dm(&target, None),
            Command::Msg(rest) => match commands::resolve_person(&rest, &self.people()) {
                Some((_, "")) => self.toast("usage: /msg @name <text>"),
                Some((id, text)) => {
                    let text = text.to_string();
                    self.dm(&id, Some(text));
                }
                None => self.toast(format!("no one called {rest}")),
            },
            Command::Whisper(rest) => self.whisper(&rest),
            Command::Accept(who) => self.accept_dm(&who),
            Command::Help => {
                self.mode = Mode::Normal;
                self.show_help = true;
                self.help_scroll = 0;
                self.toast(commands::HELP);
            }
        }
    }

    /// `C` / `/compact`: IRC-log layout for messages and the chat list.
    pub fn toggle_compact(&mut self) {
        self.prefs.compact = !self.prefs.compact;
        self.save_prefs();
        self.toast(if self.prefs.compact { "compact layout (C to undo)" } else { "grouped layout" });
    }

    fn send_text(&mut self, chat: &Chat, text: String) {
        let reply = self.reply_to.take();
        self.scroll = 0;
        // Jump to the bottom so you see what you just sent land.
        self.select_newest();
        // `@Name` becomes a real mention link — the server derives `mentions`
        // from those, and only those, so plain `@name` text pings nobody.
        let text = link_mentions(&text, &chat.space_id, &self.roster(&chat.space_id));
        let (api, tx, chat) = (self.api.clone(), self.tx.clone(), chat.clone());
        tokio::spawn(async move {
            if let Err(e) = api
                .send(&chat.space_id, &chat.object_id, &text, reply.as_deref())
                .await
            {
                let _ = tx.send(Ev::Error(format!("send: {e}")));
            }
        });
    }

    /// `e`: rewrite the message under the cursor, if it's yours (and not an
    /// agent's). Its text goes into the composer with mention links shown as
    /// `@Name`; Enter sends it back as a `PATCH`.
    pub fn start_edit(&mut self) {
        if !self.can_edit_selected() {
            let msg = if self.selected_message().is_some() {
                "only your own messages can be edited"
            } else {
                "no message selected"
            };
            return self.toast(msg);
        }
        let m = self.selected_message().expect("checked above");
        let id = m.id.clone();
        let mentions = std::cell::RefCell::new(Vec::new());
        let text = render_mentions(&m.text, |i| {
            let name = self.mention_name(i);
            if let Some(n) = &name {
                mentions.borrow_mut().push((n.clone(), i.to_string()));
            }
            name
        })
        .0;
        let draft = self.input.value().to_string();
        self.reply_to = None;
        self.editing = Some(Editing { id, draft, mentions: mentions.into_inner() });
        self.input = Input::new(text);
        self.hist_pos = None;
        self.mode = Mode::Insert;
        self.focus = Focus::Messages;
    }

    /// Whether `e` would edit the message under the cursor: yours, not an agent's.
    pub fn can_edit_selected(&self) -> bool {
        self.selected_message().is_some_and(|m| m.creator == self.me && m.agent.is_none())
    }

    /// Leaves edit mode, restoring the draft the edit pushed aside.
    pub fn cancel_edit(&mut self) {
        if let Some(e) = self.editing.take() {
            self.input = Input::new(e.draft);
        }
    }

    /// Sends the composer as the new text of the message being edited. Taken
    /// verbatim — no command parsing, so a stored `/me …` or `s/…` stays text.
    fn submit_edit(&mut self, text: String) {
        let Some(chat) = self.active_chat().cloned() else {
            return self.toast("no chat open");
        };
        let Some(Editing { id, draft, mentions }) = self.editing.take() else { return };
        let mut roster = self.roster(&chat.space_id);
        roster.extend(mentions);
        let text = link_mentions(&text, &chat.space_id, &roster);
        let unchanged = self.msgs.iter().any(|m| m.id == id && m.text == text);
        self.input = Input::new(draft);
        self.mode = Mode::Normal;
        if unchanged {
            return;
        }
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            if let Err(e) = api.edit_message(&chat.space_id, &chat.object_id, &id, &text).await {
                let _ = tx.send(Ev::Error(format!("edit: {e}")));
            }
        });
    }

    /// `s/from/to/[g]` on your newest (human, not agent) message here. The
    /// match runs on the stored text, mention links included.
    fn edit_last(&mut self, chat: &Chat, from: &str, to: &str, all: bool) {
        let Some(m) = self
            .msgs
            .iter()
            .rev()
            .find(|m| m.creator == self.me && m.agent.is_none())
            .cloned()
        else {
            self.toast("nothing of yours to edit here");
            return;
        };
        if !m.text.contains(from) {
            self.toast(format!("\"{from}\" isn't in your last message"));
            return;
        }
        let text = if all { m.text.replace(from, to) } else { m.text.replacen(from, to, 1) };
        let (api, tx, chat) = (self.api.clone(), self.tx.clone(), chat.clone());
        tokio::spawn(async move {
            if let Err(e) = api.edit_message(&chat.space_id, &chat.object_id, &m.id, &text).await {
                let _ = tx.send(Ev::Error(format!("edit: {e}")));
            }
        });
    }

    /// Keeps a sent line for ↑ recall (skipping an immediate repeat) and
    /// persists the tail.
    fn remember(&mut self, line: String) {
        self.hist_pos = None;
        self.hist_draft.clear();
        if self.history.last() == Some(&line) {
            return;
        }
        self.history.push(line);
        let over = self.history.len().saturating_sub(HISTORY_MAX);
        self.history.drain(..over);
        if self.prefs_ok {
            let (api, lines) = (self.api.clone(), self.history.clone());
            tokio::spawn(async move {
                let _ = prefs::save_history(&api, &lines).await;
            });
        }
    }

    /// ↑ / ↓ in the composer: walk sent lines; stepping past the newest
    /// restores what you were typing.
    pub fn history_step(&mut self, older: bool) {
        if self.history.is_empty() {
            return;
        }
        let next = match (self.hist_pos, older) {
            (None, true) => {
                self.hist_draft = self.input.value().to_string();
                Some(self.history.len() - 1)
            }
            (None, false) => return,
            (Some(i), true) => Some(i.saturating_sub(1)),
            (Some(i), false) if i + 1 < self.history.len() => Some(i + 1),
            (Some(_), false) => None,
        };
        self.hist_pos = next;
        let value = match next {
            Some(i) => self.history[i].clone(),
            None => std::mem::take(&mut self.hist_draft),
        };
        self.input = Input::new(value);
    }

    /// Everyone the directory can name, for `/dm` and `/msg` — not just this
    /// space's members: a DM is exactly how you reach someone elsewhere.
    fn people(&self) -> Vec<(String, String)> {
        self.identities
            .iter()
            .filter(|i| !i.name.is_empty() && i.identity != self.me)
            .map(|i| (i.name.clone(), i.identity.clone()))
            .collect()
    }

    /// `/join <query>`: open the best fuzzy match, as the picker would rank it.
    fn join(&mut self, query: &str) {
        let best = self
            .chats
            .iter()
            .filter_map(|c| crate::fuzzy::fuzzy_match(&self.pick_label(c), query).map(|(s, _)| (s, c)))
            .max_by_key(|(s, _)| *s)
            .map(|(_, c)| c.object_id.clone());
        match best {
            Some(id) => self.open_chat_id(&id),
            None => self.toast(format!("no chat matches \"{query}\"")),
        }
    }

    /// Opens the DM with `target` — `@name`, an identity, or (empty) the
    /// author of the message under the cursor — and switches to it; with
    /// `text` (`/msg`), posts that instead and stays where you are.
    pub fn dm(&mut self, target: &str, text: Option<String>) {
        let identity = if target.trim().is_empty() {
            match self.selected_message() {
                Some(m) if m.creator != self.me => m.creator.clone(),
                Some(_) => return self.toast("that's you — /dm @name, or put ▌ on someone else"),
                None => return self.toast("usage: /dm @name | identity"),
            }
        } else {
            match commands::resolve_person(target, &self.people()) {
                Some((id, "")) => id,
                Some(_) | None => return self.toast(format!("no one called {target}")),
            }
        };
        if identity == self.me {
            return self.toast("can't DM yourself");
        }
        let who = self.display_name(&identity);
        self.toast(format!("opening DM with {who}…"));
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            let res = async {
                let space = api.one_to_one(&identity).await?;
                let chat = api.general_chat(&space).await?;
                if let Some(t) = &text {
                    api.send(&space, &chat, t, None).await?;
                }
                anyhow::Ok(chat)
            }
            .await;
            match (res, text) {
                (Ok(_), Some(_)) => {
                    let _ = tx.send(Ev::Toast(format!("→ {who}: sent")));
                }
                (Ok(chat), None) => {
                    let _ = tx.send(Ev::OpenChat(chat));
                }
                (Err(e), _) => {
                    let _ = tx.send(Ev::Error(format!("dm {who}: {e}")));
                }
            }
        });
    }

    /// `/w @name text`: a private note about the message under the cursor,
    /// sent into the DM with `name` as a link to that message plus the text
    /// (`model::whisper_text`). Both of you then see it under the message.
    fn whisper(&mut self, rest: &str) {
        let Some(chat) = self.active_chat().cloned() else {
            return self.toast("open a chat and put ▌ on the message first");
        };
        let Some(m) = self.selected_message().cloned() else {
            return self.toast("put ▌ on the message to whisper about");
        };
        let (id, text) = match commands::resolve_person(rest, &self.people()) {
            Some((_, "")) => return self.toast("usage: /w @name <text>"),
            Some((id, text)) => (id, text.to_string()),
            None => return self.toast(format!("no one called {rest}")),
        };
        let author = match &m.agent {
            Some(a) => a.name.clone(),
            None => self.display_name(&m.creator),
        };
        let quoted = md_unescape(&render_mentions(&m.text, |i| self.mention_name(i)).0);
        let label = format!("↪ {author}: {quoted}");
        let text = whisper_text(&label, &chat.space_id, &chat.object_id, &m.id, &text);
        self.dm(&id, Some(text));
        // The link index picks the new edge up after a short debounce.
        self.stats_changed();
    }

    /// `W`: start a whisper to the author of the message under the cursor.
    pub fn start_whisper(&mut self) {
        let Some(m) = self.selected_message().cloned() else {
            return self.toast("put ▌ on the message to whisper about");
        };
        let to = if m.creator == self.me || m.agent.is_some() {
            String::from("@")
        } else {
            format!("@{} ", self.display_name(&m.creator))
        };
        self.input = Input::new(format!("/w {to}"));
        self.mode = Mode::Insert;
        self.focus = Focus::Messages;
    }

    /// Re-reads incoming DM requests; says so when a new one shows up.
    pub fn check_pending_dms(&self) {
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            if let Ok(p) = api.pending_dms().await {
                let _ = tx.send(Ev::PendingDms(p));
            }
        });
    }

    pub fn set_pending_dms(&mut self, pending: Vec<Space>) {
        let new: Vec<&Space> = pending
            .iter()
            .filter(|p| !self.pending_dms.iter().any(|o| o.id == p.id))
            .collect();
        if let Some(p) = new.first() {
            let name = if p.name.is_empty() { "someone" } else { p.name.as_str() };
            self.toast(if p.is_dm() {
                format!("DM request from {name} — /accept to open it")
            } else {
                format!("you were added to {name} — /accept to join")
            });
        }
        self.pending_dms = pending;
    }

    /// `/accept [name]`: approve an incoming DM request or space invite (the
    /// only one, or the one whose name matches); a DM opens straight away.
    fn accept_dm(&mut self, who: &str) {
        let who_l = who.trim().trim_start_matches('@').to_lowercase();
        let matches: Vec<&Space> = self
            .pending_dms
            .iter()
            .filter(|p| who_l.is_empty() || p.name.to_lowercase().contains(&who_l))
            .collect();
        let space = match matches.as_slice() {
            [] if self.pending_dms.is_empty() => return self.toast("nothing to accept"),
            [] => return self.toast(format!("no request or invite matching {who}")),
            [one] => (*one).clone(),
            many => {
                let names: Vec<&str> = many.iter().map(|p| p.name.as_str()).collect();
                return self.toast(format!("which one? /accept {}", names.join(" | ")));
            }
        };
        self.pending_dms.retain(|p| p.id != space.id);
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            // A space invite just joins; the space-list stream brings its
            // chats in. A DM is also set up and opened.
            if !space.is_dm() {
                let _ = tx.send(match api.accept_invite(&space.id).await {
                    Ok(()) => Ev::Toast(format!("joined {}", space.name)),
                    Err(e) => Ev::Error(format!("accept {}: {e}", space.name)),
                });
                return;
            }
            let res = async {
                api.accept_dm(&space.id).await?;
                api.general_chat(&space.id).await
            }
            .await;
            let _ = tx.send(match res {
                Ok(chat) => Ev::OpenChat(chat),
                Err(e) => Ev::Error(format!("accept {}: {e}", space.name)),
            });
        });
    }

    /// Tab in the composer: completes the `@prefix` before the cursor against
    /// the space's roster. A unique match completes to `@Name `; several
    /// extend to their common prefix and list the candidates; none toasts.
    pub fn complete_mention(&mut self) {
        let Some(chat) = self.active_chat().cloned() else { return };
        let value = self.input.value().to_string();
        let cursor = self.input.cursor(); // char index
        let chars: Vec<char> = value.chars().collect();
        let head: String = chars[..cursor.min(chars.len())].iter().collect();
        // The token starts at the last `@` that opens a word.
        let Some(at) = head.char_indices().rev().find(|&(i, c)| {
            c == '@' && head[..i].chars().next_back().is_none_or(|p| !(p.is_alphanumeric() || p == '_'))
        }) else {
            self.toast("type @ then a name to mention");
            return;
        };
        let prefix = &head[at.0 + 1..];
        if prefix.contains('\n') {
            return;
        }
        let needle = prefix.to_lowercase();
        let mut cands: Vec<String> = self
            .roster(&chat.space_id)
            .into_iter()
            .map(|(n, _)| n)
            .filter(|n| n.to_lowercase().starts_with(&needle))
            .collect();
        cands.sort();
        cands.dedup();
        let completion = match cands.as_slice() {
            [] => {
                self.toast(format!("no member matches @{prefix}"));
                return;
            }
            [one] => format!("{one} "),
            many => {
                let common = common_prefix_ci(many);
                self.toast(format!("@{}", many.join("  @")));
                if common.chars().count() <= prefix.chars().count() {
                    return;
                }
                common
            }
        };
        let before: String = head[..=at.0].to_string();
        let after: String = chars[cursor.min(chars.len())..].iter().collect();
        let new_cursor = (before.clone() + &completion).chars().count();
        self.input = Input::new(before + &completion + &after).with_cursor(new_cursor);
    }

    // ---- fuzzy picker ----------------------------------------------------

    /// The string the picker matches against and displays, e.g.
    /// "sync team: general".
    pub fn pick_label(&self, chat: &Chat) -> String {
        format!("{}: {}", chat.space_name, chat.label())
    }

    pub fn open_picker(&mut self) {
        if self.chats.is_empty() {
            self.toast("no chats yet");
            return;
        }
        self.picker = Some(Picker {
            query: Input::default(),
            sel: 0,
            items: Vec::new(),
        });
        self.picker_filter();
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    /// Recomputes matches. An empty query lists every chat in sidebar order;
    /// otherwise rows are ranked by fuzzy score.
    pub fn picker_filter(&mut self) {
        let Some(p) = &self.picker else { return };
        let query = p.query.value().to_string();
        let keep = p.items.get(p.sel).map(|i| i.object_id.clone());

        let mut scored: Vec<(i32, PickItem)> = Vec::new();
        for chat in self.chats.iter() {
            let label = self.pick_label(chat);
            if let Some((score, indices)) = crate::fuzzy::fuzzy_match(&label, &query) {
                scored.push((
                    score,
                    PickItem {
                        object_id: chat.object_id.clone(),
                        indices,
                    },
                ));
            }
        }
        if !query.trim().is_empty() {
            // Stable sort keeps sidebar order among equally good matches.
            scored.sort_by(|a, b| b.0.cmp(&a.0));
        }
        let items: Vec<PickItem> = scored.into_iter().map(|(_, i)| i).collect();

        if let Some(p) = &mut self.picker {
            // Hold the highlight on the same chat when possible, so typing
            // doesn't yank the selection out from under you.
            p.sel = keep
                .and_then(|c| items.iter().position(|i| i.object_id == c))
                .unwrap_or(0)
                .min(items.len().saturating_sub(1));
            p.items = items;
        }
    }

    pub fn picker_move(&mut self, d: isize) {
        if let Some(p) = &mut self.picker {
            if p.items.is_empty() {
                return;
            }
            let last = p.items.len() - 1;
            // Wrap: the list is short and cycling is what these menus do.
            p.sel = match (p.sel as isize + d).rem_euclid(p.items.len() as isize) as usize {
                x if x > last => last,
                x => x,
            };
        }
    }

    /// Opens the highlighted chat and dismisses the picker.
    pub fn picker_accept(&mut self) {
        let Some(p) = &self.picker else { return };
        let Some(item) = p.items.get(p.sel) else {
            self.close_picker();
            return;
        };
        // Resolve the id now: the list may have re-sorted since we filtered.
        let Some(idx) = self
            .chats
            .iter()
            .position(|c| c.object_id == item.object_id)
        else {
            self.close_picker();
            return;
        };
        self.sel = idx;
        self.user_selected = true;
        self.close_picker();
        self.open_selected();
    }

    pub fn next_unread(&mut self) {
        if self.chats.is_empty() {
            return;
        }
        let n = self.chats.len();
        for step in 1..=n {
            let i = (self.sel + step) % n;
            if self.chats[i].unread > 0 {
                self.sel = i;
                self.user_selected = true;
                self.open_selected();
                return;
            }
        }
        self.toast("no unread chats");
    }

    // ---- sync status ----------------------------------------------------

    /// Re-reads the open chat's space sync status (a cheap GET).
    pub fn refresh_sync(&self) {
        if let Some(sp) = self.active_chat().map(|c| c.space_id.clone()) {
            self.fetch_sync(vec![sp]);
        }
    }

    /// Re-reads every space's sync status — at startup, on a stream
    /// (re)connect and on `lagged`, since the stream has no snapshot. The
    /// stream itself carries every space's flips in between.
    pub fn refresh_sync_all(&self) {
        self.fetch_sync(self.spaces.iter().map(|s| s.id.clone()).collect());
    }

    fn fetch_sync(&self, spaces: Vec<String>) {
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            let all = futures_util::future::join_all(spaces.iter().map(|sp| api.sync_status(sp))).await;
            for st in all.into_iter().flatten() {
                if tx.send(Ev::Sync(st)).is_err() {
                    return;
                }
            }
        });
    }

    pub fn set_sync(&mut self, st: SyncStatus) {
        if !st.space_id.is_empty() {
            self.sync.insert(st.space_id.clone(), st);
        }
        // Peer counts moved: re-read who those peers are, soon.
        self.direct_dirty = true;
    }

    /// Re-reads `/debug/p2p` when a sync flip asked for it (at most every
    /// 5s) or every 30s regardless — it has no stream of its own.
    pub fn direct_tick(&mut self) {
        let since = self.direct_at.elapsed();
        if !(self.direct_dirty && since >= Duration::from_secs(5) || since >= Duration::from_secs(30)) {
            return;
        }
        self.direct_dirty = false;
        self.direct_at = Instant::now();
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            let d = api.debug_p2p().await.ok().map(|v| direct_peers(&v));
            let _ = tx.send(Ev::Direct(d));
        });
    }

    /// Reads the registry and the p2p layer, then opens the overlay.
    pub fn show_devices(&self) {
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            let (devs, p2p) = tokio::join!(api.devices(), api.debug_p2p());
            match devs {
                Ok(d) => {
                    let live = p2p.map(|v| peer_liveness(&v)).unwrap_or_default();
                    let _ = tx.send(Ev::Devices(d, live));
                }
                Err(e) => {
                    let _ = tx.send(Ev::Error(format!("devices: {e}")));
                }
            }
        });
    }

    /// A space's direct peers, when `/debug/p2p` is available. A space no
    /// connected peer names has none.
    pub fn direct_for(&self, space_id: &str) -> Option<DirectPeers> {
        self.direct.as_ref().map(|d| d.get(space_id).copied().unwrap_or_default())
    }

    /// The open chat's space status, once known.
    pub fn active_sync(&self) -> Option<&SyncStatus> {
        self.active_chat().and_then(|c| self.sync.get(&c.space_id))
    }

    // ---- chat stats, files and links -----------------------------------

    /// Re-reads the open chat's message count and its files/links now.
    pub fn refresh_stats(&mut self) {
        let Some(chat) = self.active_chat().cloned() else { return };
        self.stats_dirty = false;
        self.stats_at = Instant::now();
        let (api, tx) = (self.api.clone(), self.tx.clone());
        tokio::spawn(async move {
            let (total, media) = tokio::join!(
                api.message_count(&chat.space_id, &chat.object_id),
                api.media_messages(&chat.space_id, &chat.object_id, MEDIA_CAP),
            );
            match (total, media) {
                (Ok(total), Ok(media)) => {
                    let _ = tx.send(Ev::ChatStats { chat: chat.object_id.clone(), total, media });
                }
                (Err(e), _) | (_, Err(e)) => {
                    let _ = tx.send(Ev::Error(format!("chat stats: {e}")));
                }
            }
            // Whispers: edges into this chat from DM spaces, whose messages
            // open with the link (a plain link from elsewhere isn't one).
            let Ok(edges) = api.message_backlinks(&chat.space_id, &chat.object_id).await else { return };
            let mut by_chat: HashMap<(String, String), Vec<String>> = HashMap::new();
            for (sp, obj, rec, _) in edges.into_iter().filter(|e| e.0 != chat.space_id) {
                by_chat.entry((sp, obj)).or_default().push(rec);
            }
            let mut notes = Vec::new();
            for ((sp, obj), ids) in by_chat {
                for m in api.messages_by_ids(&sp, &obj, &ids).await.unwrap_or_default() {
                    let Some(w) = parse_whisper(&m.text) else { continue };
                    if w.chat_id != chat.object_id {
                        continue;
                    }
                    notes.push(WhisperNote {
                        target: w.msg_id,
                        dm_space: sp.clone(),
                        creator: m.creator,
                        body: w.body,
                        created_at: m.created_at,
                    });
                }
            }
            notes.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
            let _ = tx.send(Ev::Whispers { chat: chat.object_id, notes });
        });
    }

    /// Only DM spaces carry whispers; an edge from a group space is a link.
    pub fn set_whispers(&mut self, chat: &str, notes: Vec<WhisperNote>) {
        if self.active.as_deref() != Some(chat) {
            return;
        }
        let dms: HashSet<&str> = self.spaces.iter().filter(|s| s.is_dm()).map(|s| s.id.as_str()).collect();
        self.whispers = notes.into_iter().filter(|n| dms.contains(n.dm_space.as_str())).collect();
    }

    /// Enter on a whisper in a DM: open the message it's about.
    pub fn follow_whisper(&mut self) -> bool {
        let Some(w) = self.selected_message().and_then(|m| parse_whisper(&m.text)) else {
            return false;
        };
        if !self.chats.iter().any(|c| c.object_id == w.chat_id) {
            self.toast("that chat isn't in your list");
            return true;
        }
        self.open_chat_id(&w.chat_id);
        self.pending_jump = Some(w.msg_id);
        self.try_resolve_jump();
        true
    }

    /// Something changed in the open chat: refresh the stats soon (the tick
    /// picks it up, at most every `STATS_EVERY`).
    pub fn stats_changed(&mut self) {
        self.stats_dirty = true;
    }

    pub fn stats_tick(&mut self) {
        if self.stats_dirty && self.stats_at.elapsed() >= STATS_EVERY {
            self.refresh_stats();
        }
    }

    pub fn set_chat_stats(&mut self, chat: &str, total: u64, media: Vec<Message>) {
        if self.active.as_deref() != Some(chat) {
            return;
        }
        self.msg_total = Some(total);
        let (mut files, mut links) = (Vec::new(), Vec::new());
        for m in &media {
            self.fetch_file_infos(m);
            let text = md_unescape(&render_mentions(&m.text, |id| self.mention_name(id)).0);
            let item = |target: AttachmentTarget, link: &str| MediaItem {
                msg_id: m.id.clone(),
                creator: m.creator.clone(),
                agent: m.agent.as_ref().map(|a| a.name.clone()),
                created_at: m.created_at,
                text: text.clone(),
                target,
                link: link.to_string(),
            };
            for a in &m.attachments {
                match a.target() {
                    t @ AttachmentTarget::File { .. } => files.push(item(t, &a.link)),
                    t @ (AttachmentTarget::Url(_) | AttachmentTarget::Object) => links.push(item(t, &a.link)),
                    AttachmentTarget::Unknown => {}
                }
            }
            // Web links written in the text; mention links are `any://`, so
            // they never match.
            for u in extract_urls(&m.text) {
                links.push(item(AttachmentTarget::Url(u.clone()), &u));
            }
        }
        self.files = files;
        self.links = links;
        if let Some(kind) = self.gallery.as_ref().map(|g| g.kind) {
            let last = self.gallery_len(kind).saturating_sub(1);
            if let Some(g) = &mut self.gallery {
                g.sel = g.sel.min(last);
            }
        }
    }

    fn gallery_len(&self, kind: GalleryKind) -> usize {
        match kind {
            GalleryKind::Files => self.files.len(),
            GalleryKind::Links => self.links.len(),
        }
    }

    pub fn gallery_items(&self) -> &[MediaItem] {
        match self.gallery.as_ref().map(|g| g.kind) {
            Some(GalleryKind::Files) => &self.files,
            Some(GalleryKind::Links) => &self.links,
            None => &[],
        }
    }

    /// `F` / `L`: the open chat's files or links; pressing it again closes.
    pub fn open_gallery(&mut self, kind: GalleryKind) {
        if self.active.is_none() {
            return self.toast("open a chat first");
        }
        if self.gallery.as_ref().is_some_and(|g| g.kind == kind) {
            self.gallery = None;
            return;
        }
        self.close_search();
        self.gallery = Some(Gallery { kind, sel: 0, scroll: 0 });
        self.focus = Focus::Messages;
    }

    pub fn gallery_switch(&mut self) {
        if let Some(g) = &mut self.gallery {
            g.kind = match g.kind {
                GalleryKind::Files => GalleryKind::Links,
                GalleryKind::Links => GalleryKind::Files,
            };
            g.sel = 0;
            g.scroll = 0;
        }
    }

    pub fn gallery_move(&mut self, d: isize) {
        let Some(kind) = self.gallery.as_ref().map(|g| g.kind) else { return };
        let len = self.gallery_len(kind);
        if let Some(g) = &mut self.gallery {
            g.sel = (g.sel as isize + d).clamp(0, len.saturating_sub(1) as isize) as usize;
        }
    }

    fn gallery_selected(&self) -> Option<MediaItem> {
        let g = self.gallery.as_ref()?;
        self.gallery_items().get(g.sel).cloned()
    }

    /// Enter: close the list and put the message cursor on the item's message.
    pub fn gallery_jump(&mut self) {
        let Some(item) = self.gallery_selected() else { return };
        self.gallery = None;
        self.pending_jump = Some(item.msg_id);
        self.try_resolve_jump();
    }

    /// `o` / `s` on the item under the cursor: files download (then open, or
    /// save and reveal); web links open in the browser.
    pub fn gallery_action(&mut self, open: bool) {
        let Some(item) = self.gallery_selected() else { return };
        match item.target {
            AttachmentTarget::File { space_id, file_id } => {
                let info = self.file_infos.get(&file_id).cloned();
                self.fetch_files(vec![(space_id, file_id, info)], open);
            }
            AttachmentTarget::Url(url) => match files::open_external(&url) {
                Ok(()) => self.toast(format!("opened {url} in the browser")),
                Err(e) => self.toast(format!("open: {e}")),
            },
            AttachmentTarget::Object => self.toast("that links an object — Enter shows the message"),
            AttachmentTarget::Unknown => {}
        }
    }

    // ---- search ----------------------------------------------------------

    /// Turns the message pane into the search view. Anchored on the active
    /// chat, which seeds the default "this chat" scope.
    pub fn open_search(&mut self) {
        if self.active_chat().is_none() {
            self.toast("open a chat to search");
            return;
        }
        self.search = Some(Search {
            query: Input::default(),
            scope: SearchScope::Chat,
            mode: SearchMode::Hybrid,
            order: SearchOrder::Best,
            ranked: Vec::new(),
            results: Vec::new(),
            sel: None,
            scroll: 0,
            note: String::new(),
            searching: false,
        });
        self.focus = Focus::Messages;
    }

    pub fn close_search(&mut self) {
        self.search = None;
        if let Some(t) = self.search_task.take() {
            t.abort();
        }
    }

    /// Tab widens (this chat → space → all spaces), Shift-Tab narrows.
    pub fn search_cycle_scope(&mut self, wider: bool) {
        if let Some(s) = &mut self.search {
            use SearchScope::*;
            s.scope = match (s.scope, wider) {
                (Chat, true) | (AllSpaces, false) => Space,
                (Space, true) => AllSpaces,
                (Space, false) | (AllSpaces, true) => Chat,
                (Chat, false) => AllSpaces,
            };
        }
        self.run_search();
    }

    /// Ctrl-t: hybrid ⇄ fts. Ctrl-g: semantic only (again: back to hybrid).
    pub fn search_set_mode(&mut self, semantic: bool) {
        if let Some(s) = &mut self.search {
            s.mode = match (s.mode, semantic) {
                (SearchMode::Vector, true) => SearchMode::Hybrid,
                (_, true) => SearchMode::Vector,
                (SearchMode::Hybrid, false) => SearchMode::Fts,
                (_, false) => SearchMode::Hybrid,
            };
        }
        self.run_search();
    }

    /// Ctrl-o: best first ⇄ newest first. Re-sorts what's already held.
    pub fn search_toggle_order(&mut self) {
        if let Some(s) = &mut self.search {
            s.order = match s.order {
                SearchOrder::Best => SearchOrder::Newest,
                SearchOrder::Newest => SearchOrder::Best,
            };
            let ranked = s.ranked.clone();
            s.set_hits(ranked);
        }
    }

    /// (Re)launches a debounced search for the current query/scope/mode. Bumps
    /// the generation so a slower earlier request can't overwrite a newer one,
    /// and aborts the previous in-flight task (which also cancels its debounce).
    pub fn run_search(&mut self) {
        let Some(s) = &self.search else { return };
        self.search_gen += 1;
        let seq = self.search_gen;
        if let Some(t) = self.search_task.take() {
            t.abort();
        }

        let raw = s.query.value().to_string();
        let mode = s.mode.as_str().to_string();
        let scope = s.scope;

        // Which space(s) to hit, and whether to narrow to one chat.
        let (spaces, chat_filter): (Vec<String>, Option<String>) = match scope {
            SearchScope::Chat => match self.active_chat() {
                Some(c) => (vec![c.space_id.clone()], Some(c.object_id.clone())),
                None => (vec![], None),
            },
            SearchScope::Space => match self.active_chat() {
                Some(c) => (vec![c.space_id.clone()], None),
                None => (vec![], None),
            },
            SearchScope::AllSpaces => (self.spaces.iter().map(|sp| sp.id.clone()).collect(), None),
        };

        let names = self.names.clone();
        let api = self.api.clone();
        let tx = self.tx.clone();

        if let Some(s) = &mut self.search {
            s.searching = true;
        }

        self.search_task = Some(tokio::spawn(async move {
            tokio::time::sleep(SEARCH_DEBOUNCE).await;
            let (query, from) = parse_from_filter(&raw);
            if query.trim().is_empty() {
                let _ = tx.send(Ev::SearchResults {
                    seq,
                    hits: Vec::new(),
                    note: String::new(),
                });
                return;
            }
            match run_search_task(&api, &spaces, &query, &mode, chat_filter.as_deref(), from.as_deref(), &names)
                .await
            {
                Ok((hits, note)) => {
                    let _ = tx.send(Ev::SearchResults { seq, hits, note });
                }
                Err(e) => {
                    let _ = tx.send(Ev::SearchFailed {
                        seq,
                        msg: format!("search: {e}"),
                    });
                }
            }
        }));
    }

    pub fn search_move(&mut self, d: isize) {
        let Some(s) = &mut self.search else { return };
        if s.results.is_empty() {
            return;
        }
        let cur = s
            .sel
            .as_ref()
            .and_then(|id| s.results.iter().position(|h| &h.msg_id == id))
            .unwrap_or(s.results.len() - 1) as isize;
        let last = s.results.len() as isize - 1;
        let next = (cur + d).clamp(0, last) as usize;
        s.sel = Some(s.results[next].msg_id.clone());
    }

    /// Opens the message under the search cursor in its real chat. `reply` also
    /// drops straight into a reply. Closes the search view (Enter/Ctrl-r are
    /// one-shot).
    pub fn search_accept(&mut self, reply: bool) {
        let Some(s) = &self.search else { return };
        let Some(sel) = s.sel.clone() else { return };
        let Some(hit) = s.results.iter().find(|h| h.msg_id == sel).cloned() else {
            return;
        };
        let Some(idx) = self.chats.iter().position(|c| c.object_id == hit.chat_id) else {
            self.toast("chat not in list");
            return;
        };
        self.close_search();
        self.pending_jump = Some(hit.msg_id.clone());
        self.sel = idx;
        self.user_selected = true;
        // Activates the target chat (clears msgs + reply_to) and shows the pane.
        self.open_selected();
        if reply {
            // A reply only needs the id, so arm it now — it works even before
            // the message pages in; the banner fills in once it loads.
            self.reply_to = Some(hit.msg_id.clone());
            self.mode = Mode::Insert;
        }
        self.try_resolve_jump();
    }

    /// Places the message cursor on a pending jump target, paging history until
    /// the message appears. Called after each message load; a no-op when there's
    /// nothing pending.
    pub fn try_resolve_jump(&mut self) {
        let Some(target) = self.pending_jump.clone() else {
            return;
        };
        if self.active.is_none() {
            self.pending_jump = None;
            return;
        }
        if self.msgs.iter().any(|m| m.id == target) {
            self.sel_msg = Some(target);
            self.focus = Focus::Messages;
            self.pending_jump = None;
            return;
        }
        if self.exhausted {
            // Ran out of history without finding it (pre-index message, or the
            // window never reached it): land on the oldest we have.
            self.toast("message not in loaded history");
            self.select_oldest();
            self.pending_jump = None;
            return;
        }
        // Not loaded yet — pull another page (no-op if one is already in flight;
        // the next load will call us again).
        self.load_more();
    }

    // ---- message cursor --------------------------------------------------

    pub fn sel_msg_idx(&self) -> Option<usize> {
        let id = self.sel_msg.as_ref()?;
        self.msgs.iter().position(|m| &m.id == id)
    }

    pub fn selected_message(&self) -> Option<&Message> {
        self.sel_msg_idx().and_then(|i| self.msgs.get(i))
    }

    /// True when the cursor is on the newest message, i.e. you've seen the
    /// bottom of the chat.
    pub fn at_newest(&self) -> bool {
        match (self.sel_msg_idx(), self.msgs.len()) {
            (Some(i), n) if n > 0 => i + 1 == n,
            (None, 0) => true,
            _ => false,
        }
    }

    pub fn select_newest(&mut self) {
        self.sel_msg = self.msgs.last().map(|m| m.id.clone());
    }

    /// Moves the message cursor by `d`. The viewport follows it at render time,
    /// so there's no separate scroll position to keep in sync.
    pub fn move_msg_cursor(&mut self, d: isize) {
        if self.msgs.is_empty() {
            return;
        }
        let cur = self.sel_msg_idx().unwrap_or(self.msgs.len() - 1) as isize;
        let last = self.msgs.len() as isize - 1;
        self.select_msg_at((cur + d).clamp(0, last) as usize);
    }

    fn select_msg_at(&mut self, next: usize) {
        self.sel_msg = Some(self.msgs[next].id.clone());
        self.paged_onto = None;
        // Near the top of what we've loaded: pull in more history.
        if next < 5 {
            self.load_more();
        }
        if self.at_newest() {
            self.maybe_mark_read();
        }
    }

    /// PgDn / PgUp (Ctrl-v / Alt-v) in the message pane: scroll the view a
    /// screenful less one line of context, like `less`, then put the cursor on
    /// a message inside the new view. (Moving the cursor a screenful of
    /// messages instead only scrolled as far as the cursor poked out — often
    /// half a page.)
    pub fn page_msgs(&mut self, down: bool) {
        self.scroll_msgs(down, self.view_height.saturating_sub(1).max(1));
    }

    /// Ctrl-d / Ctrl-u: half a screen.
    pub fn half_page_msgs(&mut self, down: bool) {
        self.scroll_msgs(down, (self.view_height / 2).max(1));
    }

    fn scroll_msgs(&mut self, down: bool, step: usize) {
        if self.msgs.is_empty() {
            return;
        }
        let (scroll, pick) = page_view(
            &self.msg_ranges,
            self.view_lines,
            self.view_height,
            self.scroll,
            self.sel_msg.as_deref(),
            if down { -(step as isize) } else { step as isize },
        );
        self.scroll = scroll;
        let (h, total) = (self.view_height, self.view_lines);
        let whole = pick.is_some_and(|r| {
            let (_, s, e) = self.msg_ranges[r];
            s >= total.saturating_sub(scroll + h) && e <= total - scroll.min(total)
        });
        let idx = pick
            .and_then(|r| self.msgs.iter().position(|m| m.id == self.msg_ranges[r].0))
            .unwrap_or(if down { self.msgs.len() - 1 } else { 0 });
        self.select_msg_at(idx);
        if !whole {
            self.paged_onto = pick.map(|r| self.msg_ranges[r].0.clone());
        }
    }

    /// A screenful through the search results.
    pub fn page_search(&mut self, down: bool) {
        let Some(s) = &self.search else { return };
        let cur = s
            .sel
            .as_ref()
            .and_then(|id| s.results.iter().position(|h| &h.msg_id == id))
            .unwrap_or(s.results.len().saturating_sub(1));
        let ids: Vec<&str> = if down {
            s.results.iter().skip(cur + 1).map(|h| h.msg_id.as_str()).collect()
        } else {
            s.results[..cur.min(s.results.len())].iter().rev().map(|h| h.msg_id.as_str()).collect()
        };
        let n = page_steps(ids.iter().map(|id| self.search_heights.get(*id).copied().unwrap_or(3)), self.search_view_h);
        self.search_move(if down { n as isize } else { -(n as isize) });
    }

    /// A screenful through the chat list (a chat is one row compact, about
    /// three with its header and preview otherwise).
    pub fn page_chats(&mut self, down: bool) {
        let per = if self.prefs.compact { 1 } else { 3 };
        let n = (self.sidebar_h / per).max(1) as isize;
        self.select_delta(if down { n } else { -n });
    }

    pub fn select_oldest(&mut self) {
        if let Some(m) = self.msgs.first() {
            self.sel_msg = Some(m.id.clone());
        }
        self.load_more();
    }
}

/// Longest case-insensitive common prefix of `names`, in the first name's case.
fn common_prefix_ci(names: &[String]) -> String {
    let Some(first) = names.first() else { return String::new() };
    let mut n = first.chars().count();
    for other in &names[1..] {
        let m = first
            .chars()
            .zip(other.chars())
            .take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
            .count();
        n = n.min(m);
    }
    first.chars().take(n).collect()
}

// ---- search helpers ------------------------------------------------------

/// Splits a `from:@name` (or `from:name`) token out of the query. The search
/// API can't filter by sender, so we strip it and filter client-side. Returns
/// (remaining query, optional name needle).
/// The words of a search query worth highlighting in results: the query
/// minus its `from:` filter, split on whitespace, quotes and operators
/// stripped, single characters dropped.
pub fn search_terms(raw: &str) -> Vec<String> {
    parse_from_filter(raw)
        .0
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| w.chars().count() > 1)
        .collect()
}

fn parse_from_filter(raw: &str) -> (String, Option<String>) {
    let mut from = None;
    let mut rest: Vec<&str> = Vec::new();
    for tok in raw.split_whitespace() {
        if let Some(name) = tok.strip_prefix("from:") {
            let name = name.trim_start_matches('@');
            if !name.is_empty() {
                from = Some(name.to_string());
            }
        } else {
            rest.push(tok);
        }
    }
    (rest.join(" "), from)
}

/// Human-readable summary of what the search engine did, for the status bar.
fn search_note(res: &SearchResults) -> String {
    match res.vector_status.as_str() {
        "used" => format!("{} · semantic", res.mode),
        "unavailable" => format!("{} · semantic offline", res.mode),
        "disabled" => format!("{} · keyword only", res.mode),
        _ => res.mode.clone(),
    }
}

/// Runs the actual search: every space at once (one failing space costs its
/// hits, not the search), narrowed server-side to one chat when asked; then
/// enrich the hits (one query per chat, also concurrent), apply `from:`, and
/// rank best-first by the fused score.
async fn run_search_task(
    api: &Api,
    spaces: &[String],
    query: &str,
    mode: &str,
    chat_filter: Option<&str>,
    from: Option<&str>,
    names: &HashMap<String, String>,
) -> anyhow::Result<(Vec<SearchHit>, String)> {
    let replies = futures_util::future::join_all(spaces.iter().map(|sp| async move {
        (sp.clone(), api.search(sp, query, mode, SEARCH_LIMIT, chat_filter).await)
    }))
    .await;

    // (space, chat) -> (message id, score) of that chat's hits.
    let mut groups: HashMap<(String, String), Vec<(String, f64)>> = HashMap::new();
    let mut note = String::new();
    let mut failed = Vec::new();
    for (sp, res) in replies {
        match res {
            Ok(res) => {
                note = search_note(&res);
                for h in res.hits {
                    groups
                        .entry((sp.clone(), h.object_id))
                        .or_default()
                        .push((h.record_id, h.score));
                }
            }
            Err(e) => failed.push(e),
        }
    }
    // Only when nothing answered is it an error; otherwise say what's missing.
    if failed.len() == spaces.len() {
        if let Some(e) = failed.into_iter().next() {
            return Err(e);
        }
    } else if !failed.is_empty() {
        note = format!("{note} · {} space(s) failed", failed.len());
    }

    let enriched = futures_util::future::join_all(groups.into_iter().map(|((sp, chat), hits)| async move {
        let ids: Vec<String> = hits.iter().map(|(id, _)| id.clone()).collect();
        // A failed enrichment for one chat shouldn't sink the whole search.
        let msgs = api.messages_by_ids(&sp, &chat, &ids).await.unwrap_or_default();
        (chat, hits, msgs)
    }))
    .await;

    let mut rows: Vec<SearchHit> = Vec::new();
    for (chat, hits, msgs) in enriched {
        for m in msgs {
            if m.is_agent_presence_marker() {
                continue;
            }
            let score = hits.iter().find(|(id, _)| *id == m.id).map_or(0.0, |(_, s)| *s);
            rows.push(SearchHit {
                chat_id: chat.clone(),
                msg_id: m.id,
                creator: m.creator,
                agent: m.agent.map(|a| a.name),
                text: md_unescape(
                    &render_mentions(&parse_whisper(&m.text).map_or(m.text.clone(), |w| format!("🔒 {}", w.body)), |id| {
                        names.get(id).filter(|n| !n.is_empty()).cloned()
                    })
                    .0,
                ),
                created_at: m.created_at,
                score,
            });
        }
    }

    if let Some(from) = from {
        let needle = from.to_lowercase();
        rows.retain(|r| {
            names
                .get(&r.creator)
                .map(|n| n.to_lowercase().contains(&needle))
                .unwrap_or(false)
        });
    }

    // Best first; ties (common in fts) go to the newer message.
    rows.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then(b.created_at.total_cmp(&a.created_at))
            .then(a.msg_id.cmp(&b.msg_id))
    });
    Ok((rows, note))
}

// ---- subscription tasks --------------------------------------------------

/// Live window over one chat's messages. Reconnects with backoff; the caller
/// aborts the task to unsubscribe (the API has no unsubscribe call).
fn spawn_messages_sub(
    api: Api,
    space_id: String,
    object_id: String,
    tx: UnboundedSender<Ev>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            match api.subscribe_messages(&space_id, &object_id, WINDOW).await {
                Ok(reader) => {
                    backoff = 1;
                    if !pump_messages(reader, &object_id, &tx).await {
                        return; // channel closed: UI is gone
                    }
                }
                Err(e) => {
                    let _ = tx.send(Ev::Error(format!("subscribe: {e}")));
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    })
}

/// Returns false if the UI channel closed (caller should stop entirely).
async fn pump_messages(mut reader: SseReader, object_id: &str, tx: &UnboundedSender<Ev>) -> bool {
    loop {
        match reader.next_frame().await {
            Ok(Some(Frame::Snapshot(recs))) => {
                let msgs: Vec<Message> = recs.iter().filter_map(Message::from_record).collect();
                if tx
                    .send(Ev::MsgSnapshot {
                        chat: object_id.to_string(),
                        msgs,
                    })
                    .is_err()
                {
                    return false;
                }
            }
            Ok(Some(Frame::Changes(changes))) => {
                for ch in changes {
                    let msgs: Vec<Message> = ch
                        .added
                        .iter()
                        .chain(ch.updated.iter())
                        .filter_map(Message::from_record)
                        .collect();
                    if !msgs.is_empty()
                        && tx
                            .send(Ev::MsgUpsert {
                                chat: object_id.to_string(),
                                msgs,
                            })
                            .is_err()
                    {
                        return false;
                    }
                    for (id, reason) in ch.removed {
                        // "displaced"/"filtered-out" only mean it left the live
                        // window — the message still exists, so keep it.
                        if reason == "deleted"
                            && tx
                                .send(Ev::MsgRemoved {
                                    chat: object_id.to_string(),
                                    id,
                                })
                                .is_err()
                        {
                            return false;
                        }
                    }
                }
            }
            Ok(Some(Frame::Closed(reason))) => {
                let _ = tx.send(Ev::Error(format!("stream closed: {reason}, reconnecting")));
                return true;
            }
            Ok(Some(_)) => {}
            Ok(None) => return true,
            Err(e) => {
                let _ = tx.send(Ev::Error(format!("stream: {e}")));
                return true;
            }
        }
    }
}

/// Window-of-1 subscription that keeps one chat's sidebar preview live. Cheap:
/// one message in flight at a time. Reconnects with backoff like the others.
fn spawn_preview_sub(
    api: Api,
    space_id: String,
    object_id: String,
    tx: UnboundedSender<Ev>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            match api.subscribe_messages(&space_id, &object_id, 1).await {
                Ok(mut reader) => {
                    backoff = 1;
                    loop {
                        // Reduce each frame to "what is the newest message now",
                        // where None means the chat is empty. Outer None means
                        // this frame carries no preview update. The window holds
                        // exactly one message, so a change that only removes has
                        // emptied it.
                        let update: Option<Option<Message>> = match reader.next_frame().await {
                            Ok(Some(Frame::Snapshot(recs))) => {
                                Some(recs.iter().filter_map(Message::from_record).next_back())
                            }
                            Ok(Some(Frame::Changes(changes))) => {
                                let newest = changes
                                    .iter()
                                    .flat_map(|c| c.added.iter().chain(c.updated.iter()))
                                    .filter_map(Message::from_record)
                                    .max_by(|a, b| a.created_at.total_cmp(&b.created_at));
                                let removed = changes.iter().any(|c| !c.removed.is_empty());
                                match (newest, removed) {
                                    (Some(m), _) => Some(Some(m)),
                                    (None, true) => Some(None), // emptied
                                    (None, false) => None,
                                }
                            }
                            Ok(Some(Frame::Closed(_))) | Ok(None) => break,
                            Ok(Some(_)) => continue,
                            Err(_) => break,
                        };
                        if let Some(msg) = update {
                            if tx
                                .send(Ev::Preview {
                                    object_id: object_id.clone(),
                                    msg,
                                })
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    })
}

/// Watches the account's space list. The rows are raw tech-index records, so
/// rather than mapping them we treat every frame as "something changed" and
/// re-list `/spaces` for the projected shape. Reconnects with backoff.
/// The account event bus, filtered to `bao.status`: the serving anyrt's
/// presence beats feed the status bar. Optional by nature — a daemon without
/// the bus, or an account without a serve, simply never produces a beat, so
/// failures here reconnect quietly and never toast.
pub fn spawn_bao_sub(api: Api, tx: UnboundedSender<Ev>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            if let Ok(mut reader) = api.subscribe_events(&["bao.status"]).await {
                backoff = 1;
                loop {
                    match reader.next_frame().await {
                        Ok(Some(Frame::Event(v))) => {
                            if let Some(beat) = BaoBeat::from_envelope(&v) {
                                if tx.send(Ev::BaoBeat(beat)).is_err() {
                                    return;
                                }
                            }
                        }
                        Ok(Some(Frame::Closed(_))) | Ok(None) | Err(_) => break,
                        Ok(Some(_)) => {}
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    })
}

/// Account-wide sync-status flips. The stream has no snapshot, so each
/// (re)connect and each `lagged` asks the app to re-read what it shows.
pub fn spawn_sync_sub(api: Api, tx: UnboundedSender<Ev>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            if let Ok(mut reader) = api.subscribe_sync_status().await {
                backoff = 1;
                loop {
                    let ev = match reader.next_frame().await {
                        Ok(Some(Frame::Status(v))) => match serde_json::from_value(v) {
                            Ok(st) => Ev::Sync(st),
                            Err(_) => continue,
                        },
                        Ok(Some(Frame::Ready)) | Ok(Some(Frame::Other(_))) => Ev::SyncResync,
                        Ok(Some(Frame::Closed(_))) | Ok(None) | Err(_) => break,
                        Ok(Some(_)) => continue,
                    };
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    })
}

pub fn spawn_spaces_sub(api: Api, tx: UnboundedSender<Ev>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            match api.subscribe_spaces().await {
                Ok(mut reader) => {
                    backoff = 1;
                    loop {
                        match reader.next_frame().await {
                            Ok(Some(Frame::Snapshot(_))) | Ok(Some(Frame::Changes(_))) => {
                                match api.spaces().await {
                                    Ok(spaces) => {
                                        if tx.send(Ev::Spaces(spaces)).is_err() {
                                            return;
                                        }
                                    }
                                    Err(e) => {
                                        let _ = tx.send(Ev::Error(format!("list spaces: {e}")));
                                    }
                                }
                            }
                            Ok(Some(Frame::Closed(_))) | Ok(None) => break,
                            Ok(Some(_)) => {}
                            Err(_) => break,
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Ev::Error(format!("spaces subscribe: {e}")));
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    })
}

/// Live view of one space's chat objects: unread counters and new chats.
/// The chat-declaring type ids are re-resolved on every (re)connect, so a
/// general chat installed while we were subscribed is picked up on reconnect
/// at the latest (and usually live, via the layout leg of the filter).
pub fn spawn_chats_sub(api: Api, space: Space, tx: UnboundedSender<Ev>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            let chat_types = match api.chat_type_ids(&space.id).await {
                Ok(ids) => ids,
                Err(e) => {
                    let _ = tx.send(Ev::Error(format!("chat types: {e}")));
                    Vec::new()
                }
            };
            match api.subscribe_chat_objects(&space.id, &chat_types).await {
                Ok(mut reader) => {
                    backoff = 1;
                    loop {
                        match reader.next_frame().await {
                            Ok(Some(Frame::Snapshot(recs))) => {
                                let chats: Vec<Chat> = recs
                                    .iter()
                                    .filter_map(|r| {
                                        Chat::from_record(r, &space.id, &space.name, &chat_types)
                                    })
                                    .collect();
                                if tx
                                    .send(Ev::ChatsSnapshot {
                                        space_id: space.id.clone(),
                                        chats,
                                    })
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            Ok(Some(Frame::Changes(changes))) => {
                                for ch in changes {
                                    for rec in ch.added.iter().chain(ch.updated.iter()) {
                                        if let Some(c) = Chat::from_record(
                                            rec,
                                            &space.id,
                                            &space.name,
                                            &chat_types,
                                        ) {
                                            if tx.send(Ev::ChatUpsert(c)).is_err() {
                                                return;
                                            }
                                        }
                                    }
                                    for (id, reason) in ch.removed {
                                        if reason == "deleted"
                                            && tx.send(Ev::ChatRemoved { object_id: id }).is_err()
                                        {
                                            return;
                                        }
                                    }
                                }
                            }
                            Ok(Some(Frame::Closed(_))) | Ok(None) => break,
                            Ok(Some(_)) => {}
                            Err(_) => break,
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Ev::Error(format!("chats subscribe: {e}")));
                }
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    })
}

/// Fetches one attached file into the cache (`open`: then opens it, reusing a
/// complete cached copy) or into the Downloads folder under a name that
/// doesn't overwrite anything. Returns where it landed; failures are toasted.
async fn fetch_file(
    api: &Api,
    tx: &UnboundedSender<Ev>,
    space_id: &str,
    file_id: &str,
    info: Option<FileInfo>,
    open: bool,
) -> Option<std::path::PathBuf> {
    let toast = |m: String| {
        let _ = tx.send(Ev::Toast(m));
    };
    // The name is needed for the path (and the opener picks the app by
    // extension), so fetch it if the 📎 lookup hasn't landed.
    let info = match info {
        Some(i) => i,
        None => match api.file_info(space_id, file_id).await {
            Ok(i) => i,
            Err(e) => {
                toast(format!("file {file_id}: {e}"));
                return None;
            }
        },
    };
    let name = files::safe_name(&info.name, file_id);
    let dest = if open {
        files::cache_dir().join(file_id).join(&name)
    } else {
        files::unique_path(&files::download_dir(), &name)
    };
    let cached =
        open && std::fs::metadata(&dest).is_ok_and(|md| info.size == 0 || md.len() == info.size);
    if !cached {
        let progress = |got, total| {
            let _ = tx.send(Ev::Download {
                file_id: file_id.to_string(),
                name: name.clone(),
                got,
                total,
            });
        };
        progress(0, info.size);
        // One event per whole percent (or per ~256 KB when the size is
        // unknown), not per chunk.
        let mut last = 0u64;
        let res = files::download(api, space_id, file_id, &dest, |got, total| {
            let step = (got * 100).checked_div(total).unwrap_or(got >> 18);
            if step != last {
                last = step;
                progress(got, total);
            }
        })
        .await;
        let _ = tx.send(Ev::DownloadDone { file_id: file_id.to_string() });
        if let Err(e) = res {
            let msg = e.to_string();
            toast(if msg.contains("file.not_available") {
                format!("{name}: not available yet — no peer has it and it isn't backed up")
            } else {
                format!("{name}: {msg}")
            });
            return None;
        }
    }
    if open {
        match files::open_external(&dest.to_string_lossy()) {
            Ok(()) => toast(format!("opened {name}")),
            Err(e) => toast(format!("{name}: {e} (it's at {})", dest.display())),
        }
    }
    Some(dest)
}

/// Scrolls a pane of `total` lines, `h` tall, showing `scroll` lines up from
/// the bottom, by `delta` lines (positive = up), and picks the message (index
/// into `ranges`) for the cursor: one wholly inside the new view, as near the
/// old cursor's shifted position as possible, so the render doesn't nudge the
/// view back. At either end the cursor goes to the first / last message.
fn page_view(
    ranges: &[(String, usize, usize)],
    total: usize,
    h: usize,
    scroll: usize,
    cur: Option<&str>,
    delta: isize,
) -> (usize, Option<usize>) {
    if ranges.is_empty() {
        return (scroll, None);
    }
    let max_scroll = total.saturating_sub(h);
    let new = (scroll as isize + delta).clamp(0, max_scroll as isize) as usize;
    if new == scroll {
        return (scroll, Some(if delta > 0 { 0 } else { ranges.len() - 1 }));
    }
    let end = total - new;
    let start = end.saturating_sub(h);
    let moved = new as isize - scroll as isize;
    let target = match cur.and_then(|id| ranges.iter().find(|r| r.0 == id)) {
        Some(r) => (r.1 as isize - moved).max(0) as usize,
        None if delta > 0 => start,
        None => end,
    };
    let inside = ranges
        .iter()
        .enumerate()
        .filter(|(_, r)| r.1 >= start && r.2 <= end)
        .min_by_key(|(_, r)| r.1.abs_diff(target))
        .map(|(i, _)| i);
    // A message taller than the screen: take the one covering the top.
    let edge = if delta > 0 { start } else { end - 1 };
    let pick = inside.or_else(|| ranges.iter().position(|r| r.1 <= edge && edge < r.2));
    (new, pick)
}

/// How many items (with these heights, in the direction of travel) one
/// screen of `screen` lines passes: at least one, and a line of context kept.
fn page_steps(heights: impl Iterator<Item = usize>, screen: usize) -> usize {
    let budget = screen.saturating_sub(1).max(1);
    let mut used = 0;
    let mut n = 0;
    for h in heights {
        if n > 0 && used + h > budget {
            break;
        }
        used += h;
        n += 1;
    }
    n.max(1)
}

#[cfg(test)]
mod tests {
    use super::{page_steps, page_view};

    fn msgs(spans: &[(usize, usize)]) -> Vec<(String, usize, usize)> {
        spans.iter().enumerate().map(|(i, &(s, e))| (format!("m{i}"), s, e)).collect()
    }

    #[test]
    fn page_scrolls_a_screen_less_one_line() {
        // 10 two-line messages, one blank between: 30 lines, 10-line view.
        let r = msgs(&(0..10).map(|i| (i * 3, i * 3 + 2)).collect::<Vec<_>>());
        // At the bottom (scroll 0) on the newest, page up 9 lines.
        let (scroll, pick) = page_view(&r, 30, 10, 0, Some("m9"), 9);
        assert_eq!(scroll, 9);
        // View is lines 11..21; the cursor lands inside it, near 27-9=18.
        let p = &r[pick.unwrap()];
        assert!(p.1 >= 11 && p.2 <= 21);
        assert_eq!(p.0, "m6");
        // Clamped at the top of what's loaded; then the cursor goes to the first.
        assert_eq!(page_view(&r, 30, 10, 18, Some("m1"), 9).0, 20);
        assert_eq!(page_view(&r, 30, 10, 20, Some("m1"), 9), (20, Some(0)));
        // And at the bottom, to the last.
        assert_eq!(page_view(&r, 30, 10, 0, Some("m8"), -9), (0, Some(9)));
    }

    #[test]
    fn page_over_a_tall_message_takes_the_one_covering_the_top() {
        let r = msgs(&[(0, 2), (3, 40), (41, 43)]);
        let (scroll, pick) = page_view(&r, 44, 10, 0, Some("m2"), 9);
        assert_eq!(scroll, 9);
        assert_eq!(pick, Some(1));
    }

    #[test]
    fn pages_fill_a_screen() {
        assert_eq!(page_steps([2, 2, 2, 2, 2].into_iter(), 7), 3);
        // A message taller than the screen still counts as one step.
        assert_eq!(page_steps([40, 2].into_iter(), 10), 1);
        assert_eq!(page_steps(std::iter::empty(), 10), 1);
    }
}

use crate::api::Api;
use crate::model::{Chat, Message, Space};
use crate::sse::{Frame, SseReader};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

/// How many messages the live window holds, and the history page size.
const WINDOW: usize = 150;
const PAGE: usize = 100;

#[derive(Debug)]
pub enum Ev {
    Key(ratatui::crossterm::event::KeyEvent),
    Tick,
    ChatsSnapshot { space_id: String, chats: Vec<Chat> },
    ChatUpsert(Chat),
    ChatRemoved { object_id: String },
    MsgSnapshot { chat: String, msgs: Vec<Message> },
    MsgUpsert { chat: String, msgs: Vec<Message> },
    MsgRemoved { chat: String, id: String },
    History { chat: String, msgs: Vec<Message>, exhausted: bool },
    Preview { object_id: String, msg: Option<Message> },
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

pub struct App {
    pub api: Api,
    pub tx: UnboundedSender<Ev>,
    pub me: String,
    pub version: String,
    pub names: HashMap<String, String>,
    pub spaces: Vec<Space>,
    pub chats: Vec<Chat>,
    pub sel: usize,
    /// True once the user has moved the cursor themselves.
    pub user_selected: bool,
    pub active: Option<String>,
    pub msgs: Vec<Message>,
    pub focus: Focus,
    pub mode: Mode,
    pub layout: Layout,
    /// Whether the last frame actually rendered as a single pane; `z` flips
    /// relative to what's on screen, which Auto only knows at draw time.
    pub single_now: bool,
    pub input: String,
    /// Lines scrolled up from the bottom. 0 == pinned to newest.
    pub scroll: usize,
    pub reply_to: Option<String>,
    pub toast: Option<(String, Instant)>,
    pub show_help: bool,
    pub auto_read: bool,
    pub quit: bool,
    pub loading: bool,
    pub exhausted: bool,
    /// Total wrapped lines of the message pane, set during render so that
    /// scrolling can be clamped correctly.
    pub view_lines: usize,
    pub view_height: usize,
    /// Set when messages were appended at the bottom, so the renderer can hold
    /// the viewport steady for a user who is scrolled up reading history.
    pub pending_append: bool,
    msg_task: Option<JoinHandle<()>>,
    last_read_marked: HashMap<String, String>,
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
            spaces: Vec::new(),
            chats: Vec::new(),
            sel: 0,
            user_selected: false,
            active: None,
            msgs: Vec::new(),
            focus: Focus::Sidebar,
            mode: Mode::Normal,
            layout,
            single_now: false,
            input: String::new(),
            scroll: 0,
            reply_to: None,
            toast: None,
            show_help: false,
            auto_read,
            quit: false,
            loading: false,
            exhausted: false,
            view_lines: 0,
            view_height: 0,
            pending_append: false,
            msg_task: None,
            last_read_marked: HashMap::new(),
        }
    }

    pub fn display_name(&self, identity: &str) -> String {
        if let Some(n) = self.names.get(identity) {
            if !n.is_empty() {
                return n.clone();
            }
        }
        // Fall back to a short form of the account address.
        identity.chars().take(8).collect()
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

    /// Flips between one pane and two, relative to what's currently on screen,
    /// and pins the choice so Auto stops overriding it.
    pub fn toggle_layout(&mut self) {
        self.layout = if self.single_now {
            Layout::Split
        } else {
            Layout::Single
        };
        self.toast(match self.layout {
            Layout::Split => "two panes (z to go back)",
            _ => "one pane (z to go back)",
        });
    }

    /// Back out of the message pane to the chat list. On a narrow screen this
    /// is what actually swaps the visible pane.
    pub fn back_to_list(&mut self) {
        self.focus = Focus::Sidebar;
    }

    // ---- chat list -------------------------------------------------------

    pub fn upsert_chat(&mut self, mut chat: Chat) {
        let keep = self.selected_chat().map(|c| c.object_id.clone());
        let mut need_preview = true;
        match self
            .chats
            .iter_mut()
            .find(|c| c.object_id == chat.object_id)
        {
            Some(existing) => {
                // The subscription record has no message preview, so carry the
                // one we already fetched rather than blanking the row.
                chat.last_text = existing.last_text.clone();
                chat.last_creator = existing.last_creator.clone();
                chat.last_at = existing.last_at;
                // Unread moved => new activity => the preview is stale.
                need_preview =
                    existing.last_text.is_none() || existing.unread != chat.unread;
                *existing = chat.clone();
            }
            None => self.chats.push(chat.clone()),
        }
        self.sort_chats();
        self.restore_selection(keep);
        if need_preview {
            self.fetch_preview(&chat);
        }
    }

    /// Pulls just the newest message of a chat to show as a sidebar preview.
    fn fetch_preview(&self, chat: &Chat) {
        let api = self.api.clone();
        let tx = self.tx.clone();
        let space_id = chat.space_id.clone();
        let object_id = chat.object_id.clone();
        tokio::spawn(async move {
            if let Ok(msgs) = api.messages(&space_id, &object_id, 1, 0).await {
                let _ = tx.send(Ev::Preview {
                    object_id,
                    msg: msgs.into_iter().next_back(),
                });
            }
        });
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
        if let Some(c) = self.chats.iter_mut().find(|c| c.object_id == object_id) {
            match msg {
                Some(m) => {
                    c.last_text = Some(m.text.clone());
                    c.last_creator = m.creator.clone();
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
        self.restore_selection(keep);
    }

    /// Groups chats by the space order the API returned, then by most recent
    /// activity within each space, so live chats float to the top.
    fn sort_chats(&mut self) {
        let order: HashMap<String, usize> = self
            .spaces
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i))
            .collect();
        self.chats.sort_by(|a, b| {
            let sa = order.get(&a.space_id).copied().unwrap_or(usize::MAX);
            let sb = order.get(&b.space_id).copied().unwrap_or(usize::MAX);
            sa.cmp(&sb)
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

    /// Flags whether `incoming` extends the bottom of the timeline (a genuinely
    /// new message) rather than filling in older history.
    pub fn note_append(&mut self, incoming: &[Message]) {
        let newest = self.msgs.last().map(|m| m.created_at).unwrap_or(f64::MIN);
        if incoming.iter().any(|m| {
            m.created_at >= newest && !self.msgs.iter().any(|x| x.id == m.id)
        }) {
            self.pending_append = true;
        }
    }

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
            match self.msgs.iter_mut().find(|x| x.id == m.id) {
                Some(existing) => *existing = m,
                None => self.msgs.push(m),
            }
        }
        self.msgs.sort_by(|a, b| {
            a.created_at
                .partial_cmp(&b.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        });
    }

    pub fn open_selected(&mut self) {
        let Some(chat) = self.selected_chat().cloned() else {
            return;
        };
        if self.active.as_deref() == Some(chat.object_id.as_str()) {
            self.focus = Focus::Messages;
            return;
        }
        if let Some(t) = self.msg_task.take() {
            t.abort();
        }
        self.active = Some(chat.object_id.clone());
        self.msgs.clear();
        self.scroll = 0;
        self.reply_to = None;
        self.exhausted = false;
        self.loading = true;
        self.focus = Focus::Messages;
        self.msg_task = Some(spawn_messages_sub(
            self.api.clone(),
            chat.space_id.clone(),
            chat.object_id.clone(),
            self.tx.clone(),
        ));
    }

    /// Pages backwards through history when the user scrolls near the top.
    pub fn load_more(&mut self) {
        if self.loading || self.exhausted {
            return;
        }
        let Some(chat) = self.active_chat().cloned() else {
            return;
        };
        self.loading = true;
        let api = self.api.clone();
        let tx = self.tx.clone();
        let offset = self.msgs.len();
        tokio::spawn(async move {
            match api
                .messages(&chat.space_id, &chat.object_id, PAGE, offset)
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
    pub fn maybe_mark_read(&mut self) {
        if !self.auto_read || self.scroll != 0 {
            return;
        }
        let Some(chat) = self.active_chat().cloned() else {
            return;
        };
        if chat.unread == 0 && chat.unread_reactions == 0 {
            return;
        }
        let Some(last) = self.msgs.last().cloned() else {
            return;
        };
        if self.last_read_marked.get(&chat.object_id) == Some(&last.id) {
            return;
        }
        self.last_read_marked
            .insert(chat.object_id.clone(), last.id.clone());
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Err(e) = api
                .mark_read(&chat.space_id, &chat.object_id, &last.id)
                .await
            {
                let _ = tx.send(Ev::Error(format!("mark read: {e}")));
            }
        });
    }

    pub fn send_input(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(chat) = self.active_chat().cloned() else {
            self.toast("no chat open");
            return;
        };
        self.input.clear();
        let reply = self.reply_to.take();
        self.scroll = 0;
        let api = self.api.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            if let Err(e) = api
                .send(&chat.space_id, &chat.object_id, &text, reply.as_deref())
                .await
            {
                let _ = tx.send(Ev::Error(format!("send: {e}")));
            }
        });
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

    // ---- scrolling -------------------------------------------------------

    pub fn max_scroll(&self) -> usize {
        self.view_lines.saturating_sub(self.view_height)
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.max_scroll());
        // Near the top: pull in older history.
        if self.scroll + self.view_height + 10 >= self.view_lines {
            self.load_more();
        }
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
        if self.scroll == 0 {
            self.maybe_mark_read();
        }
    }
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

/// Live view of one space's chat objects: unread counters and new chats.
pub fn spawn_chats_sub(api: Api, space: Space, tx: UnboundedSender<Ev>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            match api.subscribe_chat_objects(&space.id).await {
                Ok(mut reader) => {
                    backoff = 1;
                    loop {
                        match reader.next_frame().await {
                            Ok(Some(Frame::Snapshot(recs))) => {
                                let chats: Vec<Chat> = recs
                                    .iter()
                                    .filter_map(|r| Chat::from_record(r, &space.id, &space.name))
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
                                        if let Some(c) =
                                            Chat::from_record(rec, &space.id, &space.name)
                                        {
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

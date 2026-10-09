//! any-tui — a terminal reader for chats in any spaces.
//!
//! Talks exclusively to the local any REST API: REST for reads and writes,
//! SSE subscriptions for live messages and unread counts.

mod api;
mod app;
mod clipboard;
mod commands;
mod edit;
mod files;
mod fuzzy;
mod model;
mod prefs;
mod sse;
mod ui;

use anyhow::{Context, Result};
use app::{App, Ev, Focus, GalleryKind, Mode, spawn_bao_sub, spawn_spaces_sub, spawn_sync_sub};
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

#[derive(Parser)]
#[command(name = "any-tui", version, about = "Read any chats in your terminal")]
struct Args {
    /// Base URL of the any local API.
    #[arg(long, default_value = "http://127.0.0.1:7001/v1")]
    api: String,
    /// Don't mark chats read automatically when you view them.
    #[arg(long)]
    no_auto_read: bool,
    /// Pane layout: auto shows one pane below 80 columns (phone-width tmux),
    /// two above. `z` hides or shows the chat list at runtime.
    #[arg(long, value_enum, default_value_t = LayoutArg::Auto)]
    layout: LayoutArg,
}

#[derive(clap::ValueEnum, Clone, Copy)]
enum LayoutArg {
    Auto,
    Split,
    Single,
}

impl From<LayoutArg> for app::Layout {
    fn from(a: LayoutArg) -> app::Layout {
        match a {
            LayoutArg::Auto => app::Layout::Auto,
            LayoutArg::Split => app::Layout::Split,
            LayoutArg::Single => app::Layout::Single,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let api = api::Api::new(&args.api)?;

    let health = api.health().await.with_context(|| {
        format!(
            "cannot reach the any API at {} — is the daemon running?",
            args.api
        )
    })?;
    // Until an account is authorized every route but /health answers
    // 401 auth.required; say so up front instead of failing on /spaces.
    if health.account.is_empty() {
        anyhow::bail!(
            "the any daemon at {} has no account authorized — see README § Quick start \
             (curl -X POST {}/auth -d '{{}}' generates one; pass a mnemonic to restore)",
            args.api,
            args.api
        );
    }
    let spaces = api.spaces().await.context("list spaces")?;
    let identities = api.identities().await.unwrap_or_default();

    let (tx, rx) = unbounded_channel::<Ev>();
    let mut app = App::new(
        api.clone(),
        tx.clone(),
        health.account.clone(),
        health.version.clone(),
        !args.no_auto_read,
        args.layout.into(),
    );
    // Settings + composer history from the daemon's local store.
    let loaded = prefs::load(&api).await;
    let prefs_ok = loaded.ok;
    app.set_loaded(loaded);
    if !prefs_ok {
        app.toast("local store unavailable — settings won't persist");
    }
    app.set_identities(identities);

    // One live subscription per space keeps unread counts and the chat list
    // fresh; the space-list subscription adds/removes those as spaces come and go.
    app.set_spaces(spaces);
    spawn_spaces_sub(api.clone(), tx.clone());
    // Bao's presence beats, for the status bar.
    spawn_bao_sub(api.clone(), tx.clone());
    // Sync status flips (nodes / LAN / iroh p2p peers), for the status bar.
    spawn_sync_sub(api.clone(), tx.clone());
    app.check_pending_dms();

    // Terminal input runs on its own blocking thread.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                match event::read() {
                    Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                        if tx.send(Ev::Key(k)).is_err() {
                            return;
                        }
                    }
                    // Redraw on resize; other events are ignored.
                    Ok(Event::Resize(_, _)) => {
                        if tx.send(Ev::Tick).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
    }

    // Slow tick so transient toasts expire without a keypress.
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut t = tokio::time::interval(Duration::from_secs(1));
            loop {
                t.tick().await;
                if tx.send(Ev::Tick).is_err() {
                    return;
                }
            }
        });
    }

    let mut terminal = ratatui::init();
    let res = run(&mut terminal, &mut app, rx).await;
    ratatui::restore();
    res
}

async fn run(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mut rx: UnboundedReceiver<Ev>,
) -> Result<()> {
    loop {
        if app.clear_screen {
            app.clear_screen = false;
            terminal.clear()?;
        }
        let mode = app.prefs.emoji_mode();
        terminal.draw(|f| {
            ui::draw(f, app);
            ui::sanitize(f.buffer_mut(), mode);
        })?;
        if app.quit {
            return Ok(());
        }
        // Block for one event, then drain whatever else is queued so a burst
        // of messages costs a single redraw.
        match rx.recv().await {
            Some(ev) => handle(app, ev),
            None => return Ok(()),
        }
        while let Ok(ev) = rx.try_recv() {
            handle(app, ev);
            if app.quit {
                return Ok(());
            }
        }
    }
}

fn handle(app: &mut App, ev: Ev) {
    match ev {
        Ev::Key(k) => on_key(app, k),
        // Auto-read waits for a dwell, so it needs a nudge from the clock
        // rather than only firing on keys and arriving messages.
        Ev::Tick => {
            app.ticks += 1;
            app.maybe_mark_read();
            app.stats_tick();
            app.direct_tick();
            // A pending 1-1 row doesn't always move the space-list stream.
            if app.ticks.is_multiple_of(30) {
                app.check_pending_dms();
                // State-flip streams can drop events; a slow re-read backs them up.
                app.refresh_sync();
            }
        }
        Ev::BaoBeat(b) => app.apply_bao_beat(b),
        Ev::Spaces(spaces) => {
            app.set_spaces(spaces);
            // An incoming DM request lands in the spaces dataset too, and a
            // new space can bring new people (or a DM peer's name).
            app.check_pending_dms();
            app.refresh_identities();
        }
        Ev::DmPeer { space_id, identity } => app.set_dm_peer(space_id, identity),
        Ev::Whispers { chat, notes } => app.set_whispers(&chat, notes),
        Ev::Sync(st) => app.set_sync(st),
        Ev::SyncResync => app.refresh_sync_all(),
        Ev::Direct(d) => app.direct = d,
        Ev::Devices(d, live) => app.devices_view = Some((d, live)),
        Ev::Identities(ids) => app.set_identities(ids),
        Ev::Renamed(name) => {
            let me = app.me.clone();
            app.names.insert(me, name);
            app.relabel_chats();
        }
        Ev::OpenChat(id) => app.open_chat_id(&id),
        Ev::PendingDms(p) => app.set_pending_dms(p),
        Ev::FileInfo { file_id, info } => {
            app.file_infos.insert(file_id, info);
        }
        Ev::Download { file_id, name, got, total } => {
            app.downloads.insert(file_id, (name, got, total));
        }
        Ev::DownloadDone { file_id } => {
            app.downloads.remove(&file_id);
        }
        Ev::ChatStats { chat, total, media } => app.set_chat_stats(&chat, total, media),
        Ev::ChatsSnapshot { space_id, chats } => {
            // Replace this space's chats wholesale, keeping other spaces intact.
            let keep: Vec<String> = chats.iter().map(|c| c.object_id.clone()).collect();
            app.chats
                .retain(|c| c.space_id != space_id || keep.contains(&c.object_id));
            for c in chats {
                app.upsert_chat(c);
            }
        }
        Ev::ChatUpsert(c) => app.upsert_chat(c),
        Ev::ChatRemoved { object_id } => app.remove_chat(&object_id),
        Ev::MsgSnapshot { chat, msgs } | Ev::MsgUpsert { chat, msgs } => {
            if app.active.as_deref() == Some(chat.as_str()) {
                app.loading = false;
                // Follow new arrivals only if already at the bottom; someone
                // reading history keeps their place.
                let follow = app.at_newest();
                app.merge_msgs(msgs);
                if follow {
                    app.select_newest();
                }
                app.refresh_active_preview();
                app.maybe_mark_read();
                app.try_resolve_jump();
                app.stats_changed();
            }
        }
        Ev::MsgRemoved { chat, id } => {
            if app.active.as_deref() == Some(chat.as_str()) {
                let was_sel = app.sel_msg.as_deref() == Some(id.as_str());
                app.msgs.retain(|m| m.id != id);
                if was_sel {
                    app.select_newest();
                }
                app.refresh_active_preview();
                app.stats_changed();
            }
        }
        Ev::History {
            chat,
            msgs,
            exhausted,
        } => {
            if app.active.as_deref() == Some(chat.as_str()) {
                app.loading = false;
                app.exhausted = exhausted;
                app.merge_msgs(msgs);
                app.try_resolve_jump();
            }
        }
        Ev::Preview { object_id, msg } => app.set_preview(&object_id, msg),
        Ev::SearchResults { seq, hits, note } => {
            // Ignore results for a query the user has already typed past.
            if seq == app.search_gen {
                if let Some(s) = &mut app.search {
                    s.searching = false;
                    s.note = note;
                    s.set_hits(hits);
                }
            }
        }
        Ev::SearchFailed { seq, msg } => {
            if seq == app.search_gen {
                if let Some(s) = &mut app.search {
                    s.searching = false;
                    s.note = msg.clone();
                }
                app.toast(msg);
            }
        }
        Ev::Toast(m) => app.toast(m),
        Ev::Error(m) => app.toast(m),
    }
}

fn on_key(app: &mut App, k: KeyEvent) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && matches!(k.code, KeyCode::Char('c')) {
        app.quit = true;
        return;
    }
    // Ctrl-L: repaint everything, the usual cure for a garbled screen.
    if ctrl && matches!(k.code, KeyCode::Char('l')) {
        app.clear_screen = true;
        return;
    }

    // Emacs paging: Ctrl-v / Alt-v are PgDn / PgUp everywhere but the
    // composer (where they're left to the editor).
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let k = match k.code {
        KeyCode::Char('v') if app.mode != Mode::Insert && ctrl => KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        KeyCode::Char('v') if app.mode != Mode::Insert && alt => KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE),
        _ => k,
    };
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);

    // The picker owns the keyboard while it's up.
    if app.picker.is_some() {
        match k.code {
            KeyCode::Esc => app.close_picker(),
            KeyCode::Enter => app.picker_accept(),
            KeyCode::Down => app.picker_move(1),
            KeyCode::Up => app.picker_move(-1),
            KeyCode::Char('n') if ctrl => app.picker_move(1),
            KeyCode::Char('p') if ctrl => app.picker_move(-1),
            KeyCode::Char('j') if ctrl => app.picker_move(1),
            KeyCode::Char('k') if ctrl => app.picker_move(-1),
            KeyCode::PageDown => app.picker_move(10),
            KeyCode::PageUp => app.picker_move(-10),
            // Everything else is line editing on the query.
            _ => {
                let edited = app
                    .picker
                    .as_mut()
                    .map(|p| edit::apply_edit_key(&mut p.query, &k, false))
                    .unwrap_or(false);
                if edited {
                    app.picker_filter();
                }
            }
        }
        return;
    }

    // The search view owns the keyboard while it's up. The query is always
    // live, so navigation and actions are on non-letter / Ctrl keys.
    if app.search.is_some() {
        match k.code {
            KeyCode::Esc => app.close_search(),
            KeyCode::Enter => app.search_accept(false),
            KeyCode::Char('r') if ctrl => app.search_accept(true),
            KeyCode::Tab => app.search_cycle_scope(true),
            KeyCode::BackTab => app.search_cycle_scope(false),
            KeyCode::Char('t') if ctrl => app.search_set_mode(false),
            KeyCode::Char('g') if ctrl => app.search_set_mode(true),
            KeyCode::Char('o') if ctrl => app.search_toggle_order(),
            KeyCode::Down => app.search_move(1),
            KeyCode::Up => app.search_move(-1),
            KeyCode::Char('n') if ctrl => app.search_move(1),
            KeyCode::Char('p') if ctrl => app.search_move(-1),
            KeyCode::PageDown => app.page_search(true),
            KeyCode::PageUp => app.page_search(false),
            // Everything else edits the query and re-runs (debounced).
            _ => {
                let edited = app
                    .search
                    .as_mut()
                    .map(|s| edit::apply_edit_key(&mut s.query, &k, false))
                    .unwrap_or(false);
                if edited {
                    app.run_search();
                }
            }
        }
        return;
    }

    // The files / links list owns the keyboard while it's up.
    if app.gallery.is_some() && app.mode == Mode::Normal {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => app.gallery = None,
            KeyCode::Char('F') => app.open_gallery(GalleryKind::Files),
            KeyCode::Char('L') => app.open_gallery(GalleryKind::Links),
            KeyCode::Tab | KeyCode::BackTab => app.gallery_switch(),
            KeyCode::Char('j') | KeyCode::Down => app.gallery_move(1),
            KeyCode::Char('k') | KeyCode::Up => app.gallery_move(-1),
            KeyCode::Char('d') if ctrl => app.gallery_move(5),
            KeyCode::Char('u') if ctrl => app.gallery_move(-5),
            // Entries are one or two lines; a page is about a screenful.
            KeyCode::PageDown => app.gallery_move((app.gallery_view_h / 2).max(1) as isize),
            KeyCode::PageUp => app.gallery_move(-((app.gallery_view_h / 2).max(1) as isize)),
            KeyCode::Char('g') | KeyCode::Home => app.gallery_move(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => app.gallery_move(isize::MAX / 2),
            KeyCode::Enter => app.gallery_jump(),
            KeyCode::Char('o') => app.gallery_action(true),
            KeyCode::Char('s') => app.gallery_action(false),
            _ => {}
        }
        return;
    }

    // Cycle chats in sidebar order from anywhere, including while reading.
    if ctrl && matches!(k.code, KeyCode::Char('n')) {
        app.select_delta(1);
        return;
    }
    if ctrl && matches!(k.code, KeyCode::Char('p')) {
        app.select_delta(-1);
        return;
    }

    if app.mode == Mode::Insert {
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.reply_to = None;
                app.cancel_edit();
            }
            // Enter sends, so an explicit newline needs its own key. Alt-Enter
            // is the common one; Ctrl-J (handled by the editor) is the
            // terminal-friendly fallback.
            KeyCode::Enter if alt || k.modifiers.contains(KeyModifiers::SHIFT) => {
                app.input.handle(tui_input::InputRequest::InsertChar('\n'));
            }
            KeyCode::Enter => app.send_input(),
            // `@name<Tab>` completes a mention from the space's roster.
            KeyCode::Tab => app.complete_mention(),
            // Recall sent lines (commands included), shell-style.
            KeyCode::Up => app.history_step(true),
            KeyCode::Down => app.history_step(false),
            // Full readline editing: cursor movement, word jumps, kill keys.
            _ => {
                edit::apply_edit_key(&mut app.input, &k, true);
            }
        }
        return;
    }

    // The devices overlay: any of these closes it, `r` re-reads it.
    if app.devices_view.is_some() && app.mode == Mode::Normal {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => app.devices_view = None,
            KeyCode::Char('r') => app.show_devices(),
            _ => {}
        }
        return;
    }

    // While help is up it owns the keyboard: scroll it, or close it. (`q`
    // closes help here — it used to fall through and quit the app.)
    if app.show_help {
        let by = |app: &mut App, d: isize| {
            app.help_scroll = app.help_scroll.saturating_add_signed(d);
        };
        match k.code {
            KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') => app.show_help = false,
            KeyCode::Char('j') | KeyCode::Down => by(app, 1),
            KeyCode::Char('k') | KeyCode::Up => by(app, -1),
            KeyCode::Char('d') if ctrl => by(app, 10),
            KeyCode::Char('u') if ctrl => by(app, -10),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                let n = app.help_rows.saturating_sub(1).max(1) as isize;
                by(app, n)
            }
            KeyCode::PageUp => {
                let n = app.help_rows.saturating_sub(1).max(1) as isize;
                by(app, -n)
            }
            KeyCode::Char('g') | KeyCode::Home => app.help_scroll = 0,
            // Clamped to the real end at draw time.
            KeyCode::Char('G') | KeyCode::End => app.help_scroll = usize::MAX / 2,
            _ => {}
        }
        return;
    }

    match k.code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Char('?') => {
            app.show_help = true;
            app.help_scroll = 0;
        }
        KeyCode::Char('z') => app.toggle_sidebar(),
        KeyCode::Char('C') => app.toggle_compact(),
        KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => {
            if app.show_help {
                app.show_help = false;
            } else if app.reply_to.is_some() {
                app.reply_to = None;
            } else {
                // Primary way back to the chat list when it's the hidden pane.
                app.back_to_list();
            }
        }
        KeyCode::Tab => {
            app.focus = match app.focus {
                Focus::Sidebar => Focus::Messages,
                Focus::Messages => Focus::Sidebar,
            }
        }
        // In the list, moving the cursor previews the chat straight away;
        // Enter is only needed to step into it.
        // In the message pane j/k walk messages rather than lines: the cursor
        // is what `r` replies to, so it has to be a message.
        KeyCode::Char('j') | KeyCode::Down => match app.focus {
            Focus::Sidebar => app.select_delta(1),
            Focus::Messages => app.move_msg_cursor(1),
        },
        KeyCode::Char('k') | KeyCode::Up => match app.focus {
            Focus::Sidebar => app.select_delta(-1),
            Focus::Messages => app.move_msg_cursor(-1),
        },
        KeyCode::Char(' ') => app.open_picker(),
        KeyCode::Char('/') => app.open_search(),
        KeyCode::Enter => {
            // On a whisper in a DM, Enter goes to the message it's about.
            if app.focus == Focus::Messages && app.follow_whisper() {
                return;
            }
            app.user_selected = true;
            app.open_selected();
        }
        // `l` / → mirror `h` / ←: from the list, step into the chat.
        KeyCode::Char('l') | KeyCode::Right if app.focus == Focus::Sidebar => {
            app.user_selected = true;
            app.open_selected();
        }
        KeyCode::Char('n') => app.next_unread(),
        KeyCode::Char('d') if ctrl => app.half_page_msgs(true),
        KeyCode::Char('u') if ctrl => app.half_page_msgs(false),
        KeyCode::PageDown | KeyCode::PageUp => {
            let down = k.code == KeyCode::PageDown;
            match app.focus {
                Focus::Sidebar => app.page_chats(down),
                Focus::Messages => app.page_msgs(down),
            }
        }
        KeyCode::Char('G') | KeyCode::End => {
            app.select_newest();
            app.maybe_mark_read();
        }
        KeyCode::Char('g') | KeyCode::Home => app.select_oldest(),
        KeyCode::Char('i') => {
            // Without an open chat the composer still takes commands (`/dm`,
            // `/join`, `/accept`, …); a plain message is refused on send.
            app.mode = Mode::Insert;
            // The input box lives in the message pane; in single-pane mode
            // it isn't on screen unless we focus it.
            app.focus = Focus::Messages;
            if app.active.is_none() {
                app.toast("no chat open — commands only (/dm, /join, /accept, /help)");
            }
        }
        KeyCode::Char('r') => {
            // Reply to the message under the cursor — the one drawn with the
            // accent bar — not just whatever happens to be newest.
            if let Some(m) = app.selected_message() {
                app.reply_to = Some(m.id.clone());
                app.mode = Mode::Insert;
                app.focus = Focus::Messages;
            } else {
                app.toast("no message selected");
            }
        }
        // Edit the message under the cursor (yours only).
        KeyCode::Char('e') => app.start_edit(),
        // Copy the message under the cursor to the clipboard.
        KeyCode::Char('y') => app.copy_selected(),
        KeyCode::Char('R') => app.mark_read_now(),
        // DM the author of the message under the cursor.
        KeyCode::Char('D') => app.dm("", None),
        // Whisper about the message under the cursor (to its author).
        KeyCode::Char('W') => app.start_whisper(),
        // Attachments of the message under the cursor: open / save.
        KeyCode::Char('o') => app.attachment_action(true),
        KeyCode::Char('s') => app.attachment_action(false),
        // The open chat's files / links, with the message each came in.
        KeyCode::Char('F') => app.open_gallery(GalleryKind::Files),
        KeyCode::Char('L') => app.open_gallery(GalleryKind::Links),
        _ => {}
    }
}

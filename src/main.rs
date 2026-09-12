//! any-tui — a terminal reader for chats in any spaces.
//!
//! Talks exclusively to the local any REST API: REST for reads and writes,
//! SSE subscriptions for live messages and unread counts.

mod api;
mod app;
mod edit;
mod fuzzy;
mod model;
mod sse;
mod ui;

use anyhow::{Context, Result};
use app::{App, Ev, Focus, Mode, spawn_spaces_sub};
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

#[derive(Parser)]
#[command(name = "any-tui", about = "Read any chats in your terminal")]
struct Args {
    /// Base URL of the any local API.
    #[arg(long, default_value = "http://127.0.0.1:7001/v1")]
    api: String,
    /// Don't mark chats read automatically when you view them.
    #[arg(long)]
    no_auto_read: bool,
    /// Pane layout: auto shows one pane below 80 columns (phone-width tmux),
    /// two above. Toggle at runtime with z.
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
            "the any daemon at {} has no account authorized — POST /v1/auth (or `any run` \
             with a data dir that holds one) first",
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
    for id in &identities {
        app.names.insert(id.identity.clone(), id.name.clone());
    }
    app.identities = identities;

    // One live subscription per space keeps unread counts and the chat list
    // fresh; the space-list subscription adds/removes those as spaces come and go.
    app.set_spaces(spaces);
    spawn_spaces_sub(api.clone(), tx.clone());

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
        terminal.draw(|f| ui::draw(f, app))?;
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
        Ev::Tick => app.maybe_mark_read(),
        Ev::Spaces(spaces) => app.set_spaces(spaces),
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
                    let keep = s.sel.clone();
                    s.results = hits;
                    s.note = note;
                    // Hold the cursor on the same message when it survives the
                    // new result set; otherwise fall to the newest.
                    s.sel = keep
                        .filter(|id| s.results.iter().any(|h| &h.msg_id == id))
                        .or_else(|| s.results.last().map(|h| h.msg_id.clone()));
                    s.scroll = 0;
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
        let page: isize = 5;
        match k.code {
            KeyCode::Esc => app.close_search(),
            KeyCode::Enter => app.search_accept(false),
            KeyCode::Char('r') if ctrl => app.search_accept(true),
            KeyCode::Tab | KeyCode::BackTab => app.search_cycle_scope(),
            KeyCode::Char('t') if ctrl => app.search_cycle_mode(),
            KeyCode::Down => app.search_move(1),
            KeyCode::Up => app.search_move(-1),
            KeyCode::Char('n') if ctrl => app.search_move(1),
            KeyCode::Char('p') if ctrl => app.search_move(-1),
            KeyCode::PageDown => app.search_move(page),
            KeyCode::PageUp => app.search_move(-page),
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
            // Full readline editing: cursor movement, word jumps, kill keys.
            _ => {
                edit::apply_edit_key(&mut app.input, &k, true);
            }
        }
        return;
    }

    // While help is up, swallow everything except the keys that dismiss it.
    if app.show_help && !matches!(k.code, KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q')) {
        return;
    }

    // The cursor steps over messages, not lines, so a "page" is a message
    // count rather than a fraction of the pane height.
    let page: isize = 5;
    match k.code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Char('?') => app.show_help = !app.show_help,
        KeyCode::Char('z') => app.toggle_sidebar(),
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
            app.user_selected = true;
            app.open_selected();
        }
        KeyCode::Char('n') => app.next_unread(),
        KeyCode::Char('d') if ctrl => app.move_msg_cursor(page),
        KeyCode::Char('u') if ctrl => app.move_msg_cursor(-page),
        KeyCode::PageDown => app.move_msg_cursor(page),
        KeyCode::PageUp => app.move_msg_cursor(-page),
        KeyCode::Char('G') | KeyCode::End => {
            app.select_newest();
            app.maybe_mark_read();
        }
        KeyCode::Char('g') | KeyCode::Home => app.select_oldest(),
        KeyCode::Char('i') => {
            if app.active.is_some() {
                app.mode = Mode::Insert;
                // The input box lives in the message pane; in single-pane mode
                // it isn't on screen unless we focus it.
                app.focus = Focus::Messages;
            } else {
                app.toast("open a chat first (Enter)");
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
        KeyCode::Char('R') => app.mark_read_now(),
        _ => {}
    }
}

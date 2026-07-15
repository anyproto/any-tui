//! any-tui — a terminal reader for chats in any spaces.
//!
//! Talks exclusively to the local any REST API: REST for reads and writes,
//! SSE subscriptions for live messages and unread counts.

mod api;
mod app;
mod model;
mod sse;
mod ui;

use anyhow::{Context, Result};
use app::{App, Ev, Focus, Mode, spawn_chats_sub};
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
    let spaces = api.spaces().await.context("list spaces")?;
    let identities = api.identities().await.unwrap_or_default();

    let (tx, rx) = unbounded_channel::<Ev>();
    let mut app = App::new(
        api.clone(),
        tx.clone(),
        health.account.clone(),
        health.version.clone(),
        !args.no_auto_read,
    );
    for id in identities {
        app.names.insert(id.identity, id.name);
    }
    app.spaces = spaces.clone();

    // One live subscription per space keeps unread counts and the chat list fresh.
    for space in spaces {
        spawn_chats_sub(api.clone(), space, tx.clone());
    }

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
        Ev::Tick => {}
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
        Ev::MsgSnapshot { chat, msgs } => {
            if app.active.as_deref() == Some(chat.as_str()) {
                app.loading = false;
                app.note_append(&msgs);
                app.merge_msgs(msgs);
                app.refresh_active_preview();
                app.maybe_mark_read();
            }
        }
        Ev::MsgUpsert { chat, msgs } => {
            if app.active.as_deref() == Some(chat.as_str()) {
                app.note_append(&msgs);
                app.merge_msgs(msgs);
                app.refresh_active_preview();
                app.maybe_mark_read();
            }
        }
        Ev::MsgRemoved { chat, id } => {
            if app.active.as_deref() == Some(chat.as_str()) {
                app.msgs.retain(|m| m.id != id);
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
            }
        }
        Ev::Preview { object_id, msg } => app.set_preview(&object_id, msg),
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

    if app.mode == Mode::Insert {
        match k.code {
            KeyCode::Esc => {
                app.mode = Mode::Normal;
                app.reply_to = None;
            }
            KeyCode::Enter => app.send_input(),
            KeyCode::Backspace => {
                app.input.pop();
            }
            KeyCode::Char('u') if ctrl => app.input.clear(),
            KeyCode::Char(c) => app.input.push(c),
            _ => {}
        }
        return;
    }

    // While help is up, swallow everything except the keys that dismiss it.
    if app.show_help && !matches!(k.code, KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q')) {
        return;
    }

    let half = (app.view_height / 2).max(1);
    match k.code {
        KeyCode::Char('q') => app.quit = true,
        KeyCode::Char('?') => app.show_help = !app.show_help,
        KeyCode::Esc => {
            if app.show_help {
                app.show_help = false;
            } else {
                app.reply_to = None;
            }
        }
        KeyCode::Tab => {
            app.focus = match app.focus {
                Focus::Sidebar => Focus::Messages,
                Focus::Messages => Focus::Sidebar,
            }
        }
        KeyCode::Char('j') | KeyCode::Down => match app.focus {
            Focus::Sidebar => {
                if app.sel + 1 < app.chats.len() {
                    app.sel += 1;
                    app.user_selected = true;
                }
            }
            Focus::Messages => app.scroll_down(1),
        },
        KeyCode::Char('k') | KeyCode::Up => match app.focus {
            Focus::Sidebar => {
                app.sel = app.sel.saturating_sub(1);
                app.user_selected = true;
            }
            Focus::Messages => app.scroll_up(1),
        },
        KeyCode::Enter => {
            app.user_selected = true;
            app.open_selected();
        }
        KeyCode::Char('n') => app.next_unread(),
        KeyCode::Char('d') if ctrl => app.scroll_down(half),
        KeyCode::Char('u') if ctrl => app.scroll_up(half),
        KeyCode::PageDown => app.scroll_down(half),
        KeyCode::PageUp => app.scroll_up(half),
        KeyCode::Char('G') | KeyCode::End => app.scroll_down(usize::MAX),
        KeyCode::Char('g') | KeyCode::Home => {
            app.scroll_up(app.max_scroll());
            app.load_more();
        }
        KeyCode::Char('i') => {
            if app.active.is_some() {
                app.mode = Mode::Insert;
            } else {
                app.toast("open a chat first (Enter)");
            }
        }
        KeyCode::Char('r') => {
            if let Some(last) = app.msgs.last() {
                app.reply_to = Some(last.id.clone());
                app.mode = Mode::Insert;
            }
        }
        KeyCode::Char('R') => app.mark_read_now(),
        _ => {}
    }
}

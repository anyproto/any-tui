use crate::app::{App, Focus, Mode, SearchScope};
use crate::model::render_mentions;
use chrono::{DateTime, Local, TimeZone};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

const SIDEBAR_W: u16 = 34;
/// The composer grows with the text, then scrolls instead of eating the chat.
const MAX_INPUT_LINES: usize = 8;

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const UNREAD: Color = Color::Yellow;
/// Selected rows brighten their text instead of taking a background tint.
const SEL: Color = Color::White;
const SEL_UNREAD: Color = Color::LightYellow;
const ME: Color = Color::Green;
const PEER: Color = Color::Magenta;
/// Agent-authored messages get their own hue and a `✦` marker, since they're
/// signed by the human account and would otherwise read as that person.
const AGENT: Color = Color::LightBlue;

/// The author label + colour for a message: the agent's name when
/// agent-authored (never the human that signed it), otherwise the sender.
fn author_label(app: &App, m: &crate::model::Message) -> (String, Color) {
    match &m.agent {
        Some(a) => (format!("✦ {}", a.name), AGENT),
        None => (
            app.display_name(&m.creator),
            if m.creator == app.me { ME } else { PEER },
        ),
    }
}

/// Sender label for a chat's last-message preview: the agent name (with the
/// `✦` marker) when the newest message was agent-authored, else the human.
fn preview_sender(app: &App, chat: &crate::model::Chat) -> String {
    match &chat.last_agent {
        Some(name) => format!("✦ {}", short_name(name)),
        None => short_name(&app.display_name(&chat.last_creator)),
    }
}

/// A stable key identifying who "spoke", for consecutive-message grouping.
/// Agent messages share the human's creator, so grouping must key on the agent
/// name too or an agent reply would fold silently under the preceding human.
fn speaker_key(m: &crate::model::Message) -> String {
    match &m.agent {
        Some(a) => format!("agent:{}", a.name),
        None => m.creator.clone(),
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let root = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());

    // Either the user hid the list with `z`, or the terminal is too narrow to
    // hold both panes. Both end up showing one pane; the status bar still
    // carries unread for every other chat, which is the only cue left then.
    app.single_now = app.sidebar_hidden || app.layout.is_single(f.area().width);
    if app.sidebar_hidden {
        draw_chat(f, app, root[0]);
    } else if app.single_now {
        // One pane at a time: whichever has focus.
        match app.focus {
            Focus::Sidebar => draw_sidebar(f, app, root[0]),
            Focus::Messages => draw_chat(f, app, root[0]),
        }
    } else {
        let cols =
            Layout::horizontal([Constraint::Length(SIDEBAR_W), Constraint::Min(20)]).split(root[0]);
        draw_sidebar(f, app, cols[0]);
        draw_chat(f, app, cols[1]);
    }
    draw_status(f, app, root[1]);

    if app.picker.is_some() {
        draw_picker(f, app, f.area());
    }
    if app.show_help {
        draw_help(f, f.area(), &app.version);
    }
}

fn draw_picker(f: &mut Frame, app: &App, area: Rect) {
    let Some(p) = &app.picker else { return };

    // Fixed size: the box must not resize and re-centre itself on every
    // keystroke as the match count changes.
    let w = 72.min(area.width.saturating_sub(2));
    let h = 24.min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);

    let title = format!(" chats ({}) ", p.items.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(title);
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    let parts = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(inner);

    // Query line.
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled("> ", Style::default().fg(ACCENT).bold()),
            Span::raw(p.query.value().to_string()),
        ])),
        parts[0],
    );
    // Cursor tracks the edit position, not just the end of the text.
    f.set_cursor_position((
        parts[0].x + 4 + (p.query.visual_cursor() as u16).min(parts[0].width.saturating_sub(5)),
        parts[0].y,
    ));

    let width = parts[1].width as usize;
    let mut lines: Vec<Line> = Vec::new();
    if p.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no matches",
            Style::default().fg(DIM),
        )));
    }
    for (i, item) in p.items.iter().enumerate() {
        let Some(chat) = app.chats.iter().find(|c| c.object_id == item.object_id) else {
            continue;
        };
        let selected = i == p.sel;

        let badge = if chat.unread_mentions > 0 {
            format!("@ {} ", chat.unread)
        } else if chat.unread > 0 {
            format!("● {} ", chat.unread)
        } else if chat.unread_reactions > 0 {
            format!("♥ {} ", chat.unread_reactions)
        } else {
            String::new()
        };

        // Highlight the chars the query matched, like helix does.
        // Selection is the ▌ bar plus brighter text — same reasoning as the
        // sidebar: a background tint stops short on truncated rows.
        let label = app.pick_label(chat);
        let base = match (selected, chat.unread > 0) {
            (true, true) => Style::default().fg(SEL_UNREAD).add_modifier(Modifier::BOLD),
            (true, false) => Style::default().fg(SEL).add_modifier(Modifier::BOLD),
            (false, true) => Style::default().fg(UNREAD),
            (false, false) => Style::default().fg(Color::Gray),
        };
        let avail = width.saturating_sub(badge.width() + 3);
        let mut spans = vec![Span::styled(
            if selected { "▌ " } else { "  " },
            Style::default().fg(if selected { ACCENT } else { Color::Reset }),
        )];
        let mut shown = 0usize;
        for (ci, ch) in label.chars().enumerate() {
            if shown >= avail {
                spans.push(Span::styled("…", base));
                break;
            }
            let st = if item.indices.contains(&ci) {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                base
            };
            spans.push(Span::styled(ch.to_string(), st));
            shown += ch.to_string().width();
        }
        let pad = width.saturating_sub(2 + shown + badge.width());
        spans.push(Span::raw(" ".repeat(pad)));
        if !badge.is_empty() {
            spans.push(Span::styled(badge, Style::default().fg(UNREAD).bold()));
        }
        lines.push(Line::from(spans));

        let preview = match &chat.last_text {
            Some(t) if t.is_empty() => "no messages".to_string(),
            Some(t) => format!(
                "{}: {}",
                preview_sender(app, chat),
                one_line(t, width)
            ),
            None => "…".to_string(),
        };
        lines.push(Line::from(Span::styled(
            format!("    {}", one_line(&preview, width.saturating_sub(5))),
            Style::default().fg(if selected { Color::Gray } else { DIM }),
        )));
    }

    // Keep the highlighted row in view.
    let h = parts[1].height as usize;
    let sel_line = p.sel * 2 + 2;
    let offset = sel_line.saturating_sub(h);
    f.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), parts[1]);
}

fn focus_style(active: bool) -> Style {
    if active {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    }
}

fn draw_sidebar(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    let total = app.total_unread();
    let title = if total > 0 {
        format!(" chats ({total}) ")
    } else {
        " chats ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_style(focused))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    let mut last_space = String::new();
    // Chats arrive grouped by space, so a header emits on each space change.
    for (i, chat) in app.chats.iter().enumerate() {
        if chat.space_id != last_space {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                truncate(&chat.space_name, inner.width as usize),
                Style::default().fg(Color::White).bold(),
            )));
            last_space = chat.space_id.clone();
        }

        let selected = i == app.sel;
        let has_unread = chat.unread > 0;

        // `@` when something unread pings you, `●` for plain unread.
        let marker = if chat.unread_mentions > 0 {
            "@"
        } else if has_unread {
            "●"
        } else {
            "○"
        };
        let badge = if chat.unread > 0 {
            format!(" {}", chat.unread)
        } else {
            String::new()
        };
        // Selection is the ▌ bar plus brighter text, not a background tint: a
        // tint only paints cells that hold text, so it stops short on
        // truncated rows and leaves the highlight looking cut off.
        let name_style = match (selected, has_unread) {
            (true, true) => Style::default().fg(SEL_UNREAD).add_modifier(Modifier::BOLD),
            (true, false) => Style::default().fg(SEL).add_modifier(Modifier::BOLD),
            (false, true) => Style::default().fg(UNREAD),
            (false, false) => Style::default().fg(Color::Gray),
        };

        let width = inner.width as usize;
        let label = truncate(chat.label(), width.saturating_sub(4 + badge.width()));
        let pad = width.saturating_sub(2 + label.width() + badge.width() + 1);
        let mut spans = vec![
            Span::styled(
                if selected { "▌" } else { " " },
                Style::default().fg(if selected { ACCENT } else { Color::Reset }),
            ),
            Span::styled(format!("{marker} "), name_style),
            Span::styled(label, name_style),
            Span::raw(" ".repeat(pad)),
        ];
        if !badge.is_empty() {
            spans.push(Span::styled(badge, Style::default().fg(UNREAD).bold()));
        }
        lines.push(Line::from(spans));

        // Preview line: several chats per space share the name "general", so
        // this is what actually distinguishes them.
        let preview = match &chat.last_text {
            Some(t) if t.is_empty() => "no messages".to_string(),
            Some(t) => format!(
                "{}: {}",
                preview_sender(app, chat),
                one_line(t, width)
            ),
            None => "…".to_string(),
        };
        lines.push(Line::from(Span::styled(
            format!("   {}", one_line(&preview, width.saturating_sub(4))),
            // Lift the preview out of DIM when selected so the whole row reads
            // as one unit.
            Style::default().fg(if selected { Color::Gray } else { DIM }),
        )));
    }

    if app.chats.is_empty() {
        lines.push(Line::from(Span::styled(
            "no chats found",
            Style::default().fg(DIM),
        )));
    }

    // Scroll just enough to keep the selected row (and its preview) on screen.
    let sel_line = selected_line_index(app) + 1;
    let h = inner.height as usize;
    let offset = sel_line.saturating_sub(h.saturating_sub(1));
    f.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), inner);
}

/// Line index of the selected chat's title row, accounting for space headers,
/// blank spacers and the per-chat preview line.
fn selected_line_index(app: &App) -> usize {
    let mut idx = 0usize;
    let mut last_space = String::new();
    for (i, chat) in app.chats.iter().enumerate() {
        if chat.space_id != last_space {
            if idx > 0 {
                idx += 1; // blank spacer between spaces
            }
            idx += 1; // space header
            last_space = chat.space_id.clone();
        }
        if i == app.sel {
            return idx;
        }
        idx += 2; // title + preview
    }
    idx
}

fn draw_chat(f: &mut Frame, app: &mut App, area: Rect) {
    // The search view lives in the message pane; the query takes the composer
    // slot at the bottom (see draw_search_view).
    if app.search.is_some() {
        draw_search_view(f, app, area);
        return;
    }
    let input_h = input_height(app, area.width);
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(input_h)]).split(area);

    let focused = app.focus == Focus::Messages;
    // With the sidebar hidden, the title is the only breadcrumb, so it also
    // advertises the way back.
    let title = match app.active_chat() {
        Some(c) if app.single_now => format!(" ‹ Esc  {} ", c.qualified()),
        Some(c) => format!(" {} ", c.qualified()),
        None => " no chat open ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_style(focused))
        .title(title)
        .title_alignment(Alignment::Left);
    let inner = block.inner(rows[0]);
    f.render_widget(block, rows[0]);

    let (lines, ranges) = render_messages(app, inner.width as usize);

    app.view_lines = lines.len();
    app.view_height = inner.height as usize;
    let h = (inner.height as usize).max(1);

    // The viewport follows the message cursor: nudge the offset just enough to
    // keep the selected message on screen, so appends at the bottom and history
    // loaded at the top both leave the reader where they were.
    if let Some(id) = &app.sel_msg {
        if let Some((_, s, e)) = ranges.iter().find(|(mid, _, _)| mid == id) {
            let (s, e) = (*s, *e);
            let mut end = lines.len().saturating_sub(app.scroll);
            if e > end {
                app.scroll = lines.len().saturating_sub(e);
                end = lines.len().saturating_sub(app.scroll);
            }
            let start = end.saturating_sub(h);
            if s < start {
                // Taller than the pane: pin its top rather than its bottom.
                app.scroll = lines.len().saturating_sub(s + h).min(lines.len());
            }
        }
    }
    let max_scroll = lines.len().saturating_sub(h);
    if app.scroll > max_scroll {
        app.scroll = max_scroll;
    }

    let end = lines.len().saturating_sub(app.scroll);
    let start = end.saturating_sub(h);
    let visible: Vec<Line> = lines[start..end].to_vec();
    f.render_widget(Paragraph::new(visible), inner);

    draw_input(f, app, rows[1]);
}

/// The search view: ranked-then-time-sorted results fill the pane, the live
/// query sits in the composer slot. Mirrors `draw_chat`'s follow-the-cursor
/// scroll so paging through results feels like paging through a chat.
fn draw_search_view(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_style(true))
        .title(" search ")
        .title_alignment(Alignment::Left);
    let inner = block.inner(rows[0]);
    f.render_widget(block, rows[0]);

    let (lines, ranges) = render_search_results(app, inner.width as usize);
    let h = (inner.height as usize).max(1);
    let sel = app.search.as_ref().and_then(|s| s.sel.clone());
    let mut scroll = app.search.as_ref().map(|s| s.scroll).unwrap_or(0);
    if let Some(id) = &sel {
        if let Some((_, s0, e0)) = ranges.iter().find(|(mid, _, _)| mid == id) {
            let (s0, e0) = (*s0, *e0);
            let mut end = lines.len().saturating_sub(scroll);
            if e0 > end {
                scroll = lines.len().saturating_sub(e0);
                end = lines.len().saturating_sub(scroll);
            }
            let start = end.saturating_sub(h);
            if s0 < start {
                scroll = lines.len().saturating_sub(s0 + h).min(lines.len());
            }
        }
    }
    let max_scroll = lines.len().saturating_sub(h);
    if scroll > max_scroll {
        scroll = max_scroll;
    }
    if let Some(s) = &mut app.search {
        s.scroll = scroll;
    }

    let end = lines.len().saturating_sub(scroll);
    let start = end.saturating_sub(h);
    let visible: Vec<Line> = lines[start..end].to_vec();
    f.render_widget(Paragraph::new(visible), inner);

    draw_search_query(f, app, rows[1]);
}

fn render_search_results(app: &App, width: usize) -> (Vec<Line<'static>>, MsgRanges) {
    let mut lines: Vec<Line> = Vec::new();
    let mut ranges: MsgRanges = Vec::new();
    let Some(s) = &app.search else {
        return (lines, ranges);
    };

    if s.query.value().trim().is_empty() {
        lines.push(Line::from(Span::styled(
            "  type to search messages",
            Style::default().fg(DIM),
        )));
        return (lines, ranges);
    }
    if s.results.is_empty() {
        lines.push(Line::from(Span::styled(
            if s.searching {
                "  searching…"
            } else {
                "  no matches"
            },
            Style::default().fg(DIM),
        )));
        return (lines, ranges);
    }

    let text_w = width.saturating_sub(2).max(10);
    // With a wider scope the results span chats, so each row names its chat.
    let show_loc = s.scope != SearchScope::Chat;
    let sel_id = s.sel.clone().unwrap_or_default();

    for (i, hit) in s.results.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        let (label, color) = match &hit.agent {
            Some(name) => (format!("✦ {name}"), AGENT),
            None => (
                app.display_name(&hit.creator),
                if hit.creator == app.me { ME } else { PEER },
            ),
        };
        let mut head = vec![
            Span::styled(label, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(fmt_time(hit.created_at), Style::default().fg(DIM)),
        ];
        if show_loc {
            let loc = app
                .chats
                .iter()
                .find(|c| c.object_id == hit.chat_id)
                .map(|c| {
                    if s.scope == SearchScope::AllSpaces {
                        c.qualified()
                    } else {
                        c.label().to_string()
                    }
                })
                .unwrap_or_default();
            if !loc.is_empty() {
                head.push(Span::raw("  "));
                head.push(Span::styled(format!("· {loc}"), Style::default().fg(ACCENT)));
            }
        }

        let start_line = lines.len();
        lines.push(Line::from(head));
        for l in wrap(&hit.text, text_w) {
            lines.push(Line::from(vec![Span::raw("  "), Span::raw(l)]));
        }
        let end_line = lines.len();
        ranges.push((hit.msg_id.clone(), start_line, end_line));

        // Cursor bar, same treatment as the message list.
        if hit.msg_id == sel_id {
            for l in lines.iter_mut().take(end_line).skip(start_line) {
                let mut spans = vec![Span::styled("▌", Style::default().fg(ACCENT))];
                let mut rest = l.spans.clone();
                if !rest.is_empty() && rest[0].content.starts_with("  ") {
                    let trimmed = rest[0].content[2..].to_string();
                    rest[0] = Span::styled(trimmed, rest[0].style);
                    spans.push(Span::raw(" "));
                }
                spans.extend(rest);
                *l = Line::from(spans);
            }
        }
    }
    (lines, ranges)
}

fn draw_search_query(f: &mut Frame, app: &App, area: Rect) {
    let Some(s) = &app.search else {
        return;
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" / ")
        .title_bottom(Line::from(Span::styled(
            " Enter open · Ctrl-r reply · Tab scope · Esc ",
            Style::default().fg(DIM),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("> ", Style::default().fg(ACCENT).bold()),
            Span::raw(s.query.value().to_string()),
        ])),
        inner,
    );
    let cx = inner.x + 2 + (s.query.visual_cursor() as u16).min(inner.width.saturating_sub(3));
    f.set_cursor_position((cx, inner.y));
}

/// Returns the rendered lines plus, for each message, the half-open line range
/// `[start, end)` it occupies — the caller needs those to keep the cursor in
/// view.
type MsgRanges = Vec<(String, usize, usize)>;

fn render_messages(app: &App, width: usize) -> (Vec<Line<'static>>, MsgRanges) {
    let mut lines: Vec<Line> = Vec::new();
    let mut ranges: MsgRanges = Vec::new();
    if app.active.is_none() {
        lines.push(Line::from(Span::styled(
            "  select a chat and press Enter",
            Style::default().fg(DIM),
        )));
        return (lines, ranges);
    }
    if app.msgs.is_empty() {
        lines.push(Line::from(Span::styled(
            if app.loading {
                "  loading…"
            } else {
                "  no messages yet"
            },
            Style::default().fg(DIM),
        )));
        return (lines, ranges);
    }

    if app.exhausted {
        lines.push(Line::from(Span::styled(
            "  ── beginning of chat ──",
            Style::default().fg(DIM),
        )));
    }

    // Each record carries its own `unread` flag, so the divider sits exactly
    // above the first unread message — which may be mid-history, since a peer's
    // offline message can slot between ones you've read. Rows from older builds
    // carry no flag; then the chat's unreadCount approximates a trailing run.
    let unread = app.active_chat().map(|c| c.unread as usize).unwrap_or(0);
    let first_unread = app.msgs.iter().position(|m| m.unread).or({
        if unread > 0 && unread <= app.msgs.len() {
            Some(app.msgs.len() - unread)
        } else {
            None
        }
    });

    let text_w = width.saturating_sub(2).max(10);
    let mut prev_speaker = String::new();
    let mut prev_time = 0f64;

    let sel_id = app.sel_msg.clone().unwrap_or_default();
    for (i, m) in app.msgs.iter().enumerate() {
        if first_unread == Some(i) {
            lines.push(unread_separator(width));
        }

        // Group consecutive messages from one speaker within 5 minutes. Agent
        // and human turns never group together (see speaker_key).
        let speaker = speaker_key(m);
        let grouped = speaker == prev_speaker && (m.created_at - prev_time).abs() < 300.0;
        if !grouped {
            if i > 0 {
                lines.push(Line::from(""));
            }
            let (label, color) = author_label(app, m);
            let mut head = vec![
                Span::styled(label, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(fmt_time(m.created_at), Style::default().fg(DIM)),
            ];
            if m.mentions_me(&app.me) {
                head.push(Span::styled("  @you", Style::default().fg(UNREAD).bold()));
            }
            lines.push(Line::from(head));
        }

        let start_line = lines.len();
        if let Some(rid) = &m.reply_to {
            let snippet = app
                .msgs
                .iter()
                .find(|x| &x.id == rid)
                .map(|x| {
                    let (text, _) = render_mentions(&x.text, |id| mention_name(app, id));
                    format!("{}: {}", app.display_name(&x.creator), one_line(&text, 40))
                })
                .unwrap_or_else(|| "…".to_string());
            // Truncate against the real pane width, otherwise a narrow pane
            // clips this mid-word with no ellipsis to show it was cut.
            lines.push(Line::from(Span::styled(
                format!("  ↪ {}", one_line(&snippet, text_w.saturating_sub(2))),
                Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
            )));
        }

        if !m.text.is_empty() {
            // Mention links render as `@Name` chips (current name, falling
            // back to the snapshot in the link text), highlighted in the body.
            let (text, chips) = render_mentions(&m.text, |id| mention_name(app, id));
            for (n, l) in wrap(&text, text_w).into_iter().enumerate() {
                let mut spans = vec![Span::raw("  ")];
                spans.extend(chip_spans(&l, &chips));
                if n == 0 && m.edited() {
                    spans.push(Span::styled(" (edited)", Style::default().fg(DIM)));
                }
                lines.push(Line::from(spans));
            }
        }

        // Attachments can't be opened yet, but you should be able to see that
        // a message carries them.
        if !m.attachments.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("  📎 {}", m.attachment_summary()),
                Style::default().fg(Color::Blue),
            )));
        }

        if !m.reactions.is_empty() {
            let mut spans = vec![Span::raw("  ")];
            for (emoji, n) in &m.reactions {
                spans.push(Span::styled(
                    format!("{emoji}{n} "),
                    Style::default().fg(Color::Blue),
                ));
            }
            lines.push(Line::from(spans));
        }

        // Only the newest message reflects a live run: a trailing not-done
        // agent message means the agent is still working. Older not-done
        // messages are just intermediate turns, not ongoing activity.
        let is_last = i + 1 == app.msgs.len();
        if is_last {
            if let Some(a) = &m.agent {
                if !a.done {
                    lines.push(Line::from(Span::styled(
                        format!("  ✦ {} is working…", a.name),
                        Style::default().fg(AGENT).add_modifier(Modifier::ITALIC),
                    )));
                }
            }
        }

        let end_line = lines.len();
        ranges.push((m.id.clone(), start_line, end_line));

        // Mark the cursor message: a left bar on its own lines, so it's obvious
        // which message `r` will reply to.
        if m.id == sel_id && app.focus == Focus::Messages {
            for l in lines.iter_mut().take(end_line).skip(start_line) {
                let mut spans = vec![Span::styled("▌", Style::default().fg(ACCENT))];
                // Replace the two-space indent the body lines already carry.
                let mut rest = l.spans.clone();
                if !rest.is_empty() && rest[0].content.starts_with("  ") {
                    let trimmed = rest[0].content[2..].to_string();
                    rest[0] = Span::styled(trimmed, rest[0].style);
                    spans.push(Span::raw(" "));
                }
                spans.extend(rest);
                // The bar alone marks the cursor; a background tint would only
                // cover each line's text and end ragged against short lines.
                *l = Line::from(spans);
            }
        }

        prev_speaker = speaker;
        prev_time = m.created_at;
    }
    (lines, ranges)
}

/// Current display name for a mention identity, if the directory knows one.
fn mention_name(app: &App, identity: &str) -> Option<String> {
    app.names.get(identity).filter(|n| !n.is_empty()).cloned()
}

/// Splits one wrapped body line into spans, styling every occurrence of a
/// mention chip (`@Name`) so it stands out from the surrounding text. Chips are
/// matched longest-first so `@Anna Lee` isn't eaten by `@Anna`.
fn chip_spans(line: &str, chips: &[String]) -> Vec<Span<'static>> {
    if chips.is_empty() {
        return vec![Span::raw(line.to_string())];
    }
    let mut chips: Vec<&String> = chips.iter().collect();
    chips.sort_by_key(|c| std::cmp::Reverse(c.len()));
    chips.dedup();
    let style = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let hit = chips
            .iter()
            .filter_map(|c| rest.find(c.as_str()).map(|i| (i, c.len())))
            .min_by_key(|&(i, len)| (i, std::cmp::Reverse(len)));
        match hit {
            Some((i, len)) => {
                if i > 0 {
                    spans.push(Span::raw(rest[..i].to_string()));
                }
                spans.push(Span::styled(rest[i..i + len].to_string(), style));
                rest = &rest[i + len..];
            }
            None => {
                spans.push(Span::raw(rest.to_string()));
                break;
            }
        }
    }
    spans
}

fn unread_separator(width: usize) -> Line<'static> {
    let label = " new ";
    let dashes = width.saturating_sub(label.len() + 2) / 2;
    Line::from(vec![
        Span::styled("─".repeat(dashes), Style::default().fg(Color::Red)),
        Span::styled(label, Style::default().fg(Color::Red).bold()),
        Span::styled("─".repeat(dashes), Style::default().fg(Color::Red)),
    ])
}

/// Height of the composer: it grows with the wrapped text (plus a banner line
/// when replying), and stops growing at MAX_INPUT_LINES.
fn input_height(app: &App, width: u16) -> u16 {
    if app.mode != Mode::Insert {
        return 1;
    }
    let inner_w = (width.saturating_sub(2) as usize).max(1);
    let lines = wrap_input(app.input.value(), inner_w).len().clamp(1, MAX_INPUT_LINES);
    let banner = if app.reply_to.is_some() { 1 } else { 0 };
    lines as u16 + 2 + banner
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    if app.mode == Mode::Insert {
        let mut area = area;
        // Banner above the box: say exactly who is being replied to, since the
        // reply target is otherwise invisible once you start typing.
        if let Some(rid) = &app.reply_to {
            let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
            let who = app
                .msgs
                .iter()
                .find(|m| &m.id == rid)
                .map(|m| {
                    format!(
                        "{}: {}",
                        app.display_name(&m.creator),
                        one_line(&m.preview_text(), rows[0].width as usize)
                    )
                })
                .unwrap_or_else(|| "…".to_string());
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" ↩ replying to ", Style::default().fg(ACCENT).bold()),
                    Span::styled(
                        one_line(&who, rows[0].width.saturating_sub(16) as usize),
                        Style::default().fg(Color::Gray),
                    ),
                ])),
                rows[0],
            );
            area = rows[1];
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT))
            .title(" message ")
            .title_bottom(Line::from(Span::styled(
                " Enter send · Alt-Enter newline · Esc cancel ",
                Style::default().fg(DIM),
            )));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let w = inner.width.max(1) as usize;
        let (wrapped, crow, ccol) =
            wrap_input_cursor(app.input.value(), w, app.input.cursor());
        // Scroll so the cursor row stays visible once the text outgrows the box,
        // keeping the cursor on the bottom visible row as it moves past the end.
        let h = inner.height as usize;
        let start = (crow as usize + 1).saturating_sub(h);
        let shown: Vec<Line> = wrapped[start..]
            .iter()
            .map(|l| Line::from(l.clone()))
            .collect();
        f.render_widget(Paragraph::new(shown), inner);

        let cy = inner.y + (crow.saturating_sub(start as u16)).min(inner.height.saturating_sub(1));
        let cx = inner.x + ccol.min(inner.width.saturating_sub(1));
        f.set_cursor_position((cx, cy));
    } else {
        // Keep the hint short enough for a phone-width pane.
        let narrow = area.width < 60;
        let hint = match (app.active.is_some(), app.single_now, narrow) {
            (true, true, true) => "  Esc back · i compose",
            (true, true, false) => "  Esc  back to chats   i  compose   r  reply   ?  help",
            (true, false, _) => "  i  compose   r  reply   ?  help",
            (false, _, true) => "  Enter open · ? help",
            (false, _, false) => "  Enter  open chat   ?  help",
        };
        f.render_widget(
            Paragraph::new(Span::styled(hint, Style::default().fg(DIM))),
            area,
        );
    }
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    // A fresh error or toast takes over the bar for a few seconds.
    if let Some((msg, at)) = &app.toast {
        if at.elapsed() < Duration::from_secs(4) {
            f.render_widget(
                Paragraph::new(Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(Color::Black).bg(UNREAD),
                )),
                area,
            );
            return;
        }
    }

    // Search takes over the mode indicator, and carries the scope/mode/count
    // the user needs to steer it (Tab and Ctrl-t change these live).
    if let Some(s) = &app.search {
        let scope = match s.scope {
            SearchScope::Chat => app
                .active_chat()
                .map(|c| format!("chat: {}", c.label()))
                .unwrap_or_else(|| "chat".to_string()),
            SearchScope::Space => app
                .active_chat()
                .map(|c| format!("space: {}", c.space_name))
                .unwrap_or_else(|| "space".to_string()),
            SearchScope::AllSpaces => "all spaces".to_string(),
        };
        let detail = if s.note.is_empty() {
            s.mode.as_str().to_string()
        } else {
            s.note.clone()
        };
        let count = if s.searching {
            "  …".to_string()
        } else if s.query.value().trim().is_empty() {
            String::new()
        } else {
            format!("  {} hits", s.results.len())
        };
        let spans = vec![
            Span::styled(
                " SEARCH ",
                Style::default()
                    .fg(Color::Black)
                    .bg(PEER)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {scope}"), Style::default().fg(ACCENT).bold()),
            Span::styled(format!("  · {detail}"), Style::default().fg(DIM)),
            Span::styled(count, Style::default().fg(DIM)),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let label = match app.mode {
        Mode::Insert => " INSERT ",
        Mode::Normal => " NORMAL ",
    };
    let mut spans = vec![Span::styled(
        label,
        Style::default()
            .fg(Color::Black)
            .bg(match app.mode {
                Mode::Insert => ME,
                Mode::Normal => ACCENT,
            })
            .add_modifier(Modifier::BOLD),
    )];

    // Everything here is budgeted against the real width: on a phone-width
    // pane this bar is the only unread cue, so it must never wrap away.
    let total = area.width as usize;
    let mut used = label.width();
    let narrow = total < 60;

    // Position of the message cursor, shown only when you're off the bottom.
    let scroll_txt = match app.sel_msg_idx() {
        Some(i) if i + 1 < app.msgs.len() => format!("  ↑{}/{}", i + 1, app.msgs.len()),
        _ => String::new(),
    };
    let reserve = scroll_txt.width();

    let others = app.other_unread();
    if others.is_empty() {
        let t = if narrow {
            "  no unread"
        } else {
            "  no unread elsewhere"
        };
        if used + t.width() + reserve <= total {
            spans.push(Span::styled(t, Style::default().fg(DIM)));
        }
    } else {
        let head = "  new: ";
        if used + head.width() + reserve <= total {
            spans.push(Span::styled(head, Style::default().fg(DIM)));
            used += head.width();
        }
        let mut shown = 0;
        for c in others.iter() {
            // Narrow screens get the space name: it distinguishes chats better
            // than the label, which is almost always "general".
            let name = if narrow {
                c.space_name.clone()
            } else {
                c.qualified()
            };
            let mark = if c.unread_mentions > 0 { "@" } else { "●" };
            let e = format!("{mark}{} {}  ", name, c.unread);
            // Leave room for a "+N" overflow marker.
            if used + e.width() + reserve + 4 > total {
                break;
            }
            spans.push(Span::styled(e.clone(), Style::default().fg(UNREAD)));
            used += e.width();
            shown += 1;
        }
        if shown < others.len() {
            let more = format!("+{}", others.len() - shown);
            if used + more.width() + reserve <= total {
                spans.push(Span::styled(more, Style::default().fg(UNREAD).bold()));
            }
        }
    }

    if !scroll_txt.is_empty() {
        spans.push(Span::styled(scroll_txt, Style::default().fg(Color::Blue)));
    }

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_help(f: &mut Frame, area: Rect, version: &str) {
    // A phone-width pane can't fit the roomy keymap, and clipped help is worse
    // than terse help.
    if area.width < 56 {
        return draw_help_compact(f, area, version);
    }
    let text = vec![
        "  Navigation",
        "    Space           fuzzy-find a chat",
        "    /               search messages",
        "    Ctrl-n / Ctrl-p next / previous chat",
        "    j / k, ↓ / ↑    move chat · move message cursor",
        "    Enter           step into the chat",
        "    Esc / h         back to chat list",
        "    Tab             switch pane",
        "    n               next chat with unread",
        "    g / G           oldest / newest message",
        "    Ctrl-d / Ctrl-u jump 5 messages",
        "",
        "  Search (/)",
        "    type            query (updates as you type)",
        "    Tab             scope: chat → space → all",
        "    Ctrl-t          mode: hybrid → fts → vector",
        "    ↓ / ↑, PgDn/PgUp move / page through results",
        "    Enter           jump to the message",
        "    Ctrl-r          jump there and reply",
        "    from:@name      filter by sender",
        "",
        "  Chat list",
        "    z               show / hide the chat list",
        "                    (it hides itself anyway under 80 cols)",
        "",
        "  Messages",
        "    i               compose",
        "                    Enter sends · Alt-Enter newline",
        "    r               reply to the message under ▌",
        "    R               mark chat read now",
        "",
        "  Other",
        "    ?               toggle this help",
        "    q / Ctrl-c      quit",
        "",
        "  The bottom bar tracks unread in every other chat,",
        "  live, even when the chat list is hidden.",
    ];
    let w = 60.min(area.width.saturating_sub(4));
    // +1 for the daemon line appended below, +2 for the border.
    let h = (text.len() as u16 + 3).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" keys ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let mut lines: Vec<Line> = text
        .iter()
        .map(|l| Line::from(Span::styled(*l, Style::default().fg(Color::Gray))))
        .collect();
    lines.push(Line::from(Span::styled(
        one_line(
            &format!("  daemon: {version}"),
            inner.width as usize,
        ),
        Style::default().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_help_compact(f: &mut Frame, area: Rect, version: &str) {
    let text = vec![
        "  Navigate",
        "   Space    find a chat",
        "   C-n/C-p  next/prev chat",
        "   j/k      move cursor",
        "   Enter    step into chat",
        "   Esc/h    back to list",
        "   Tab      switch pane",
        "   n        next unread",
        "   g/G      oldest/newest",
        "   C-d/C-u  jump 5 msgs",
        "",
        "  Chat list",
        "   z        show/hide",
        "",
        "  Message",
        "   i        compose",
        "   A-Enter  newline",
        "   r        reply to ▌",
        "   R        mark read",
        "",
        "   ?  help      q  quit",
    ];
    let w = 30.min(area.width.saturating_sub(2));
    let h = (text.len() as u16 + 3).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" keys ");
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let mut lines: Vec<Line> = text
        .iter()
        .map(|l| Line::from(Span::styled(*l, Style::default().fg(Color::Gray))))
        .collect();
    lines.push(Line::from(Span::styled(
        one_line(&format!("  {version}"), inner.width as usize),
        Style::default().fg(DIM),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

// ---- text helpers --------------------------------------------------------

/// Wraps composer text. Unlike `wrap`, this preserves the text verbatim
/// (spaces included) so the cursor column matches what was typed, and it keeps
/// empty lines produced by explicit newlines.
fn wrap_input(s: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.split('\n') {
        let mut cur: Vec<char> = Vec::new();
        for ch in para.chars() {
            cur.push(ch);
            if line_width(&cur) > width {
                match cur.iter().rposition(|c| *c == ' ') {
                    // Break at the last space so words stay whole.
                    Some(b) if b > 0 => {
                        let rest: Vec<char> = cur.split_off(b + 1);
                        while cur.last() == Some(&' ') {
                            cur.pop();
                        }
                        out.push(cur.iter().collect());
                        cur = rest;
                    }
                    // A single word longer than the box: hard break.
                    _ => {
                        let last = cur.pop().unwrap_or(' ');
                        out.push(cur.iter().collect());
                        cur = vec![last];
                    }
                }
            }
        }
        out.push(cur.iter().collect());
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn line_width(cs: &[char]) -> usize {
    cs.iter().map(|c| c.to_string().width()).sum()
}

/// Like [`wrap_input`], but also reports where the edit cursor (a source char
/// index) lands in the wrapped output as `(row, col)`. Kept in lock-step with
/// `wrap_input`'s breaking rules so the cursor sits exactly on the glyph it
/// edits, including across soft wraps and explicit newlines.
fn wrap_input_cursor(s: &str, width: usize, cursor: usize) -> (Vec<String>, u16, u16) {
    let mut out: Vec<String> = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    let mut cpos: Option<(usize, usize)> = None;
    let mut gi = 0usize; // source chars consumed so far

    // The cursor sits before the char at index `cursor`; record its position
    // the moment we've consumed exactly that many source chars.
    macro_rules! mark {
        () => {
            if cpos.is_none() && gi == cursor {
                cpos = Some((out.len(), line_width(&cur)));
            }
        };
    }

    mark!();
    for ch in s.chars() {
        if ch == '\n' {
            out.push(cur.iter().collect());
            cur = Vec::new();
            gi += 1;
            mark!();
            continue;
        }
        cur.push(ch);
        if line_width(&cur) > width {
            match cur.iter().rposition(|c| *c == ' ') {
                Some(b) if b > 0 => {
                    let rest: Vec<char> = cur.split_off(b + 1);
                    while cur.last() == Some(&' ') {
                        cur.pop();
                    }
                    out.push(cur.iter().collect());
                    cur = rest;
                }
                _ => {
                    let last = cur.pop().unwrap_or(' ');
                    out.push(cur.iter().collect());
                    cur = vec![last];
                }
            }
        }
        gi += 1;
        mark!();
    }
    out.push(cur.iter().collect());
    if out.is_empty() {
        out.push(String::new());
    }
    let (row, col) = cpos.unwrap_or((out.len() - 1, 0));
    (out, row as u16, col as u16)
}

/// Greedy word wrap on display width, preserving explicit newlines.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        if para.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in para.split_whitespace() {
            let w = word.width();
            if line.is_empty() {
                // A single word longer than the pane: hard-split it.
                if w > width {
                    for chunk in hard_split(word, width) {
                        out.push(chunk);
                    }
                } else {
                    line = word.to_string();
                }
            } else if line.width() + 1 + w <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                if w > width {
                    for chunk in hard_split(word, width) {
                        out.push(chunk);
                    }
                } else {
                    line = word.to_string();
                }
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn hard_split(word: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in word.chars() {
        if cur.width() + ch.to_string().width() > width {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        if out.width() + 1 >= width {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn one_line(s: &str, width: usize) -> String {
    truncate(&s.replace('\n', " "), width)
}

/// First name only — sidebar previews have no room for "Konstantin Ivanov".
fn short_name(s: &str) -> String {
    s.split_whitespace().next().unwrap_or(s).to_string()
}

fn fmt_time(ts: f64) -> String {
    let dt: DateTime<Local> = match Local.timestamp_opt(ts as i64, 0) {
        chrono::LocalResult::Single(t) => t,
        _ => return String::new(),
    };
    let now = Local::now();
    if dt.date_naive() == now.date_naive() {
        dt.format("%H:%M").to_string()
    } else {
        dt.format("%d %b %H:%M").to_string()
    }
}

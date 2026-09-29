use crate::app::{App, Focus, GalleryKind, Mode, SearchMode, SearchOrder, SearchScope};
use crate::model::Liveness;
use crate::model::{BaoPresence, action_body, find_ci, highlight_hits, md_unescape, parse_whisper, render_mentions};
use chrono::{DateTime, Local, TimeZone};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState},
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
/// Something unread that pings you: `@3`, `@you`, `↪ reply to you`.
const MENTION: Color = Color::Indexed(203);
/// A `/hl` word: `★2`, the `★` mark, the word itself in the text.
const HIGHLIGHT: Color = Color::Indexed(141);
const ME: Color = Color::Green;
/// Agent-authored messages get their own hue and a `✦` marker, since they're
/// signed by the human account and would otherwise read as that person.
const AGENT: Color = Color::LightBlue;
/// Direct-sync badges (LAN / iroh p2p) on the chat list.
const P2P: Color = Color::Indexed(114);
/// Whispers — private notes about a message, carried in a DM — sit on a
/// dark red band.
const WHISPER_BG: Color = Color::Indexed(88);

/// The author label + colour for a message: the agent's name when
/// agent-authored (never the human that signed it), otherwise the sender.
fn author_label(app: &App, m: &crate::model::Message) -> (String, Color) {
    match &m.agent {
        Some(a) => (format!("✦ {}", a.name), AGENT),
        None => person_label(app, &m.creator),
    }
}

/// A human's name and colour: green for you, otherwise a colour hashed from
/// the identity, IRC-client style, so the same person keeps the same colour
/// across chats and restarts.
fn person_label(app: &App, identity: &str) -> (String, Color) {
    let color = if identity == app.me { ME } else { nick_color(identity) };
    (iconed_name(app, identity, app.display_name(identity)), color)
}

/// `(A7hQ66M)` — the start of a named person's identity, shown after the name so
/// two people who picked the same name stay apart. None for agents (their
/// name is theirs) and for people shown by id already (no name known).
fn id_tag(app: &App, m_agent: bool, identity: &str) -> Option<String> {
    let named = app.names.get(identity).is_some_and(|n| !n.is_empty());
    (!m_agent && named).then(|| format!("({})", crate::model::short_id(identity)))
}

/// `🧉 Name` when the person's profile icon renders in a terminal.
fn iconed_name(app: &App, identity: &str, name: String) -> String {
    match app.member_icon(identity) {
        Some(icon) => format!("{icon} {name}"),
        None => name,
    }
}

/// 256-colour picks that read on dark and light terminals and stay clear of
/// the colours that already mean something (you, agents, accent, unread).
const NICKS: [Color; 10] = [
    Color::Indexed(168),
    Color::Indexed(173),
    Color::Indexed(179),
    Color::Indexed(107),
    Color::Indexed(73),
    Color::Indexed(110),
    Color::Indexed(140),
    Color::Indexed(175),
    Color::Indexed(209),
    Color::Indexed(147),
];

fn nick_color(identity: &str) -> Color {
    // FNV-1a: tiny, stable across runs (unlike std's randomized hasher).
    let h = identity
        .bytes()
        .fold(0x811c9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x01000193));
    NICKS[h as usize % NICKS.len()]
}

/// Sender label for a chat's last-message preview: the agent name (with the
/// `✦` marker) when the newest message was agent-authored, else the human.
fn preview_sender(app: &App, chat: &crate::model::Chat) -> String {
    match &chat.last_agent {
        Some(name) => format!("✦ {}", short_name(name)),
        None => iconed_name(app, &chat.last_creator, short_name(&app.display_name(&chat.last_creator))),
    }
}

/// `Name: text`, or `* Name waves` for a `/me` action.
fn preview_line(sender: &str, text: &str, width: usize) -> String {
    match action_body(text) {
        Some(body) => format!("* {sender} {}", one_line(body, width)),
        None => format!("{sender}: {}", one_line(text, width)),
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

/// The last pass over a finished frame: every cell's grapheme made safe for
/// the terminal under the `/icons` mode (`model::terminal_safe`). Done on the
/// buffer, after layout, so widths ratatui already allotted never change —
/// the terminal just never receives a character its width table disagrees
/// on.
pub fn sanitize(buf: &mut ratatui::buffer::Buffer, mode: crate::model::EmojiMode) {
    if mode == crate::model::EmojiMode::Full {
        return;
    }
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if let Some(safe) = crate::model::terminal_safe(cell.symbol(), mode) {
                cell.set_symbol(&safe);
            }
        }
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
    if app.devices_view.is_some() {
        draw_devices(f, f.area(), app);
    }
    if app.show_help {
        draw_help(f, f.area(), app);
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

        let (badge, badge_color) = match unread_badge(chat) {
            Some((b, c)) => (format!("{b} "), c),
            None if chat.unread_reactions > 0 => (format!("(♥{}) ", chat.unread_reactions), UNREAD),
            None => (String::new(), UNREAD),
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
            spans.push(Span::styled(badge, Style::default().fg(badge_color).bold()));
        }
        lines.push(Line::from(spans));

        let preview = match &chat.last_text {
            Some(t) if t.is_empty() => "no messages".to_string(),
            Some(t) => preview_line(&preview_sender(app, chat), t, width),
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

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
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
    app.sidebar_h = inner.height as usize;
    let app: &App = app;

    let mut lines: Vec<Line> = Vec::new();
    let mut last_space = String::new();
    let width = inner.width as usize;
    let compact = app.prefs.compact;
    // Compact rows name the chat only where a space has more than one.
    let mut per_space: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for c in &app.chats {
        *per_space.entry(c.space_id.as_str()).or_default() += 1;
    }
    // Chats arrive grouped by space, so a header emits on each space change.
    for (i, chat) in app.chats.iter().enumerate() {
        // How this space reaches other people's devices: `lan` (mDNS) and/or
        // `p2p` (direct over iroh), `✗` when the space is offline.
        let sync = app
            .sync
            .get(&chat.space_id)
            .map(|st| sync_badge(st, app.direct_for(&chat.space_id)))
            .unwrap_or_default();
        let sync_style = Style::default().fg(if sync.starts_with('✗') { Color::Red } else { P2P });
        let (icon, icon_style) = match app.space_icon(&chat.space_id) {
            Some((g, c)) => (format!("{g} "), Style::default().fg(icon_palette(c.as_deref()))),
            None => (String::new(), Style::default()),
        };

        if !compact && chat.space_id != last_space {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            let name = truncate(&chat.space_name, width.saturating_sub(icon.width() + sync.width() + 1));
            let pad = width.saturating_sub(icon.width() + name.width() + sync.width());
            lines.push(Line::from(vec![
                Span::styled(icon.clone(), icon_style),
                Span::styled(name, Style::default().fg(Color::White).bold()),
                Span::raw(" ".repeat(pad)),
                Span::styled(sync.clone(), sync_style),
            ]));
            last_space = chat.space_id.clone();
        }

        let selected = i == app.sel;
        let has_unread = chat.unread > 0;

        // No marker column: the unread count says it (`unread_badge`).
        let (badge, badge_color) = match unread_badge(chat) {
            Some((b, c)) => (format!(" {b}"), c),
            None => (String::new(), UNREAD),
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

        // Compact: one row per chat, `● 🧉 Gustavo   p2p 2`. Otherwise the
        // chat's own row under its space header, then a preview line.
        let (label, right) = if compact {
            let label = if per_space.get(chat.space_id.as_str()).copied().unwrap_or(0) > 1 {
                format!("{}/{}", chat.space_name, chat.label())
            } else {
                chat.space_name.clone()
            };
            let right = if sync.is_empty() { String::new() } else { format!(" {sync}") };
            (label, right)
        } else {
            (chat.label().to_string(), String::new())
        };
        let row_icon = if compact { icon.clone() } else { String::new() };
        let label = truncate(&label, width.saturating_sub(3 + row_icon.width() + right.width() + badge.width()));
        let pad = width.saturating_sub(2 + row_icon.width() + label.width() + right.width() + badge.width());
        let mut spans = vec![
            Span::styled(
                if selected { "▌" } else { " " },
                Style::default().fg(if selected { ACCENT } else { Color::Reset }),
            ),
            Span::raw(" "),
            Span::styled(row_icon, icon_style),
            Span::styled(label, name_style),
            Span::raw(" ".repeat(pad)),
            Span::styled(right, sync_style),
        ];
        if !badge.is_empty() {
            spans.push(Span::styled(badge, Style::default().fg(badge_color).bold()));
        }
        lines.push(Line::from(spans));
        if compact {
            continue;
        }

        // Preview line: several chats per space share the name "general", so
        // this is what actually distinguishes them.
        let preview = match &chat.last_text {
            Some(t) if t.is_empty() => "no messages".to_string(),
            Some(t) => preview_line(&preview_sender(app, chat), t, width),
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
    let sel_line = selected_line_index(app) + usize::from(!app.prefs.compact);
    let h = inner.height as usize;
    let offset = sel_line.saturating_sub(h.saturating_sub(1));
    f.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), inner);
}

/// Line index of the selected chat's title row, accounting for space headers,
/// blank spacers and the per-chat preview line.
fn selected_line_index(app: &App) -> usize {
    // Compact: one row per chat, nothing else.
    if app.prefs.compact {
        return app.sel;
    }
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
    if app.gallery.is_some() {
        draw_gallery(f, app, area);
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
    // Messages are one line apart; count that gap into each one's height.
    app.msg_heights = ranges.iter().map(|(id, s0, e0)| (id.clone(), e0 - s0 + 1)).collect();
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

    let (scope_title, mode_title) = search_titles(app);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_style(true))
        .title(scope_title)
        .title(mode_title.right_aligned());
    let inner = block.inner(rows[0]);
    f.render_widget(block, rows[0]);

    let (mut lines, mut ranges) = render_search_results(app, inner.width as usize);
    let h = (inner.height as usize).max(1);
    app.search_view_h = h;
    app.search_heights = ranges.iter().map(|(id, s0, e0)| (id.clone(), e0 - s0 + 1)).collect();
    // Results sit against the prompt, where the cursor starts (fzf-style);
    // the tips on an empty query stay at the top.
    let has_results = app.search.as_ref().is_some_and(|s| !s.results.is_empty());
    if has_results && lines.len() < h {
        let pad = h - lines.len();
        lines.splice(0..0, std::iter::repeat_n(Line::from(""), pad));
        for r in &mut ranges {
            r.1 += pad;
            r.2 += pad;
        }
    }
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

/// The open chat's files or links (`F` / `L`), newest first: one entry per
/// file or link with who sent it and when, and a line of the message it came
/// in. Replaces the message pane like the search view does.
fn draw_gallery(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(g) = &app.gallery else { return };
    let (kind, sel_idx) = (g.kind, g.sel);
    let on = Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD);
    let off = Style::default().fg(DIM);
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(format!(" 📎 files {} ", app.files.len()), if kind == GalleryKind::Files { on } else { off }),
        Span::styled("│", off),
        Span::styled(format!(" 🔗 links {} ", app.links.len()), if kind == GalleryKind::Links { on } else { off }),
        Span::raw(" "),
    ]);
    let hint = match kind {
        GalleryKind::Files => " Enter message · o open · s save · Tab links · Esc ",
        GalleryKind::Links => " Enter message · o open · Tab files · Esc ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focus_style(true))
        .title(title)
        .title_bottom(Line::from(Span::styled(hint, off)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let width = inner.width as usize;
    let items = app.gallery_items();
    let mut lines: Vec<Line> = Vec::new();
    let mut sel_range = (0, 0);
    if items.is_empty() {
        let what = if kind == GalleryKind::Files { "files" } else { "links" };
        let msg = if app.msg_total.is_none() {
            "  loading…".to_string()
        } else {
            format!("  no {what} in this chat")
        };
        lines.push(Line::from(Span::styled(msg, off)));
    }
    for (i, it) in items.iter().enumerate() {
        let selected = i == sel_idx;
        let (who, color) = match &it.agent {
            Some(n) => (format!("✦ {n}"), AGENT),
            None => person_label(app, &it.creator),
        };
        let who = match id_tag(app, it.agent.is_some(), &it.creator) {
            Some(t) => format!("{who} {t}"),
            None => who,
        };
        let meta = format!("  {who} · {}", fmt_time(it.created_at));
        let label = match &it.target {
            crate::model::AttachmentTarget::File { file_id, .. } => match app.file_infos.get(file_id) {
                Some(info) if !info.name.is_empty() => {
                    format!("📎 {} · {}", info.name, crate::files::human_size(info.size))
                }
                _ => "📎 file".to_string(),
            },
            crate::model::AttachmentTarget::Object => "🔗 linked object".to_string(),
            _ => format!("🔗 {}", it.link.trim_start_matches("https://").trim_start_matches("http://")),
        };
        let label_w = width.saturating_sub(meta.width() + 3).max(12);
        let label = truncate(&label, label_w);
        let pad = width.saturating_sub(2 + label.width() + meta.width());
        let start = lines.len();
        let mut head = vec![
            Span::styled(if selected { "▌ " } else { "  " }, Style::default().fg(ACCENT)),
            Span::styled(
                label,
                if selected {
                    Style::default().fg(SEL).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Blue)
                },
            ),
            Span::raw(" ".repeat(pad)),
        ];
        head.push(Span::styled(format!("  {who}"), Style::default().fg(color)));
        head.push(Span::styled(format!(" · {}", fmt_time(it.created_at)), off));
        lines.push(Line::from(head));
        // The message it came in, minus the link itself when that's all.
        let preview = one_line(&it.text, width.saturating_sub(6));
        if !preview.trim().is_empty() && preview.trim() != it.link {
            lines.push(Line::from(vec![
                Span::styled(if selected { "▌ " } else { "  " }, Style::default().fg(ACCENT)),
                Span::styled(format!("  ↳ {preview}"), off),
            ]));
        }
        if selected {
            sel_range = (start, lines.len());
        }
    }

    // Keep the cursor's entry on screen.
    let h = (inner.height as usize).max(1);
    app.gallery_view_h = h;
    let mut scroll = app.gallery.as_ref().map_or(0, |g| g.scroll);
    if sel_range.0 < scroll {
        scroll = sel_range.0;
    } else if sel_range.1 > scroll + h {
        scroll = sel_range.1 - h;
    }
    scroll = scroll.min(lines.len().saturating_sub(h));
    if let Some(g) = &mut app.gallery {
        g.scroll = scroll;
    }
    let end = (scroll + h).min(lines.len());
    f.render_widget(Paragraph::new(lines[scroll..end].to_vec()), inner);
}

/// An unread count as shown: `999+` past that.
fn unread_count(n: u64) -> String {
    if n > 999 { "999+".to_string() } else { n.to_string() }
}

/// A chat's unread badge and its colour — `(3)`, `(@3)` when something
/// unread pings you, `(★3)` for a `/hl` word — or None when it's read.
fn unread_badge(chat: &crate::model::Chat) -> Option<(String, Color)> {
    if chat.unread == 0 {
        return None;
    }
    let (mark, color) = if chat.unread_mentions > 0 {
        ("@", MENTION)
    } else if chat.hl {
        ("★", HIGHLIGHT)
    } else {
        ("", UNREAD)
    };
    Some((format!("({mark}{})", unread_count(chat.unread)), color))
}

/// An `icon:v2` colour name (any-ui's icon palette) as a terminal colour.
/// Emoji carry their own colours; an unnamed colour leaves the glyph plain.
fn icon_palette(name: Option<&str>) -> Color {
    match name {
        Some("yellow") => Color::Indexed(178),
        Some("orange") => Color::Indexed(208),
        Some("red") => Color::Indexed(167),
        Some("pink") => Color::Indexed(211),
        Some("purple") => Color::Indexed(141),
        Some("blue") => Color::Indexed(75),
        Some("ice") => Color::Indexed(117),
        Some("teal") => Color::Indexed(37),
        Some("green") => Color::Indexed(71),
        _ => Color::Reset,
    }
}

/// The sidebar's per-space path badge: `lan`, `p2p`, `lan p2p`, or `✗`.
/// Only other people's devices count when we can tell (`direct`): your own
/// devices sync every space, so they'd badge them all.
fn sync_badge(st: &crate::model::SyncStatus, direct: Option<crate::model::DirectPeers>) -> String {
    if matches!(st.state.as_str(), "offline" | "error") {
        return "✗".to_string();
    }
    let (lan, p2p) = match direct {
        Some(d) => (d.lan, d.p2p),
        None => (st.local_peers, st.global_peers),
    };
    let mut parts = Vec::new();
    if lan > 0 {
        parts.push("lan");
    }
    if p2p > 0 {
        parts.push("p2p");
    }
    parts.join(" ")
}

/// The search view's two border titles, as segmented controls: where
/// (`this chat │ space │ all spaces`, Tab) and how (`hybrid │ fts`, Ctrl-t,
/// plus `semantic` while Ctrl-g has it on) with the order (Ctrl-o).
fn search_titles(app: &App) -> (Line<'static>, Line<'static>) {
    let Some(s) = &app.search else {
        return (Line::from(" search "), Line::from(""));
    };
    let on = Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD);
    let off = Style::default().fg(DIM);
    let seg = |label: &str, active: bool| Span::styled(format!(" {label} "), if active { on } else { off });
    let bar = || Span::styled("│", off);

    let chat = app.active_chat().map(|c| c.label().to_string()).unwrap_or_else(|| "chat".into());
    let space = app.active_chat().map(|c| c.space_name.clone()).unwrap_or_else(|| "space".into());
    let scope = Line::from(vec![
        Span::raw(" "),
        seg(&format!("⌕ {}", truncate(&chat, 18)), s.scope == SearchScope::Chat),
        bar(),
        seg(&truncate(&space, 18), s.scope == SearchScope::Space),
        bar(),
        seg("all spaces", s.scope == SearchScope::AllSpaces),
        Span::raw(" "),
    ]);

    let mut mode = vec![
        Span::raw(" "),
        seg("hybrid", s.mode == SearchMode::Hybrid),
        bar(),
        seg("fts", s.mode == SearchMode::Fts),
    ];
    if s.mode == SearchMode::Vector {
        mode.push(bar());
        mode.push(seg("semantic", true));
    }
    mode.push(Span::styled(
        match s.order {
            SearchOrder::Best => "  best first ",
            SearchOrder::Newest => "  newest first ",
        },
        off,
    ));
    (scope, Line::from(mode))
}

/// Lines of one hit shown when it isn't under the cursor; the selected one
/// opens up to `SEL_HIT_LINES`.
const HIT_LINES: usize = 3;
const SEL_HIT_LINES: usize = 12;

fn render_search_results(app: &App, width: usize) -> (Vec<Line<'static>>, MsgRanges) {
    let mut lines: Vec<Line> = Vec::new();
    let mut ranges: MsgRanges = Vec::new();
    let Some(s) = &app.search else {
        return (lines, ranges);
    };
    let dim = Style::default().fg(DIM);
    let where_ = match s.scope {
        SearchScope::Chat => "this chat",
        SearchScope::Space => "this space",
        SearchScope::AllSpaces => "all spaces",
    };

    let query = crate::app::search_terms(s.query.value());
    if query.is_empty() {
        lines.push(Line::from(Span::styled(format!("  type to search {where_}"), dim)));
        lines.push(Line::from(""));
        for (k, what) in [
            ("Tab / S-Tab", "this chat → space → all spaces"),
            ("Ctrl-t", "hybrid ⇄ fts (exact words)"),
            ("Ctrl-g", "semantic only (meaning, not words)"),
            ("Ctrl-o", "best first ⇄ newest first"),
            ("from:@name", "only messages by someone"),
            ("Enter / C-r", "jump to the message / and reply"),
        ] {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<13}"), Style::default().fg(ACCENT)),
                Span::styled(what.to_string(), dim),
            ]));
        }
        return (lines, ranges);
    }
    if s.results.is_empty() {
        let msg = if s.searching {
            format!("  searching {where_}…")
        } else {
            let wider = match s.scope {
                SearchScope::Chat => " — Tab searches the whole space",
                SearchScope::Space => " — Tab searches all spaces",
                SearchScope::AllSpaces => "",
            };
            let loosen = match s.mode {
                SearchMode::Fts => " · fts wants the exact words; Ctrl-t for hybrid",
                _ => "",
            };
            format!("  no matches in {where_}{wider}{loosen}")
        };
        lines.push(Line::from(Span::styled(msg, dim)));
        return (lines, ranges);
    }

    let text_w = width.saturating_sub(2).max(10);
    // With a wider scope the results span chats, so each row names its chat.
    let show_loc = s.scope != SearchScope::Chat;
    let sel_id = s.sel.clone().unwrap_or_default();
    let term_style = Style::default().fg(UNREAD).add_modifier(Modifier::BOLD);

    let compact = app.prefs.compact;
    for (i, hit) in s.results.iter().enumerate() {
        if i > 0 && !compact {
            lines.push(Line::from(""));
        }
        let (label, color) = match &hit.agent {
            Some(name) => (format!("✦ {name}"), AGENT),
            None => person_label(app, &hit.creator),
        };
        let tag = id_tag(app, hit.agent.is_some(), &hit.creator).map(|t| format!(" {t}")).unwrap_or_default();
        let mut head = vec![
            Span::styled(label.clone(), Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(tag, dim),
            Span::raw("  "),
            Span::styled(fmt_time(hit.created_at), dim),
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
        let text = match action_body(&hit.text) {
            Some(body) => format!("* {label} {body}"),
            None => hit.text.clone(),
        };
        let terms: Vec<String> = find_ci(&text, &query, false);
        let marks: Vec<(String, Style)> = terms.into_iter().map(|t| (t, term_style)).collect();
        let cap = if hit.msg_id == sel_id { SEL_HIT_LINES } else { HIT_LINES };

        // Compact: IRC-log lines, `24 Sep 13:32 Name (tag) · place text…`,
        // continuations hanging past the time. The first line always shows
        // (it says who and where); a match further in follows after `…`.
        if compact {
            let mut lead: Vec<(String, Style)> = head
                .iter()
                .filter(|sp| !sp.content.trim().is_empty())
                .map(|sp| (sp.content.trim().to_string(), sp.style))
                .collect();
            // Time first, as in the chat's compact lines; a colon after the
            // place, so the text doesn't run into it.
            if let Some(pos) = lead.iter().position(|(t, _)| *t == fmt_time(hit.created_at)) {
                let t = lead.remove(pos);
                lead.insert(0, t);
            }
            if let Some(last) = lead.last_mut().filter(|(t, _)| t.starts_with("· ")) {
                last.0.push(':');
            }
            let lead_text: String = lead.iter().map(|(t, _)| format!("{t} ")).collect();
            let hang = 6;
            let wrapped = wrap(&format!("{lead_text}{text}"), text_w.saturating_sub(hang));
            let first = wrapped
                .iter()
                .position(|l| !find_ci(l, &query, false).is_empty())
                .unwrap_or(0);
            let mut shown: Vec<(usize, bool)> = vec![(0, false)];
            if first < cap {
                shown.extend((1..cap.min(wrapped.len())).map(|n| (n, false)));
            } else {
                let from = first.saturating_sub(1).max(1);
                shown.extend((from..(from + cap - 1).min(wrapped.len())).enumerate().map(|(k, n)| (n, k == 0)));
            }
            let last = shown.last().map_or(0, |(n, _)| *n);
            for (n, cut) in shown {
                let mut spans = vec![Span::raw("  ")];
                if n > 0 {
                    spans.push(Span::raw(" ".repeat(hang)));
                }
                if cut {
                    spans.push(Span::styled("… ", dim));
                }
                let mut rest = wrapped[n].as_str();
                if n == 0 {
                    for (t, st) in &lead {
                        let Some(r) = rest.strip_prefix(t.as_str()) else { break };
                        spans.push(Span::styled(t.clone(), *st));
                        spans.push(Span::raw(" "));
                        rest = r.strip_prefix(' ').unwrap_or(r);
                    }
                }
                spans.extend(mark_spans(rest, &marks));
                lines.push(Line::from(spans));
            }
            if last + 1 < wrapped.len() {
                let more = wrapped.len() - last - 1;
                lines.push(Line::from(Span::styled(
                    format!("  {}… {more} more line{}", " ".repeat(hang), if more == 1 { "" } else { "s" }),
                    dim,
                )));
            }
        } else {
            lines.push(Line::from(head));

            // Show a window of the wrapped text around the first matched term,
            // with `…` where it was cut; the hit under the cursor opens up.
            let wrapped = wrap(&text, text_w.saturating_sub(2));
            let first = wrapped
                .iter()
                .position(|l| !find_ci(l, &query, false).is_empty())
                .unwrap_or(0);
            let mut from = first.saturating_sub(1).min(wrapped.len().saturating_sub(cap));
            // A cut window shouldn't open on a blank line.
            while from > 0 && from < first && wrapped[from].trim().is_empty() {
                from += 1;
            }
            let to = (from + cap).min(wrapped.len());
            for (n, l) in wrapped[from..to].iter().enumerate() {
                let mut spans = vec![Span::raw("  ")];
                if n == 0 && from > 0 {
                    spans.push(Span::styled("… ", dim));
                }
                spans.extend(mark_spans(l, &marks));
                lines.push(Line::from(spans));
            }
            if to < wrapped.len() {
                let more = wrapped.len() - to;
                lines.push(Line::from(Span::styled(
                    format!("  … {more} more line{}", if more == 1 { "" } else { "s" }),
                    dim,
                )));
            }
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
            " Enter open · C-r reply · Tab scope · C-t fts · C-o order · Esc ",
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
        let compact = app.prefs.compact;
        let (label, color) = author_label(app, m);
        let tag = id_tag(app, m.agent.is_some(), &m.creator);
        // A reply to one of your messages gets its own `↪ reply to you` line;
        // the server-derived `@you` would only repeat it.
        let reply_to_me = m.reply_to.as_ref().is_some_and(|rid| {
            app.msgs.iter().any(|x| &x.id == rid && x.creator == app.me && x.agent.is_none())
        });
        let pings_me = m.mentions_me(&app.me) && !reply_to_me;
        let name_style = Style::default().fg(color).add_modifier(Modifier::BOLD);
        // Mention links render as `@Name` chips (current name, falling back
        // to the snapshot in the link text); `/hl` words in other people's
        // messages light up like a mention.
        // A whisper in a DM: its link becomes a quote line (like a reply's),
        // and only the private text is the body. Enter follows the link.
        let whisper = parse_whisper(&m.text);
        let raw = whisper.as_ref().map_or(m.text.as_str(), |w| w.body.as_str());
        let (text, chips) = render_mentions(raw, |id| mention_name(app, id));
        let text = md_unescape(&text);
        // Agents sign as you too, so "yours" means human-authored by you.
        let hl = if m.creator == app.me && m.agent.is_none() {
            Vec::new()
        } else {
            highlight_hits(&text, &app.prefs.highlights)
        };
        let mut marks: Vec<(String, Style)> = chips
            .into_iter()
            .map(|c| (c, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
            .collect();
        marks.extend(hl.iter().map(|h| (h.clone(), Style::default().fg(HIGHLIGHT).add_modifier(Modifier::BOLD))));
        let action = action_body(&text).map(str::to_string);

        // Compact mode has no headers: every line carries its own time
        // (clock only, so the columns line up) and name, and a divider marks
        // each new day, like an IRC log's "day changed".
        if compact && fmt_day(m.created_at) != fmt_day(prev_time) {
            lines.push(Line::from(Span::styled(
                format!("  ── {} ──", fmt_day(m.created_at)),
                Style::default().fg(DIM),
            )));
        }
        if !compact && !grouped {
            if i > 0 {
                lines.push(Line::from(""));
            }
            let mut head = vec![
                Span::styled(label.clone(), name_style),
                Span::styled(tag.as_ref().map(|t| format!(" {t}")).unwrap_or_default(), Style::default().fg(DIM)),
                Span::raw("  "),
                Span::styled(fmt_time(m.created_at), Style::default().fg(DIM)),
            ];
            if pings_me {
                head.push(Span::styled("  @you", Style::default().fg(MENTION).bold()));
            } else if !hl.is_empty() {
                head.push(Span::styled("  ★", Style::default().fg(HIGHLIGHT).bold()));
            }
            lines.push(Line::from(head));
        }

        let start_line = lines.len();
        if let Some(rid) = &m.reply_to {
            let target = app.msgs.iter().find(|x| &x.id == rid);
            // A reply to one of your own messages stands out; the rest stay
            // quiet quotes.
            let to_me = reply_to_me;
            let snippet = target
                .map(|x| {
                    let (text, _) = render_mentions(&x.text, |id| mention_name(app, id));
                    let who = if to_me { "reply to you".to_string() } else { app.display_name(&x.creator) };
                    preview_line(&who, &md_unescape(&text), 40)
                })
                .unwrap_or_else(|| "…".to_string());
            // Truncate against the real pane width, otherwise a narrow pane
            // clips this mid-word with no ellipsis to show it was cut.
            lines.push(Line::from(Span::styled(
                format!("  ↪ {}", one_line(&snippet, text_w.saturating_sub(2))),
                if to_me {
                    Style::default().fg(MENTION).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(DIM).add_modifier(Modifier::ITALIC)
                },
            )));
        }
        if let Some(w) = &whisper {
            let place = app
                .chats
                .iter()
                .find(|c| c.object_id == w.chat_id)
                .map_or_else(|| "another chat".to_string(), |c| c.qualified());
            let quote = format!("🔒 whisper about {} · in {place}", w.label.trim_start_matches("↪ "));
            lines.push(whisper_band(&one_line(&quote, text_w.saturating_sub(2)), text_w, Modifier::BOLD));
        }

        if !m.text.is_empty() {
            // What leads the first line: `* Name` for a `/me` action, and in
            // compact mode the time and name every line carries.
            let time = (fmt_clock(m.created_at), Style::default().fg(DIM));
            let lead: Vec<(String, Style)> = match (compact, &action) {
                (false, None) => vec![],
                (false, Some(_)) => vec![(format!("* {label}"), name_style)],
                (true, None) => vec![time, (label.clone(), name_style)],
                (true, Some(_)) => vec![time, (format!("* {label}"), name_style)],
            };
            // Compact lines carry what a header would: the id tag, and the
            // `@you` / `★` marks (compact has no header to put them on).
            let mut lead = lead;
            if compact {
                if let Some(t) = &tag {
                    lead.push((t.clone(), Style::default().fg(DIM)));
                }
                let mark = |c: Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
                if pings_me {
                    lead.push(("@you".to_string(), mark(MENTION)));
                } else if !hl.is_empty() {
                    lead.push(("★".to_string(), mark(HIGHLIGHT)));
                }
            }
            let body = action.as_deref().unwrap_or(&text);
            let lead_text: String = lead.iter().map(|(t, _)| format!("{t} ")).collect();
            // Compact continuation lines hang under the name, past the time.
            let hang = if compact { 6 } else { 0 };
            let full = format!("{lead_text}{body}");
            for (n, l) in wrap(&full, text_w.saturating_sub(hang)).into_iter().enumerate() {
                let mut spans = vec![Span::raw("  ")];
                if n > 0 && hang > 0 {
                    spans.push(Span::raw(" ".repeat(hang)));
                }
                let mut rest = l.as_str();
                if n == 0 {
                    // A name too long for the pane wraps; then the rest is
                    // plain text rather than a half-styled name.
                    for (t, st) in &lead {
                        let Some(r) = rest.strip_prefix(t.as_str()) else { break };
                        spans.push(Span::styled(t.clone(), *st));
                        spans.push(Span::raw(" "));
                        rest = r.strip_prefix(' ').unwrap_or(r);
                    }
                }
                spans.extend(mark_spans(rest, &marks));
                if action.is_some() {
                    for sp in spans.iter_mut() {
                        sp.style = sp.style.add_modifier(Modifier::ITALIC);
                    }
                }
                if n == 0 && m.edited() {
                    spans.push(Span::styled(" (edited)", Style::default().fg(DIM)));
                }
                lines.push(Line::from(spans));
            }
        }

        // Attachments can't be opened yet, but you should be able to see that
        // a message carries them.
        // One line per attachment — the file's name and size once its
        // metadata lands, a live percentage while `o`/`s` fetches it, and a
        // key hint on the message under the cursor.
        for (n, a) in m.attachments.iter().enumerate() {
            let (label, file_id) = attachment_label(app, a);
            let mut spans = vec![Span::styled(
                format!("  {}", truncate(&label, text_w.saturating_sub(18))),
                Style::default().fg(Color::Blue),
            )];
            if let Some((_, got, total)) = file_id.and_then(|id| app.downloads.get(&id)) {
                spans.push(Span::styled(
                    format!("  ⤓ {}", progress_text(*got, *total)),
                    Style::default().fg(UNREAD),
                ));
            } else if n == 0 && m.id == sel_id && app.focus == Focus::Messages {
                spans.push(Span::styled("  o open · s save", Style::default().fg(DIM)));
            }
            lines.push(Line::from(spans));
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

        // Whispers about this message, from your DMs: only the two people
        // in each DM ever see them. They sit on a full-width red band, under
        // a label that says so, so they can't be mistaken for the chat.
        let mut last_partner = String::new();
        for n in app.whispers.iter().filter(|n| n.target == m.id) {
            let peer = app.dm_peers.get(&n.dm_space).cloned().unwrap_or_default();
            let partner = if n.creator == app.me { peer.clone() } else { n.creator.clone() };
            let partner_name = app.display_name(&partner);
            if partner != last_partner {
                lines.push(whisper_band(
                    &format!("🔒 WHISPER · only you and {partner_name} can see this"),
                    text_w,
                    Modifier::BOLD,
                ));
                last_partner = partner;
            }
            let who = if n.creator == app.me {
                format!("you → {partner_name}")
            } else {
                format!("{partner_name} → you")
            };
            let (body, _) = render_mentions(&n.body, |id| mention_name(app, id));
            let full = format!("{who}: {}", md_unescape(&body));
            for l in wrap(&full, text_w.saturating_sub(2)) {
                lines.push(whisper_band(&l, text_w, Modifier::empty()));
            }
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

/// `📎 name · size` for a file (the sender's kind until its metadata
/// lands), `🔗 url` for a web link. Also the file id, to find its download.
fn attachment_label(app: &App, a: &crate::model::Attachment) -> (String, Option<String>) {
    use crate::model::AttachmentTarget;
    match a.target() {
        AttachmentTarget::File { file_id, .. } => {
            let label = match app.file_infos.get(&file_id) {
                Some(i) if !i.name.is_empty() => {
                    format!("📎 {} · {}", i.name, crate::files::human_size(i.size))
                }
                _ => format!("📎 {}", a.kind),
            };
            (label, Some(file_id))
        }
        AttachmentTarget::Url(u) => (format!("🔗 {u}"), None),
        AttachmentTarget::Object => ("🔗 linked object".to_string(), None),
        AttachmentTarget::Unknown => (format!("📎 {}", a.kind), None),
    }
}

/// "45%", or bytes so far when the size is unknown.
fn progress_text(got: u64, total: u64) -> String {
    match (got * 100).checked_div(total) {
        Some(pct) => format!("{pct}%"),
        None => crate::files::human_size(got),
    }
}

/// One line of a whisper: white on dark red, padded to the pane width so
/// the band is solid rather than ending ragged at the text (a background
/// only paints cells that hold something).
fn whisper_band(text: &str, width: usize, extra: Modifier) -> Line<'static> {
    let body = format!(" {text}");
    let pad = width.saturating_sub(body.width());
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{body}{}", " ".repeat(pad)),
            Style::default().fg(Color::White).bg(WHISPER_BG).add_modifier(extra),
        ),
    ])
}

/// Current display name for a mention identity, if the directory knows one.
fn mention_name(app: &App, identity: &str) -> Option<String> {
    app.names.get(identity).filter(|n| !n.is_empty()).cloned()
}

/// Splits one wrapped body line into spans, styling every occurrence of a
/// mark — a mention chip (`@Name`) or a `/hl` word — so it stands out from
/// the surrounding text. Marks match longest-first so `@Anna Lee` isn't
/// eaten by `@Anna`.
fn mark_spans(line: &str, marks: &[(String, Style)]) -> Vec<Span<'static>> {
    if marks.is_empty() {
        return vec![Span::raw(line.to_string())];
    }
    let mut marks: Vec<&(String, Style)> = marks.iter().filter(|(m, _)| !m.is_empty()).collect();
    marks.sort_by_key(|(m, _)| std::cmp::Reverse(m.len()));
    marks.dedup_by(|a, b| a.0 == b.0);
    let mut spans = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let hit = marks
            .iter()
            .filter_map(|(m, st)| rest.find(m.as_str()).map(|i| (i, m.len(), *st)))
            .min_by_key(|&(i, len, _)| (i, std::cmp::Reverse(len)));
        match hit {
            Some((i, len, st)) => {
                if i > 0 {
                    spans.push(Span::raw(rest[..i].to_string()));
                }
                spans.push(Span::styled(rest[i..i + len].to_string(), st));
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
    // The scope and mode are on the search view's border; the bar carries
    // what the engine reports and the count.
    if let Some(s) = &app.search {
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
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {detail}"), Style::default().fg(DIM)),
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
    if let Some(mark) = &app.prefs.away {
        let t = format!(" {mark} away");
        used += t.width();
        spans.push(Span::styled(t, Style::default().fg(DIM)));
    }
    // Incoming DM requests stay visible until accepted: a toast is too easy
    // to miss.
    if let Some(p) = app.pending_dms.first() {
        let who = if p.name.is_empty() { "someone" } else { p.name.as_str() };
        let more = match app.pending_dms.len() {
            1 => String::new(),
            n => format!(" +{}", n - 1),
        };
        let what = if p.is_dm() { "DM from" } else { "invite:" };
        let t = format!(" ✉ {what} {}{more} (/accept)", truncate(who, 16));
        used += t.width();
        spans.push(Span::styled(t, Style::default().fg(UNREAD).bold()));
    }
    // Downloads in flight: one by name, then how many more.
    if let Some((name, got, total)) = app.downloads.values().next() {
        let more = match app.downloads.len() {
            1 => String::new(),
            n => format!(" +{}", n - 1),
        };
        let t = format!(" ⤓ {} {}{more}", truncate(name, 24), progress_text(*got, *total));
        used += t.width();
        spans.push(Span::styled(t, Style::default().fg(UNREAD)));
    }
    let narrow = total < 60;

    // Position of the message cursor, shown only when you're off the bottom.
    let scroll_txt = match app.sel_msg_idx() {
        Some(i) if i + 1 < app.msgs.len() => format!("  ↑{}/{}", i + 1, app.msgs.len()),
        _ => String::new(),
    };
    // The open chat's size, right-aligned: its width is held back from the
    // unread list, and it goes first when even the short form won't fit.
    let stats_txt = match app.msg_total {
        Some(n) if app.active.is_some() => {
            let long = format!("{n} msgs · {} files · {} links ", app.files.len(), app.links.len());
            let short = format!("{n}m {}f {}l ", app.files.len(), app.links.len());
            if narrow { short } else { long }
        }
        _ => String::new(),
    };
    let stats_txt = if used + stats_txt.width() + scroll_txt.width() + 16 <= total {
        stats_txt
    } else {
        String::new()
    };
    // How the open chat's space syncs, left of the stats: state, then the
    // live paths — sync nodes, LAN peers, direct p2p (iroh) peers.
    let (sync_txt, sync_color) = match app.active_sync() {
        Some(st) => {
            let color = match st.state.as_str() {
                "synced" => ME,
                "syncing" => UNREAD,
                "offline" | "error" => Color::Red,
                _ => DIM,
            };
            (format!("{}  ", st.summary(narrow, app.direct_for(&st.space_id))), color)
        }
        None => (String::new(), DIM),
    };
    let sync_txt = if used + sync_txt.width() + stats_txt.width() + scroll_txt.width() + 16 <= total {
        sync_txt
    } else {
        String::new()
    };
    let reserve = scroll_txt.width() + stats_txt.width() + sync_txt.width();

    // Bao's presence (anybao ADR-025), the way any-ui's status bar shows it:
    // nothing until the first beat, then what the agent is doing — its own
    // status line, else the code of the cell it is running, else the run
    // title — with a ticking ellipsis and the tool-call count while a run
    // is live. `✦ bao` alone is idle.
    let (bao_txt, bao_style) = match app.bao_presence() {
        BaoPresence::Unknown => (String::new(), Style::default()),
        BaoPresence::Offline => ("  ✦ bao offline".to_string(), Style::default().fg(DIM)),
        BaoPresence::NoResponder => ("  ✦ no active bao".to_string(), Style::default().fg(DIM)),
        BaoPresence::Idle => ("  ✦ bao".to_string(), Style::default().fg(DIM)),
        BaoPresence::Working { doing, cells } => {
            let dots = ".".repeat((app.ticks % 3) as usize + 1);
            let calls = if cells > 0 {
                format!(" ({cells})")
            } else {
                String::new()
            };
            (
                format!("  ✦ {}{dots}{calls}", truncate(&doing, 48)),
                Style::default().fg(AGENT),
            )
        }
    };
    if !bao_txt.is_empty() {
        // Budgeted like everything else here; on a phone-width bar the
        // doing-text is cut to what fits rather than dropped, and the unread
        // list keeps at least a short head.
        let avail = total.saturating_sub(used + reserve + 14);
        let shown = truncate(&bao_txt, avail);
        if shown.width() > 4 {
            used += shown.width();
            spans.push(Span::styled(shown, bao_style));
        }
    }

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
            let (badge, color) = unread_badge(c).unwrap_or_default();
            let e = format!("{name} {badge}  ");
            // Leave room for a "+N" overflow marker.
            if used + e.width() + reserve + 4 > total {
                break;
            }
            spans.push(Span::styled(e.clone(), Style::default().fg(color)));
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
    if !stats_txt.is_empty() || !sync_txt.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(sync_txt, Style::default().fg(sync_color)),
                Span::styled(stats_txt, Style::default().fg(DIM)),
            ]))
            .alignment(Alignment::Right),
            area,
        );
    }
}

/// `/devices`: every device of the account — this one first, then the
/// online ones, then by last sign of life. Name, OS and `any` version come
/// from the registry, which runs bao from its election; online / last seen
/// from the p2p layer (the registry deliberately stores no liveness).
fn draw_devices(f: &mut Frame, area: Rect, app: &App) {
    let Some((devs, live)) = &app.devices_view else { return };
    let now = chrono::Utc::now().timestamp() as f64;
    let mut rows: Vec<&crate::model::Device> = devs.devices.iter().collect();
    let key = |d: &crate::model::Device| {
        let l = live.get(&d.peer_id);
        (
            d.peer_id != devs.me,
            !l.is_some_and(|l| l.connected),
            -(l.and_then(|l| l.last_seen).unwrap_or(0.0) as i64),
            d.name.clone(),
        )
    };
    rows.sort_by_key(|d| key(d));

    let os = |o: &str| match o {
        "darwin" => "macOS",
        "ios" => "iOS",
        "android" => "Android",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
    .to_string();
    let ago = |t: f64| {
        let s = (now - t).max(0.0) as u64;
        match s {
            0..=59 => "just now".to_string(),
            60..=3599 => format!("{}m ago", s / 60),
            3600..=86399 => format!("{}h ago", s / 3600),
            _ => format!("{}d ago", s / 86400),
        }
    };
    let name_w = rows.iter().map(|d| d.name.width()).max().unwrap_or(4).clamp(4, 22);
    let ver_w = rows.iter().map(|d| d.version.width()).max().unwrap_or(4).clamp(4, 26);

    let mut lines: Vec<Line> = Vec::new();
    for d in &rows {
        let me = d.peer_id == devs.me;
        let l = live.get(&d.peer_id);
        let (dot, state, color) = match l {
            _ if me => ("◆", "this device".to_string(), ACCENT),
            Some(l) if l.connected => ("●", format!("online · {}", l.via), ME),
            Some(Liveness { last_seen: Some(t), .. }) => ("○", format!("seen {}", ago(*t)), DIM),
            _ => ("○", "not seen".to_string(), DIM),
        };
        let bao = if devs.active.get("bao") == Some(&d.peer_id) {
            Span::styled("✦ bao ", Style::default().fg(AGENT).add_modifier(Modifier::BOLD))
        } else if d.apps.iter().any(|a| a == "bao") {
            Span::styled("  bao ", Style::default().fg(DIM))
        } else {
            Span::raw("      ")
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {dot} "), Style::default().fg(color)),
            Span::styled(
                format!("{:<name_w$}  ", truncate(&d.name, name_w)),
                Style::default().fg(if me { SEL } else { Color::Gray }).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{:<8}", os(&d.os)), Style::default().fg(Color::Gray)),
            Span::styled(format!("{:<ver_w$}  ", truncate(&d.version, ver_w)), Style::default().fg(DIM)),
            bao,
            Span::styled(state, Style::default().fg(color)),
        ]));
    }
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(" no devices registered", Style::default().fg(DIM))));
    }
    if live.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            " (this server doesn't report p2p peers, so online state is unknown)",
            Style::default().fg(DIM),
        )));
    }

    let content_w = lines.iter().map(|l| l.width()).max().unwrap_or(20) as u16 + 2;
    let w = content_w.max(40).min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
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
        .title(" my devices ")
        .title_bottom(Line::from(Span::styled(" r refresh · Esc close ", Style::default().fg(DIM))));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_help(f: &mut Frame, area: Rect, app: &mut App) {
    let version = app.version.clone();
    // A phone-width pane can't fit the roomy keymap, and clipped help is worse
    // than terse help.
    if area.width < 56 {
        return draw_help_compact(f, area, app);
    }
    let text = vec![
        "  Navigation",
        "    Space           fuzzy-find a chat",
        "    /               search messages",
        "    Ctrl-n / Ctrl-p next / previous chat",
        "    j / k, ↓ / ↑    move chat · move message cursor",
        "    Enter / l       step into the chat",
        "    Esc / h         back to chat list",
        "    Tab             switch pane",
        "    n               next chat with unread",
        "    g / G           oldest / newest message",
        "    Ctrl-d / Ctrl-u jump 5 messages",
        "    C-v / A-v       page down / up (PgDn / PgUp)",
        "",
        "  Search (/)",
        "    type            query (updates as you type)",
        "    Tab / S-Tab     scope: chat → space → all",
        "    Ctrl-t          hybrid ⇄ fts (exact words)",
        "    Ctrl-g          semantic only",
        "    Ctrl-o          best first ⇄ newest first",
        "    ↓ / ↑, C-v/A-v  move / page through results",
        "    Enter           jump to the message",
        "    Ctrl-r          jump there and reply",
        "    from:@name      filter by sender",
        "",
        "  Chat list",
        "    z               show / hide the chat list",
        "    C               compact: one line per message / chat",
        "                    (it hides itself anyway under 80 cols)",
        "",
        "  Messages",
        "    i               compose",
        "                    Enter sends · Alt-Enter newline",
        "    r               reply to the message under ▌",
        "    R               mark chat read now",
        "    D               DM the author under ▌",
        "    W               whisper about the message under ▌",
        "    o / s           open / save its attachments",
        "    F / L           this chat's files / links",
        "    ↑ / ↓           (composing) recall sent lines",
        "",
        "  Commands (type in the composer)",
        "    /me <action>    * Name waves",
        "    /shrug /tableflip /unflip   [text] + a face",
        "    s/old/new/[g]   edit your last message",
        "    /dm @name|id    open a DM (/dm alone: author under ▌)",
        "    /msg @name txt  send to a DM without leaving here",
        "    /w @name txt    whisper about the message under ▌",
        "    /accept [name]  accept a DM request / space invite",
        "    /join <chat>    open the best fuzzy match",
        "    /hl [word]      list / add highlight words; /unhl",
        "    /away [emoji]   mark yourself away; /back",
        "    /compact        same as C",
        "    /icons [mode]   safe / off / full: icons and emoji",
        "    /devices        your devices: online, which runs bao",
        "    /nick <name>    rename yourself (/name too)",
        "    //text          send a literal leading slash",
        "",
        "  Other",
        "    ?               toggle this help",
        "    Ctrl-L          repaint the screen",
        "    q / Ctrl-c      quit",
        "",
        "  The bottom bar tracks unread in every other chat,",
        "  live, even when the chat list is hidden.",
    ];
    let mut lines: Vec<Line> = text
        .iter()
        .map(|l| Line::from(Span::styled(*l, Style::default().fg(Color::Gray))))
        .collect();
    lines.push(Line::from(Span::styled(
        one_line(&format!("  daemon: {version}"), 58),
        Style::default().fg(DIM),
    )));
    help_box(f, area, lines, 60, &mut app.help_scroll, &mut app.help_rows);
}

fn draw_help_compact(f: &mut Frame, area: Rect, app: &mut App) {
    let version = app.version.clone();
    let text = vec![
        "  Navigate",
        "   Space    find a chat",
        "   C-n/C-p  next/prev chat",
        "   j/k      move cursor",
        "   Enter/l  step into chat",
        "   Esc/h    back to list",
        "   Tab      switch pane",
        "   n        next unread",
        "   g/G      oldest/newest",
        "   C-d/C-u  jump 5 msgs",
        "   C-v/A-v  page down/up",
        "",
        "  Chat list",
        "   z        show/hide",
        "   C        compact",
        "",
        "  Message",
        "   i        compose",
        "   A-Enter  newline",
        "   r        reply to ▌",
        "   R        mark read",
        "   D        DM author",
        "   W        whisper",
        "   o / s    open/save file",
        "   F / L    files / links",
        "",
        "  Commands",
        "   /me /shrug s/a/b/",
        "   /dm /msg /accept",
        "   /join /hl /away",
        "   /compact /icons",
        "   /devices /nick",
        "   //literal",
        "",
        "   ?  help      q  quit",
    ];
    let mut lines: Vec<Line> = text
        .iter()
        .map(|l| Line::from(Span::styled(*l, Style::default().fg(Color::Gray))))
        .collect();
    lines.push(Line::from(Span::styled(one_line(&format!("  {version}"), 28), Style::default().fg(DIM))));
    help_box(f, area, lines, 30, &mut app.help_scroll, &mut app.help_rows);
}

/// The help overlay's box, centred and at most `max_w` wide. When the text
/// is taller than the screen it scrolls — `scroll` is clamped here, since
/// only the draw knows the height — with a scrollbar on the right border and
/// the keys for it on the bottom one.
fn help_box(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>, max_w: u16, scroll: &mut usize, page: &mut usize) {
    let w = max_w.min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    let rows = h.saturating_sub(2) as usize;
    *page = rows;
    let max_scroll = lines.len().saturating_sub(rows);
    *scroll = (*scroll).min(max_scroll);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .title(" keys ");
    if max_scroll > 0 {
        block = block.title_bottom(Line::from(Span::styled(
            format!(" j/k scroll · {}/{} · Esc ", *scroll + rows, lines.len()),
            Style::default().fg(DIM),
        )));
    }
    f.render_widget(Clear, rect);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(Paragraph::new(lines).scroll((*scroll as u16, 0)), inner);
    if max_scroll > 0 {
        let mut state = ScrollbarState::new(max_scroll).position(*scroll).viewport_content_length(rows);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(ACCENT))
                .track_style(Style::default().fg(DIM)),
            rect.inner(ratatui::layout::Margin { vertical: 1, horizontal: 0 }),
            &mut state,
        );
    }
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

fn local(ts: f64) -> Option<DateTime<Local>> {
    match Local.timestamp_opt(ts as i64, 0) {
        chrono::LocalResult::Single(t) => Some(t),
        _ => None,
    }
}

/// `HH:MM`, for compact lines (the day goes in a divider).
fn fmt_clock(ts: f64) -> String {
    local(ts).map(|t| t.format("%H:%M").to_string()).unwrap_or_default()
}

fn fmt_day(ts: f64) -> String {
    local(ts).map(|t| t.format("%a %d %b %Y").to_string()).unwrap_or_default()
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

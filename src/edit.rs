//! Shared readline-style editing for every text field (the composer, the fuzzy
//! picker, and whatever comes next). All of them hold a [`tui_input::Input`] and
//! route keys through [`apply_edit_key`], so editing behaves identically
//! everywhere: cursor movement, word jumps, and the usual kill keys.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_input::{Input, InputRequest};

/// Maps a key to an editing request and applies it, returning `true` when the
/// key was consumed. Callers own higher-priority bindings (Enter, Esc, list
/// navigation): check those first, then hand the rest here.
///
/// `multiline` decides whether Ctrl-J inserts a newline — the composer wants
/// that, the single-line picker does not.
pub fn apply_edit_key(input: &mut Input, k: &KeyEvent, multiline: bool) -> bool {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);

    let req = match k.code {
        // Movement.
        KeyCode::Left if ctrl || alt => InputRequest::GoToPrevWord,
        KeyCode::Right if ctrl || alt => InputRequest::GoToNextWord,
        KeyCode::Left => InputRequest::GoToPrevChar,
        KeyCode::Right => InputRequest::GoToNextChar,
        KeyCode::Home => InputRequest::GoToStart,
        KeyCode::End => InputRequest::GoToEnd,
        KeyCode::Char('b') if ctrl => InputRequest::GoToPrevChar,
        KeyCode::Char('f') if ctrl => InputRequest::GoToNextChar,
        KeyCode::Char('b') if alt => InputRequest::GoToPrevWord,
        KeyCode::Char('f') if alt => InputRequest::GoToNextWord,
        KeyCode::Char('a') if ctrl => InputRequest::GoToStart,
        KeyCode::Char('e') if ctrl => InputRequest::GoToEnd,

        // Deletion.
        KeyCode::Backspace if alt => InputRequest::DeletePrevWord,
        KeyCode::Backspace | KeyCode::Char('h') if ctrl => InputRequest::DeletePrevChar,
        KeyCode::Backspace => InputRequest::DeletePrevChar,
        KeyCode::Delete => InputRequest::DeleteNextChar,
        KeyCode::Char('d') if ctrl => InputRequest::DeleteNextChar,
        KeyCode::Char('d') if alt => InputRequest::DeleteNextWord,
        KeyCode::Char('w') if ctrl => InputRequest::DeletePrevWord,
        KeyCode::Char('u') if ctrl => InputRequest::DeleteLine,
        KeyCode::Char('k') if ctrl => InputRequest::DeleteTillEnd,

        // A literal newline for multi-line fields (Ctrl-J; Alt/Shift-Enter is
        // handled by the caller since Enter usually means "submit").
        KeyCode::Char('j') if ctrl && multiline => InputRequest::InsertChar('\n'),

        // Plain typing. Ctrl/Alt chords that reach here are not ours to eat.
        KeyCode::Char(c) if !ctrl && !alt => InputRequest::InsertChar(c),

        _ => return false,
    };
    input.handle(req);
    true
}

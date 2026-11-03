//! `/langmap`: shortcut keys under a non-Latin keyboard layout.
//!
//! The terminal sends the character the active layout produces, never the
//! physical key: with a Cyrillic layout the `j` key arrives as `о` (or, on
//! a phonetic layout, as `й`) and no shortcut matches. There is no key code
//! to read instead — terminals, tmux and mosh carry only characters (the
//! kitty protocol's "base layout key" is the one exception, and crossterm
//! drops it while parsing). Layouts also differ too much to guess
//! (ЙЦУКЕН, several phonetic variants, …), so the map comes from the
//! keyboard itself: the user switches to their layout and types the three
//! physical rows `qwertyuiop[] asdfghjkl;' zxcvbnm,./`. What arrives is that
//! layout's character for each position, paired here with [`QWERTY`] — the
//! same idea as vim's `langmap`.
//!
//! The map is applied only where the key is a shortcut: chords (Ctrl / Alt)
//! everywhere, plain keys only outside a text field. Typing in the
//! composer, the picker or the search box stays as typed.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

/// The physical rows, left to right, top to bottom, as a US layout labels
/// them. A `/langmap` string is this sequence typed under another layout.
pub const QWERTY: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,./";

/// Named layouts that are the same everywhere, for `/langmap <name>`.
const PRESETS: &[(&str, &str)] = &[("jcuken", "йцукенгшщзхъфывапролджэячсмитьбю.")];

/// At least this many positions, so a stray word never becomes a map.
const MIN_KEYS: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub struct Langmap {
    map: HashMap<char, char>,
    /// The string it was built from, shown back by a bare `/langmap`.
    source: String,
}

impl Langmap {
    /// Builds the map from `layout`: [`QWERTY`] typed under the user's
    /// layout (whitespace between rows is fine), or a preset name. Letters
    /// map in both cases, so `о` → `j` also gives `О` → `J`.
    pub fn parse(layout: &str) -> Result<Langmap, String> {
        let layout = PRESETS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(layout.trim()))
            .map(|(_, l)| *l)
            .unwrap_or(layout);
        let chars: Vec<char> = layout.chars().filter(|c| !c.is_whitespace()).collect();
        let max = QWERTY.chars().count();
        if chars.len() < MIN_KEYS || chars.len() > max {
            return Err(format!(
                "type the keys {QWERTY} under your layout ({MIN_KEYS}–{max} of them, left to right, top to bottom)"
            ));
        }
        let mut map = HashMap::new();
        for (from, to) in chars.iter().copied().zip(QWERTY.chars()) {
            if from == to {
                continue;
            }
            map.entry(from).or_insert(to);
            if from.is_alphabetic() && to.is_ascii_alphabetic() {
                if let (Some(fu), Some(tu)) = (single(from.to_uppercase()), single(to.to_uppercase())) {
                    map.entry(fu).or_insert(tu);
                }
            }
        }
        Ok(Langmap { map, source: chars.iter().collect() })
    }

    /// The string the map was built from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The Latin character on the physical key that produces `c`, if the
    /// map knows that key.
    pub fn latin(&self, c: char) -> Option<char> {
        self.map.get(&c).copied()
    }

    /// `k` with its character mapped when the key is a shortcut: always for
    /// a Ctrl / Alt chord, and for a plain key only when `typing` is false
    /// (no text field has the keyboard).
    pub fn translate(&self, k: KeyEvent, typing: bool) -> KeyEvent {
        let chord = k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if typing && !chord {
            return k;
        }
        match k.code {
            KeyCode::Char(c) => match self.latin(c) {
                Some(l) => KeyEvent { code: KeyCode::Char(l), ..k },
                None => k,
            },
            _ => k,
        }
    }
}

fn single(mut it: impl Iterator<Item = char>) -> Option<char> {
    let c = it.next()?;
    it.next().is_none().then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHONETIC: &str = "яверты уиопшщ асдфгх йкльэ зжцвбнм,./";

    fn key(c: char, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), m)
    }

    #[test]
    fn presets_cover_the_rows() {
        for (_, l) in PRESETS {
            assert_eq!(l.chars().count(), QWERTY.chars().count());
        }
    }

    #[test]
    fn rows_pair_with_qwerty_positions() {
        let m = Langmap::parse(PHONETIC).unwrap();
        assert_eq!(m.latin('й'), Some('j'));
        assert_eq!(m.latin('к'), Some('k'));
        assert_eq!(m.latin('я'), Some('q'));
        assert_eq!(m.latin('З'), Some('Z'));
        assert_eq!(m.latin(','), None, "identity keys are not mapped");
        assert_eq!(m.latin('j'), None);
        assert_eq!(m.source(), PHONETIC.replace(' ', ""));
    }

    #[test]
    fn preset_by_name() {
        let m = Langmap::parse("JCUKEN").unwrap();
        assert_eq!(m.latin('о'), Some('j'));
        assert_eq!(m.latin('Й'), Some('Q'));
        assert_eq!(m.latin('.'), Some('/'), "the / key gives . in ЙЦУКЕН");
    }

    #[test]
    fn a_prefix_of_the_rows_is_enough() {
        let m = Langmap::parse("явертыуиоп").unwrap();
        assert_eq!(m.latin('п'), Some('p'));
        assert_eq!(m.latin('й'), None);
    }

    #[test]
    fn too_short_or_too_long_is_refused() {
        assert!(Langmap::parse("яверты").is_err());
        assert!(Langmap::parse(&format!("{PHONETIC}ё")).is_err());
    }

    #[test]
    fn plain_key_maps_only_outside_a_text_field() {
        let m = Langmap::parse(PHONETIC).unwrap();
        let k = key('й', KeyModifiers::NONE);
        assert_eq!(m.translate(k, false).code, KeyCode::Char('j'));
        assert_eq!(m.translate(k, true).code, KeyCode::Char('й'));
    }

    #[test]
    fn chords_map_everywhere_and_keep_modifiers() {
        let m = Langmap::parse(PHONETIC).unwrap();
        let t = m.translate(key('н', KeyModifiers::CONTROL), true);
        assert_eq!(t.code, KeyCode::Char('n'));
        assert_eq!(t.modifiers, KeyModifiers::CONTROL);
        assert_eq!(m.translate(key('ф', KeyModifiers::ALT), true).code, KeyCode::Char('f'));
        let k = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(m.translate(k, false), k);
    }
}

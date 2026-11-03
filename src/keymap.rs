//! `/langmap`: shortcut keys under a non-Latin keyboard layout.
//!
//! The terminal sends the character the active layout produces, never the
//! physical key: with a Cyrillic layout the `j` key arrives as `о` (or, on
//! a phonetic layout, as `й`) and no shortcut matches. There is no key code
//! to read instead — terminals, tmux and mosh carry only characters (the
//! kitty protocol's "base layout key" is the one exception, and crossterm
//! drops it while parsing). Layouts also differ too much to guess
//! (ЙЦУКЕН, several phonetic variants, and the Latin side may be Colemak
//! rather than QWERTY), so the map comes from the keyboard itself: the user
//! types their keyboard's rows in the Latin layout, then the same keys in
//! the other layout, and the two are paired position by position — the
//! same idea as vim's `langmap`.
//!
//! The map is applied only where the key is a shortcut: chords (Ctrl / Alt)
//! everywhere, plain keys only outside a text field. Typing in the
//! composer, the picker or the search box stays as typed.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

/// Named layouts that are the same everywhere, for `/langmap <name>`:
/// the QWERTY rows, then the same keys under that layout.
const PRESETS: &[(&str, &str)] = &[(
    "jcuken",
    "qwertyuiop[]asdfghjkl;'zxcvbnm,./ йцукенгшщзхъфывапролджэячсмитьбю.",
)];

/// What a bare `/langmap` says when nothing is set.
pub const HINT: &str = "two strings: the keys typed in English, then the same keys typed in the local layout — e.g. /langmap qwertyuiop явертыуиоп";

/// At least this many keys, so a stray word never becomes a map.
const MIN_KEYS: usize = 10;

#[derive(Clone, Debug, PartialEq)]
pub struct Langmap {
    map: HashMap<char, char>,
    /// The string it was built from, shown back by a bare `/langmap`.
    source: String,
}

impl Langmap {
    /// Builds the map from `layout`: the keyboard's rows typed under the
    /// user's Latin layout, then the same keys typed under the other
    /// layout, paired position by position — so neither the Latin layout
    /// (QWERTY, Colemak, …) nor the board (a 3x5 split has no `[` `]` `'`)
    /// is assumed. Whitespace splits the text into groups; the first half
    /// of the groups is one layout, the second half the other, and the
    /// ASCII half is taken as the Latin one whichever came first. A preset
    /// name stands for a known pair. Letters map in both cases, so
    /// `о` → `j` also gives `О` → `J`.
    pub fn parse(layout: &str) -> Result<Langmap, String> {
        let layout = PRESETS
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(layout.trim()))
            .map(|(_, l)| *l)
            .unwrap_or(layout);
        let groups: Vec<&str> = layout.split_whitespace().collect();
        if groups.is_empty() || groups.len() % 2 != 0 {
            return Err(HINT.to_string());
        }
        let (a, b) = groups.split_at(groups.len() / 2);
        let a: Vec<char> = a.concat().chars().collect();
        let b: Vec<char> = b.concat().chars().collect();
        let ascii = |v: &[char]| v.iter().all(char::is_ascii);
        let (latin, other) = if ascii(&a) {
            (a, b)
        } else if ascii(&b) {
            (b, a)
        } else {
            return Err(format!("one half must be your Latin layout — {HINT}"));
        };
        if latin.len() != other.len() {
            return Err(format!(
                "{} keys in one layout, {} in the other — type the same keys both times",
                latin.len(),
                other.len()
            ));
        }
        if latin.len() < MIN_KEYS {
            return Err(format!("{MIN_KEYS} keys at least — {HINT}"));
        }
        let mut map = HashMap::new();
        for (from, to) in other.iter().copied().zip(latin.iter().copied()) {
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
        let source = format!("{} {}", latin.iter().collect::<String>(), other.iter().collect::<String>());
        Ok(Langmap { map, source })
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

    // A full-width board: 12 / 11 / 10 keys, one group per row.
    // A Colemak-DH 3x5 board with a Russian phonetic layout on top: the
    // same keys typed in the Latin layout, then in the Russian one.
    const COLEMAK: &str = "qwfpbjluy;arstgmneiozxcdvkh,./";
    const PHONETIC_OVER_COLEMAK: &str = "явфпбйлуыьарстгмнеиозжцдвкх,./";

    fn key(c: char, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), m)
    }

    fn colemak() -> Langmap {
        Langmap::parse(&format!("{COLEMAK} {PHONETIC_OVER_COLEMAK}")).unwrap()
    }

    #[test]
    fn presets_pair_up() {
        for (name, _) in PRESETS {
            Langmap::parse(name).unwrap();
        }
    }

    #[test]
    fn keys_pair_position_by_position() {
        let m = colemak();
        assert_eq!(m.latin('й'), Some('j'));
        assert_eq!(m.latin('ф'), Some('f'));
        assert_eq!(m.latin('н'), Some('n'));
        assert_eq!(m.latin('ь'), Some(';'));
        assert_eq!(m.latin('З'), Some('Z'));
        assert_eq!(m.latin(','), None, "identity keys are not mapped");
        assert_eq!(m.latin('j'), None);
        assert_eq!(m.source(), format!("{COLEMAK} {PHONETIC_OVER_COLEMAK}"));
    }

    #[test]
    fn halves_may_come_in_either_order_and_in_rows() {
        let swapped = Langmap::parse(&format!("{PHONETIC_OVER_COLEMAK} {COLEMAK}")).unwrap();
        assert_eq!(swapped, colemak());
        let rows = Langmap::parse("qwfpbjluy; arstgmneio zxcdvkh,./ явфпбйлуыь арстгмнеио зжцдвкх,./").unwrap();
        assert_eq!(rows, colemak());
    }

    #[test]
    fn preset_by_name() {
        let m = Langmap::parse("JCUKEN").unwrap();
        assert_eq!(m.latin('о'), Some('j'));
        assert_eq!(m.latin('Й'), Some('Q'));
        assert_eq!(m.latin('.'), Some('/'), "the / key gives . in ЙЦУКЕН");
    }

    #[test]
    fn mismatches_are_refused() {
        assert!(Langmap::parse(COLEMAK).is_err(), "one layout only");
        assert!(Langmap::parse("qwfpbjluy; явфпбйлуыь зжц").is_err(), "odd groups");
        assert!(Langmap::parse("qwfpbjluy;arst явфпбйлуыь").is_err(), "lengths differ");
        assert!(Langmap::parse("qwfpb явфпб").is_err(), "too short");
        assert!(Langmap::parse("явфпбйлуыь йцукенгшщз").is_err(), "no Latin half");
    }

    #[test]
    fn plain_key_maps_only_outside_a_text_field() {
        let m = colemak();
        let k = key('й', KeyModifiers::NONE);
        assert_eq!(m.translate(k, false).code, KeyCode::Char('j'));
        assert_eq!(m.translate(k, true).code, KeyCode::Char('й'));
    }

    #[test]
    fn chords_map_everywhere_and_keep_modifiers() {
        let m = colemak();
        let t = m.translate(key('н', KeyModifiers::CONTROL), true);
        assert_eq!(t.code, KeyCode::Char('n'));
        assert_eq!(t.modifiers, KeyModifiers::CONTROL);
        assert_eq!(m.translate(key('ф', KeyModifiers::ALT), true).code, KeyCode::Char('f'));
        let k = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(m.translate(k, false), k);
    }
}

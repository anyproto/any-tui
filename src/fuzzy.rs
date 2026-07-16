//! Small fuzzy matcher for the chat picker, in the spirit of fzf/helix:
//! a subsequence match, scored so that tight, word-aligned hits win.

/// Returns `(score, matched_char_indices)` if `needle` is a subsequence of
/// `hay`, case-insensitively. Indices are into `hay`'s `chars()`, so the caller
/// can highlight them. An empty needle matches everything with score 0.
pub fn fuzzy_match(hay: &str, needle: &str) -> Option<(i32, Vec<usize>)> {
    if needle.trim().is_empty() {
        return Some((0, Vec::new()));
    }
    let h: Vec<char> = hay.chars().collect();
    // Fold per char to keep indices aligned with `h`; `str::to_lowercase` can
    // change the char count for some scripts.
    let hl: Vec<char> = h.iter().map(|c| lower(*c)).collect();
    let n: Vec<char> = needle
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(lower)
        .collect();
    if n.is_empty() {
        return Some((0, Vec::new()));
    }

    let mut idx = Vec::with_capacity(n.len());
    let mut hi = 0usize;
    for &nc in &n {
        let mut found = false;
        while hi < hl.len() {
            if hl[hi] == nc {
                idx.push(hi);
                hi += 1;
                found = true;
                break;
            }
            hi += 1;
        }
        if !found {
            return None;
        }
    }

    let mut score = 0i32;
    let mut prev: Option<usize> = None;
    for &i in &idx {
        if let Some(p) = prev {
            if i == p + 1 {
                score += 8; // consecutive run
            } else {
                // Opening a gap costs more than widening it. Without a real
                // fixed cost here, a scattered run of word-start hits
                // ("g-e-n-x") outscores a tight one ("gen").
                score -= 3 + ((i - p - 2).min(5)) as i32;
            }
        }
        // Start of a word is a strong signal ("sy g" -> "sync team: general").
        if i == 0 || !h[i - 1].is_alphanumeric() {
            score += 10;
        }
        if h[i].is_uppercase() {
            score += 2;
        }
        prev = Some(i);
    }
    // Prefer matches that start early, and shorter haystacks on equal footing.
    score -= (idx[0] as i32) / 2;
    score -= (h.len() as i32) / 16;
    Some((score, idx))
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_subsequence_and_rejects_non_match() {
        assert!(fuzzy_match("sync team: general", "sync").is_some());
        assert!(fuzzy_match("sync team: general", "stg").is_some());
        assert!(fuzzy_match("sync team: general", "zzz").is_none());
    }

    #[test]
    fn empty_needle_matches() {
        assert_eq!(fuzzy_match("anything", "").unwrap().0, 0);
    }

    #[test]
    fn is_case_insensitive_and_reports_indices() {
        let (_, idx) = fuzzy_match("Sync Team", "st").unwrap();
        assert_eq!(idx, vec![0, 5]);
    }

    #[test]
    fn word_starts_outrank_mid_word_hits() {
        let word_start = fuzzy_match("sync team: general", "tg").unwrap().0;
        let mid_word = fuzzy_match("stuff: aggregate", "tg").unwrap().0;
        assert!(word_start > mid_word, "{word_start} vs {mid_word}");
    }

    #[test]
    fn consecutive_beats_scattered() {
        let tight = fuzzy_match("general", "gen").unwrap().0;
        let loose = fuzzy_match("g-e-n-x", "gen").unwrap().0;
        assert!(tight > loose, "{tight} vs {loose}");
    }

    #[test]
    fn handles_non_ascii() {
        assert!(fuzzy_match("Кость: привет", "кость").is_some());
    }
}

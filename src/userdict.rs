//! Per-user IME memory: which words the user picked for which pinyin, plus
//! word-to-word follow pairs for suggestions (联想). Plain text at
//! %APPDATA%\fastrans\userdict.txt, loaded at startup, saved on hide/quit.
//!
//! Line format:
//!   e\t<pinyin>\t<word>\t<count>   picked `word` for `pinyin` count times
//!   b\t<prev>\t<next>\t<count>     typed `next` right after `prev`
//!
//! `counts` (the conversion-boost table) is always derived from the `e`
//! entries, so it round-trips exactly. Follow picks bump only bigrams: a
//! 联想 pick was never chosen *for a pinyin*, so it must not skew conversion.

use std::collections::HashMap;
use std::path::PathBuf;

/// Entries beyond these caps are pruned (lowest count first) at save time —
/// applied to the live maps too, so memory and disk stay in sync.
const MAX_ENTRIES: usize = 4000;
const MAX_BIGRAMS: usize = 4000;
/// How many follow-up suggestions the UI shows.
const MAX_SUGGESTIONS: usize = 6;

#[derive(Default)]
pub struct UserDict {
    /// pinyin (as typed, lowercased) -> picked words with counts.
    entries: HashMap<String, Vec<(String, u32)>>,
    /// word -> total pick count (rank boost in conversion); derived from
    /// `entries`, rebuilt after load and after pruning.
    counts: HashMap<String, u32>,
    /// prev word -> next words with counts (联想).
    bigrams: HashMap<String, Vec<(String, u32)>>,
    dirty: bool,
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("fastrans").join("userdict.txt"))
}

fn bump(list: &mut Vec<(String, u32)>, word: &str, by: u32) {
    match list.iter_mut().find(|(w, _)| w == word) {
        Some((_, c)) => *c += by,
        None => list.push((word.to_string(), by)),
    }
    list.sort_by(|a, b| b.1.cmp(&a.1));
}

/// Rejects strings that would corrupt the line-oriented file format.
fn clean(s: &str) -> bool {
    !s.is_empty() && !s.contains(['\t', '\n', '\r'])
}

impl UserDict {
    pub fn load() -> Self {
        let mut s = Self::default();
        let Some(p) = path() else { return s };
        let Ok(text) = std::fs::read_to_string(p) else {
            return s;
        };
        for line in text.lines() {
            let mut it = line.split('\t');
            match (it.next(), it.next(), it.next(), it.next()) {
                (Some("e"), Some(py), Some(word), Some(n)) if clean(py) && clean(word) => {
                    let Ok(n) = n.parse::<u32>() else { continue };
                    bump(s.entries.entry(py.to_string()).or_default(), word, n);
                }
                (Some("b"), Some(prev), Some(next), Some(n)) if clean(prev) && clean(next) => {
                    let Ok(n) = n.parse::<u32>() else { continue };
                    bump(s.bigrams.entry(prev.to_string()).or_default(), next, n);
                }
                _ => {}
            }
        }
        s.rebuild_counts();
        s
    }

    fn rebuild_counts(&mut self) {
        self.counts.clear();
        for words in self.entries.values() {
            for (w, n) in words {
                *self.counts.entry(w.clone()).or_default() += n;
            }
        }
    }

    /// True when the user has no pick history yet (fast path: no boosts).
    pub fn is_untrained(&self) -> bool {
        self.counts.is_empty()
    }

    /// Records a candidate pick: `pinyin` produced `word`.
    pub fn record_pick(&mut self, pinyin: &str, word: &str, prev: Option<&str>) {
        if !clean(pinyin) || !clean(word) {
            return;
        }
        bump(
            self.entries.entry(pinyin.to_ascii_lowercase()).or_default(),
            word,
            1,
        );
        *self.counts.entry(word.to_string()).or_default() += 1;
        if let Some(prev) = prev.filter(|p| clean(p)) {
            bump(self.bigrams.entry(prev.to_string()).or_default(), word, 1);
        }
        self.dirty = true;
    }

    /// Past picks whose pinyin is a prefix of `run` (lowercased ASCII):
    /// probes each prefix length directly, so cost is O(len(run)) hash
    /// lookups, independent of how much has been learned.
    pub fn prefix_matches(&self, run: &str) -> Vec<(usize, &str, u32)> {
        if self.entries.is_empty() || !run.is_ascii() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for k in 1..=run.len() {
            if let Some(words) = self.entries.get(&run[..k]) {
                for (w, n) in words.iter().take(2) {
                    out.push((k, w.as_str(), *n));
                }
            }
        }
        out.sort_by(|a, b| b.0.cmp(&a.0).then(b.2.cmp(&a.2)));
        out.truncate(4);
        out
    }

    /// Bumps only the follow-pair statistics: a 联想 pick has no pinyin and
    /// must not skew pinyin conversion scores.
    pub fn record_follow(&mut self, prev: &str, word: &str) {
        if !clean(prev) || !clean(word) {
            return;
        }
        bump(self.bigrams.entry(prev.to_string()).or_default(), word, 1);
        self.dirty = true;
    }

    /// Total pick count of a word (used as a rank boost in conversion).
    pub fn count(&self, word: &str) -> u32 {
        self.counts.get(word).copied().unwrap_or(0)
    }

    /// Follow-up suggestions (联想) after committing `prev`, best first.
    pub fn suggestions(&self, prev: &str) -> Vec<String> {
        self.bigrams
            .get(prev)
            .map(|v| {
                v.iter()
                    .take(MAX_SUGGESTIONS)
                    .map(|(w, _)| w.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Prunes the live maps to the persistence caps (lowest counts dropped),
    /// keeping in-memory behaviour identical to what the next load sees.
    fn prune(&mut self) {
        fn prune_map(map: &mut HashMap<String, Vec<(String, u32)>>, cap: usize) {
            let total: usize = map.values().map(Vec::len).sum();
            if total <= cap {
                return;
            }
            let mut all: Vec<u32> = map
                .values()
                .flat_map(|v| v.iter().map(|(_, n)| *n))
                .collect();
            all.sort_unstable_by(|a, b| b.cmp(a));
            let threshold = all[cap - 1];
            let mut budget = cap;
            for v in map.values_mut() {
                v.retain(|(_, n)| {
                    let keep = *n >= threshold && budget > 0;
                    if keep {
                        budget -= 1;
                    }
                    keep
                });
            }
            map.retain(|_, v| !v.is_empty());
        }
        prune_map(&mut self.entries, MAX_ENTRIES);
        prune_map(&mut self.bigrams, MAX_BIGRAMS);
        self.rebuild_counts();
    }

    pub fn save_if_dirty(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(p) = path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        self.prune();
        let mut out = String::new();
        for (py, words) in &self.entries {
            for (w, n) in words {
                out.push_str(&format!("e\t{py}\t{w}\t{n}\n"));
            }
        }
        for (prev, nexts) in &self.bigrams {
            for (w, n) in nexts {
                out.push_str(&format!("b\t{prev}\t{w}\t{n}\n"));
            }
        }
        // Atomic-ish: write a temp file, then rename over the target, so a
        // crash mid-write can't truncate the learned data. Only a successful
        // rename clears the dirty flag (a failed save retries next time).
        let tmp = p.with_extension("txt.tmp");
        if std::fs::write(&tmp, out).is_ok() && std::fs::rename(&tmp, &p).is_ok() {
            self.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_ranks() {
        let mut d = UserDict::default();
        d.record_pick("nihao", "你号", None);
        d.record_pick("nihao", "你好", None);
        d.record_pick("nihao", "你好", Some("在吗"));
        let m = d.prefix_matches("nihao");
        assert_eq!((m[0].0, m[0].1), (5, "你好"));
        assert_eq!(d.count("你好"), 2);
        assert_eq!(d.suggestions("在吗"), vec!["你好".to_string()]);
        assert!(d.suggestions("没有").is_empty());
        // Follow picks influence bigrams only, not conversion counts.
        d.record_follow("你好", "世界");
        assert_eq!(d.count("世界"), 0);
        assert_eq!(d.suggestions("你好"), vec!["世界".to_string()]);
        // Malformed input is rejected.
        d.record_pick("a\tb", "x", None);
        d.record_pick("ab", "", None);
        assert!(d.prefix_matches("a\tb").is_empty());
    }

    #[test]
    fn prefix_matches_probes_prefixes() {
        let mut d = UserDict::default();
        d.record_pick("ni", "你", None);
        d.record_pick("nihao", "你好", None);
        let m = d.prefix_matches("nihaoma");
        assert_eq!((m[0].0, m[0].1), (5, "你好"));
        assert_eq!((m[1].0, m[1].1), (2, "你"));
        assert!(d.prefix_matches("hao").is_empty());
    }
}

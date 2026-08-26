//! Built-in pinyin -> Chinese conversion, for machines without a Chinese IME.
//!
//! When the input bar's text ends in a run of ASCII letters, the app treats it
//! as pinyin: candidates appear under the input, digits 1-9 pick one, space
//! picks the first, and Enter auto-converts the rest via the best path.
//!
//! Conversion is a single joint Viterbi over byte positions: word edges are
//! found by walking syllable decompositions (so "keneng" considers both
//! ke'neng -> 可能 and ken'eng, and the dictionary score decides). Syllables
//! are interned to integer IDs and word keys are packed into a u128, so the
//! hot path builds no strings until the final output.
//!
//! Dictionary: AOSP PinyinIME rawdict (Apache 2.0), 65k entries with
//! frequencies, preprocessed into assets/pinyin_dict.txt as
//! `word\tfreq\tsyl1'syl2`.

use std::collections::{HashMap, HashSet};

use crate::userdict::UserDict;

const DICT: &str = include_str!("../assets/pinyin_dict.txt");
/// Weight of the user's pick history in conversion scoring.
const USER_BOOST: f64 = 1.5;
/// Boost ceiling: without it, heavily-picked short words out-bid correct
/// longer words and whole-line conversion fragments over time.
const USER_BOOST_CAP: f64 = 3.5;
/// Total candidate cap (paged 5 at a time in the UI).
const MAX_CANDIDATES: usize = 45;

fn user_boost(user: &UserDict, word: &str) -> f64 {
    let n = user.count(word);
    if n == 0 {
        0.0
    } else {
        (USER_BOOST * (1.0 + n as f64).ln()).min(USER_BOOST_CAP)
    }
}
/// Longest word (in syllables) we look up.
const MAX_WORD_SYLS: usize = 8;
/// Bits per syllable id in a packed key (416 syllables < 512).
const SYL_BITS: u32 = 9;
/// Score for a syllable that matches no dictionary word (emitted as letters).
const UNKNOWN_PENALTY: f64 = -30.0;
/// Per-syllable penalty for an abbreviated (简拼) match in the sentence
/// lattice: full-pinyin parses must always win when they exist, but a
/// jianpin edge must still beat the raw-letters fallback.
const ABBREV_PENALTY: f64 = -5.0;
/// Extra per-word cost for jianpin edges, so fewer/longer words win over
/// chains of ultra-frequent single characters ("wmdl" -> 我们+到了, not
/// 我们+的+了).
const ABBREV_EDGE_COST: f64 = -2.5;
/// Runs longer than this are not analyzed (pasted ASCII blobs).
const MAX_RUN_BYTES: usize = 64;

pub struct PinyinDict {
    /// Interned syllable spellings, id = index + 1 (0 is reserved).
    syl_text: Vec<&'static str>,
    /// spelling -> id.
    syl_ids: HashMap<&'static str, u16>,
    /// packed syllable-id key -> (word, ln(p)) sorted by descending score.
    words: HashMap<u128, Vec<(&'static str, f64)>>,
    /// Every syllable-prefix of every word key (including full keys).
    key_prefixes: HashSet<u128>,
    /// 简拼: packed first-letter key ("hl" for hui'lai) -> top words.
    /// Typing just the initials surfaces the word, like Microsoft Pinyin.
    abbrev: HashMap<u64, Vec<(&'static str, f64)>>,
    max_syllable_len: usize,
}

/// Packs 1..=8 lowercase letters into a u64 jianpin key.
fn pack_initials(letters: &[u8]) -> Option<u64> {
    if !(1..=8).contains(&letters.len()) {
        return None;
    }
    let mut key = 0u64;
    for &b in letters {
        if !b.is_ascii_lowercase() {
            return None;
        }
        key = (key << 5) | (b - b'a' + 1) as u64;
    }
    Some(key)
}

/// One selectable candidate: `text` replaces the first `consumed_bytes` of the
/// pinyin run.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub text: String,
    pub consumed_bytes: usize,
    /// True for the whole-line conversion candidate: committing it should
    /// memorize its per-word path, not the blob.
    pub sentence: bool,
}

/// The result of analyzing a pinyin run.
#[derive(Debug, Default)]
pub struct Analysis {
    pub candidates: Vec<Candidate>,
    /// Whole-run conversion: best path over convertible prefix + leftover
    /// letters.
    pub best_line: String,
    /// The best path's dictionary words with their pinyin byte spans in the
    /// run — committing the conversion records each (pinyin, word) pair, so
    /// the IME learns from natural typing, not just explicit digit picks.
    pub path: Vec<(String, usize, usize)>,
}

fn pack(key: u128, syl_id: u16) -> u128 {
    (key << SYL_BITS) | syl_id as u128
}

impl PinyinDict {
    pub fn load() -> Self {
        let mut syl_text: Vec<&'static str> = Vec::with_capacity(512);
        let mut syl_ids: HashMap<&'static str, u16> = HashMap::with_capacity(512);
        let mut words: HashMap<u128, Vec<(&'static str, f64)>> = HashMap::with_capacity(50_000);
        let mut key_prefixes: HashSet<u128> = HashSet::with_capacity(80_000);
        let mut total = 0.0_f64;

        let mut intern = |s: &'static str, syl_text: &mut Vec<&'static str>| -> u16 {
            *syl_ids.entry(s).or_insert_with(|| {
                syl_text.push(s);
                syl_text.len() as u16
            })
        };

        let mut abbrev: HashMap<u64, Vec<(&'static str, f64)>> = HashMap::with_capacity(20_000);
        for line in DICT.lines() {
            let mut it = line.split('\t');
            let (Some(word), Some(freq), Some(syls)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let Ok(freq) = freq.parse::<f64>() else {
                continue;
            };
            let freq = freq.max(f64::MIN_POSITIVE);
            total += freq;
            let mut key = 0u128;
            let mut n = 0;
            let mut initials = [0u8; 8];
            for s in syls.split('\'') {
                key = pack(key, intern(s, &mut syl_text));
                key_prefixes.insert(key);
                if n < 8 {
                    initials[n] = s.as_bytes().first().copied().unwrap_or(0);
                }
                n += 1;
            }
            if n == 0 || n > MAX_WORD_SYLS {
                continue;
            }
            words.entry(key).or_default().push((word, freq.ln()));
            if let Some(jp) = pack_initials(&initials[..n]) {
                abbrev.entry(jp).or_default().push((word, freq.ln()));
            }
        }
        // Normalize to log-probabilities: every token then scores negative, so
        // the Viterbi path prefers fewer/longer words ("今天" over "进"+"天").
        let ln_total = total.ln();
        for v in words.values_mut() {
            for (_, s) in v.iter_mut() {
                *s -= ln_total;
            }
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
        for v in abbrev.values_mut() {
            for (_, s) in v.iter_mut() {
                *s -= ln_total;
            }
            v.sort_by(|a, b| b.1.total_cmp(&a.1));
            v.truncate(12);
        }
        let max_syllable_len = syl_text.iter().map(|s| s.len()).max().unwrap_or(6);
        Self {
            syl_text,
            syl_ids,
            words,
            key_prefixes,
            abbrev,
            max_syllable_len,
        }
    }

    /// Word edges starting at byte `j`: every (end_byte, key) whose syllable
    /// join is a dictionary key or key prefix, found by bounded DFS over
    /// syllable decompositions. Calls `emit(end_byte, key, is_word)`.
    fn walk_words(&self, run: &[u8], j: usize, mut emit: impl FnMut(usize, u128)) {
        // Iterative DFS stack: (byte_pos, key, depth).
        let mut stack: Vec<(usize, u128, usize)> = vec![(j, 0, 0)];
        while let Some((pos, key, depth)) = stack.pop() {
            if depth >= MAX_WORD_SYLS {
                continue;
            }
            let start = if depth > 0 && pos < run.len() && run[pos] == b'\'' {
                pos + 1
            } else {
                pos
            };
            let max_end = (start + self.max_syllable_len).min(run.len());
            for end in (start + 1)..=max_end {
                let piece = &run[start..end];
                if piece.contains(&b'\'') {
                    break;
                }
                // run is validated ASCII, so this is always valid UTF-8.
                let piece = std::str::from_utf8(piece).unwrap();
                let Some(&id) = self.syl_ids.get(piece) else {
                    continue;
                };
                let key = pack(key, id);
                if !self.key_prefixes.contains(&key) {
                    continue;
                }
                if self.words.contains_key(&key) {
                    emit(end, key);
                }
                stack.push((end, key, depth + 1));
            }
        }
    }

    /// Single syllables at byte `j` (for the unknown-word fallback).
    fn syllables_at(&self, run: &[u8], j: usize) -> Vec<usize> {
        let mut ends = Vec::new();
        let max_end = (j + self.max_syllable_len).min(run.len());
        for end in (j + 1)..=max_end {
            let piece = &run[j..end];
            if piece.contains(&b'\'') {
                break;
            }
            if self.syl_ids.contains_key(std::str::from_utf8(piece).unwrap()) {
                ends.push(end);
            }
        }
        ends
    }

    /// Analyzes a pinyin run: candidate list + whole-run best conversion.
    /// Non-pinyin input (non-ASCII, too long) returns the run unchanged.
    /// `user` biases both ranking and conversion toward past picks.
    pub fn analyze(&self, run: &str, user: &UserDict) -> Analysis {
        if run.len() > MAX_RUN_BYTES
            || !run.bytes().all(|b| b.is_ascii_alphabetic() || b == b'\'')
        {
            return Analysis {
                candidates: Vec::new(),
                best_line: run.to_string(),
                path: Vec::new(),
            };
        }
        let lower = run.to_ascii_lowercase();
        let bytes = lower.as_bytes();
        let orig = run.as_bytes();
        let n = bytes.len();

        // Joint Viterbi over byte positions. dp[i] = best (score, prev, word).
        #[derive(Clone)]
        struct State {
            score: f64,
            prev: usize,
            word: &'static str,
            /// Fallback: emit these bytes as-is (unknown syllable).
            raw: Option<(usize, usize)>,
        }
        let mut dp: Vec<Option<State>> = vec![None; n + 1];
        dp[0] = Some(State {
            score: 0.0,
            prev: 0,
            word: "",
            raw: None,
        });
        for j in 0..n {
            let Some(base) = dp[j].clone() else { continue };
            if bytes[j] == b'\'' {
                let cand = State {
                    score: base.score,
                    prev: j,
                    word: "",
                    raw: None,
                };
                if dp[j + 1].as_ref().is_none_or(|s| cand.score > s.score) {
                    dp[j + 1] = Some(cand);
                }
                continue;
            }
            self.walk_words(bytes, j, |end, key| {
                // Among words sharing this key, prefer the user's picks.
                // Untrained users skip the scan entirely; trained ones scan
                // the full list so a deep-ranked remembered word can still
                // win (the candidate row uses the same rule).
                let words = &self.words[&key];
                let (word, gain) = if user.is_untrained() {
                    words[0]
                } else {
                    let mut best = (words[0].0, words[0].1 + user_boost(user, words[0].0));
                    for &(w, g) in &words[1..] {
                        let g = g + user_boost(user, w);
                        if g > best.1 {
                            best = (w, g);
                        }
                    }
                    best
                };
                let score = base.score + gain;
                if dp[end].as_ref().is_none_or(|s| score > s.score) {
                    dp[end] = Some(State {
                        score,
                        prev: j,
                        word,
                        raw: None,
                    });
                }
            });
            // Unknown fallback: a valid syllable with no word entry, kept as
            // letters so the rest of the line still converts.
            for end in self.syllables_at(bytes, j) {
                let score = base.score + UNKNOWN_PENALTY;
                if dp[end].as_ref().is_none_or(|s| score > s.score) {
                    dp[end] = Some(State {
                        score,
                        prev: j,
                        word: "",
                        raw: Some((j, end)),
                    });
                }
            }
            // 简拼 word edges: L letters taken as L syllable initials, so a
            // word is reachable from its abbreviation ("wm" -> 我们) and full
            // pinyin mixes freely with jianpin ("w" + "buxiangquchifan").
            // The per-syllable penalty keeps full parses on top; uppercase
            // letters mean deliberate literal text and are never jianpin.
            for len in 1..=MAX_WORD_SYLS.min(n - j) {
                if bytes[j + len - 1] == b'\'' || orig[j + len - 1].is_ascii_uppercase() {
                    break;
                }
                let Some(ws) =
                    pack_initials(&bytes[j..j + len]).and_then(|k| self.abbrev.get(&k))
                else {
                    continue;
                };
                let (word, gain) = if user.is_untrained() {
                    ws[0]
                } else {
                    let mut best = (ws[0].0, ws[0].1 + user_boost(user, ws[0].0));
                    for &(w, g) in &ws[1..] {
                        let g = g + user_boost(user, w);
                        if g > best.1 {
                            best = (w, g);
                        }
                    }
                    best
                };
                let score = base.score + gain + ABBREV_PENALTY * len as f64 + ABBREV_EDGE_COST;
                if dp[j + len].as_ref().is_none_or(|s| score > s.score) {
                    dp[j + len] = Some(State {
                        score,
                        prev: j,
                        word,
                        raw: None,
                    });
                }
            }
        }

        // Furthest reachable position; the rest is the partial tail.
        let full_end = (0..=n).rev().find(|&i| dp[i].is_some()).unwrap_or(0);
        let mut parts: Vec<&str> = Vec::new();
        let mut path: Vec<(String, usize, usize)> = Vec::new();
        let mut has_raw = false;
        let mut i = full_end;
        while i > 0 {
            let s = dp[i].as_ref().unwrap();
            match s.raw {
                Some((a, b)) => {
                    parts.push(&lower[a..b]);
                    has_raw = true;
                }
                None => {
                    parts.push(s.word);
                    if !s.word.is_empty() {
                        path.push((s.word.to_string(), s.prev, i));
                    }
                }
            }
            i = s.prev;
        }
        parts.reverse();
        path.reverse();
        let mut best_line: String = parts.concat();
        best_line.push_str(lower[full_end..].trim_start_matches('\''));

        // Candidates, in order: user picks that cover the whole convertible
        // prefix, the whole-line conversion, shorter user picks, then
        // dictionary words starting at byte 0 (longest span first). A user
        // pick that covers less than the whole line must NOT sit above it —
        // otherwise space commits a partial word and re-recording it
        // entrenches the wrong default. The UI pages these 5 at a time.
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut push = |cands: &mut Vec<Candidate>, text: &str, consumed: usize, sentence: bool| {
            if !cands
                .iter()
                .any(|c| c.text == text && c.consumed_bytes == consumed)
            {
                cands.push(Candidate {
                    text: text.to_string(),
                    consumed_bytes: consumed,
                    sentence,
                });
            }
        };
        let remembered = user.prefix_matches(&lower);
        for (consumed, w, _) in remembered.iter().filter(|(c, _, _)| *c >= full_end) {
            push(&mut candidates, w, *consumed, false);
        }
        // Skip the whole-line candidate when it contains raw letters — a
        // half-converted string is not something to commit or memorize.
        if full_end > 0 && !has_raw {
            let text = parts.concat();
            push(&mut candidates, &text, full_end, true);
        }
        for (consumed, w, _) in remembered.iter().filter(|(c, _, _)| *c < full_end) {
            push(&mut candidates, w, *consumed, false);
        }
        let mut starts: Vec<(usize, u128, f64)> = Vec::new();
        self.walk_words(bytes, 0, |end, key| {
            starts.push((end, key, self.words[&key][0].1));
        });
        starts.sort_by(|a, b| b.0.cmp(&a.0).then(b.2.total_cmp(&a.2)));
        'outer: for (end, key, _) in starts {
            // Same scan-and-boost rule as the Viterbi, so what the row shows
            // first is also what Enter's whole-line conversion would use.
            let mut ws: Vec<(&str, f64)> = self.words[&key]
                .iter()
                .map(|&(w, g)| (w, g + user_boost(user, w)))
                .collect();
            ws.sort_by(|a, b| b.1.total_cmp(&a.1));
            let take = if end == full_end { 10 } else { 8 };
            for (w, _) in ws.into_iter().take(take) {
                if candidates.iter().any(|c| c.text.as_str() == w) {
                    continue;
                }
                candidates.push(Candidate {
                    text: w.to_string(),
                    consumed_bytes: end,
                    sentence: false,
                });
                if candidates.len() >= MAX_CANDIDATES {
                    break 'outer;
                }
            }
        }

        // 简拼 (abbreviated pinyin), like Microsoft Pinyin: the run's letters
        // taken as per-syllable initials — "hl" -> 回来, "dw" -> 等我. When
        // the run doesn't parse as full pinyin these are the primary
        // candidates and drive the preview; otherwise they trail the list.
        if !lower.contains('\'') {
            let mut jianpin: Vec<Candidate> = Vec::new();
            if let Some(ws) = pack_initials(lower.as_bytes()).and_then(|jp| self.abbrev.get(&jp)) {
                let mut ws: Vec<(&str, f64)> = ws
                    .iter()
                    .map(|&(w, g)| (w, g + user_boost(user, w)))
                    .collect();
                ws.sort_by(|a, b| b.1.total_cmp(&a.1));
                for (w, _) in ws.into_iter().take(8) {
                    if !candidates.iter().any(|c| c.text.as_str() == w) {
                        jianpin.push(Candidate {
                            text: w.to_string(),
                            consumed_bytes: lower.len(),
                            sentence: false,
                        });
                    }
                }
            }
            if !jianpin.is_empty() {
                if full_end == 0 {
                    candidates.extend(jianpin);
                    if let Some(c) = candidates.iter().find(|c| c.consumed_bytes == lower.len()) {
                        best_line = c.text.clone();
                    }
                } else {
                    let keep = MAX_CANDIDATES - jianpin.len().min(8);
                    candidates.truncate(keep);
                    candidates.extend(jianpin.into_iter().take(8));
                }
            }
        }
        Analysis {
            candidates,
            best_line,
            path,
        }
    }

    #[cfg(test)]
    fn syllable_count(&self) -> usize {
        self.syl_text.len()
    }
}

/// Byte offset where the trailing pinyin run starts, or `text.len()` if none.
/// A run is a trailing sequence of ASCII letters and apostrophes.
pub fn run_start(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut start = bytes.len();
    while start > 0 {
        let c = bytes[start - 1];
        if c.is_ascii_alphabetic() || c == b'\'' {
            start -= 1;
        } else {
            break;
        }
    }
    // A run must begin with a letter, not an apostrophe.
    while start < bytes.len() && bytes[start] == b'\'' {
        start += 1;
    }
    start
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_common_pinyin() {
        let dict = PinyinDict::load();
        let user = UserDict::default();
        assert!(dict.syllable_count() > 300);

        let a = dict.analyze("nihao", &user);
        assert_eq!(a.candidates[0].text, "你好");
        assert_eq!(a.candidates[0].consumed_bytes, 5);

        let a = dict.analyze("jintianchileshenme", &user);
        assert!(a.best_line.contains("今天"), "got {}", a.best_line);
        assert!(a.best_line.contains("什么"), "got {}", a.best_line);

        // A trailing single letter now converts as jianpin (sentence
        // abbreviation support) instead of dangling as a raw letter.
        let a = dict.analyze("nihaoq", &user);
        assert!(a.best_line.starts_with("你好"), "got {}", a.best_line);
        assert!(
            !a.best_line.contains(|c: char| c.is_ascii_alphabetic()),
            "got {}",
            a.best_line
        );

        // Apostrophe forces a split: xi'an vs xian.
        let a = dict.analyze("xi'an", &user);
        assert_eq!(a.candidates[0].text, "西安");

        // Non-pinyin input passes through unharmed (UTF-8 safety guard).
        let a = dict.analyze("nǐhǎo", &user);
        assert_eq!(a.best_line, "nǐhǎo");
        assert!(a.candidates.is_empty());
    }

    #[test]
    fn segmentation_ties_use_word_scores() {
        // These all have an equal-syllable-count wrong split that a greedy
        // segmenter picks; the joint Viterbi must choose the common word.
        let dict = PinyinDict::load();
        let user = UserDict::default();
        for (input, expect) in [
            ("keneng", "可能"),
            ("sange", "三个"),
            ("jineng", "技能"),
            ("dangao", "蛋糕"),
        ] {
            let a = dict.analyze(input, &user);
            assert_eq!(a.best_line, expect, "input {input}");
        }
    }

    #[test]
    fn sentence_jianpin_and_mixed() {
        let dict = PinyinDict::load();
        let user = UserDict::default();
        // Whole-sentence jianpin: every letter is an initial.
        let a = dict.analyze("wmdl", &user);
        assert_eq!(a.best_line, "我们到了", "wmdl -> {}", a.best_line);
        // The conversion path exposes (word, pinyin-span) pairs for learning.
        let path: Vec<(&str, usize, usize)> =
            a.path.iter().map(|(w, s, e)| (w.as_str(), *s, *e)).collect();
        assert_eq!(path, vec![("我们", 0, 2), ("到了", 2, 4)]);
        // Jianpin mixed with full pinyin.
        let a = dict.analyze("wbuxiangquchifan", &user);
        assert_eq!(a.best_line, "我不想去吃饭", "got {}", a.best_line);
        // Full-pinyin parses still beat jianpin interpretations.
        let a = dict.analyze("nihao", &user);
        assert_eq!(a.best_line, "你好");
        let a = dict.analyze("jintianchileshenme", &user);
        assert!(a.best_line.contains("今天"), "got {}", a.best_line);
        // Uppercase input is literal, never jianpin-converted.
        let a = dict.analyze("GPT", &user);
        assert!(!a.best_line.contains(|c: char| c > '\u{7f}'), "got {}", a.best_line);
    }

    #[test]
    fn jianpin_abbreviations() {
        let dict = PinyinDict::load();
        let user = UserDict::default();
        for (jp, expect) in [("hl", "回来"), ("dw", "等我"), ("bj", "北京"), ("zmb", "怎么办")] {
            let a = dict.analyze(jp, &user);
            assert!(
                a.candidates.iter().any(|c| c.text == expect),
                "{jp}: {expect} not in {:?}",
                a.candidates.iter().take(8).map(|c| &c.text).collect::<Vec<_>>()
            );
            // Pure jianpin drives the whole-line preview to a real word.
            assert!(!a.best_line.contains(|c: char| c.is_ascii_alphabetic()), "{jp} -> {}", a.best_line);
        }
        // Memory personalizes jianpin exactly like full pinyin.
        let mut user = UserDict::default();
        user.record_pick("hl", "回来", None);
        let a = dict.analyze("hl", &user);
        assert_eq!(a.candidates[0].text, "回来");
        assert_eq!(a.best_line, "回来");
        // Full-pinyin runs keep their normal candidates first.
        let a = dict.analyze("nihao", &UserDict::default());
        assert_eq!(a.candidates[0].text, "你好");
    }

    #[test]
    fn user_memory_ranks_first() {
        let dict = PinyinDict::load();
        let mut user = UserDict::default();
        user.record_pick("shizi", "狮子", None);
        user.record_pick("shizi", "狮子", None);
        // Remembered pick outranks whatever the dictionary prefers.
        let a = dict.analyze("shizi", &user);
        assert_eq!(a.candidates[0].text, "狮子");
        // While more pinyin follows, the whole-line conversion stays first
        // (a partial pick above it would make space commit half a phrase),
        // with the remembered word right behind it — and the Viterbi itself
        // picks the remembered word inside the line.
        let a = dict.analyze("shizihen", &user);
        assert!(a.candidates[0].text.starts_with("狮子"), "got {:?}", a.candidates[0]);
        assert!(
            a.candidates[1..3]
                .iter()
                .any(|c| c.text == "狮子" && c.consumed_bytes == 5),
            "got {:?}",
            &a.candidates[..3]
        );
        // Heavy use also biases whole-line conversion.
        let mut heavy = UserDict::default();
        for _ in 0..200 {
            heavy.record_pick("shi", "试", None);
        }
        let a = dict.analyze("shi", &heavy);
        assert_eq!(a.candidates[0].text, "试");
    }
}

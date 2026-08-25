//! Output register polish: nudge opus-mt's textbook English toward North
//! American business-casual. Deliberately conservative — only word-boundary
//! contractions, a few stock de-stiffeners, and whitespace cleanup. Never
//! touches meaning. `style=0` in the config turns it off.

/// Multi-word phrases first (longest match wins), then contractions.
const PHRASES: &[(&str, &str)] = &[
    ("As soon as possible", "ASAP"),
    ("as soon as possible", "ASAP"),
    ("In order to", "To"),
    ("in order to", "to"),
    ("a great deal of", "a lot of"),
    ("It is a pity that", "Too bad"),
    ("I have been", "I've been"),
    ("We have been", "We've been"),
    ("You have been", "You've been"),
    ("They have been", "They've been"),
    ("we have been", "we've been"),
    ("you have been", "you've been"),
    ("they have been", "they've been"),
];

const CONTRACTIONS: &[(&str, &str)] = &[
    ("I am", "I'm"),
    ("You are", "You're"),
    ("you are", "you're"),
    ("We are", "We're"),
    ("we are", "we're"),
    ("They are", "They're"),
    ("they are", "they're"),
    ("He is", "He's"),
    ("he is", "he's"),
    ("She is", "She's"),
    ("she is", "she's"),
    ("It is", "It's"),
    ("it is", "it's"),
    ("That is", "That's"),
    ("that is", "that's"),
    ("There is", "There's"),
    ("there is", "there's"),
    ("What is", "What's"),
    ("what is", "what's"),
    ("I will", "I'll"),
    ("We will", "We'll"),
    ("we will", "we'll"),
    ("You will", "You'll"),
    ("you will", "you'll"),
    ("They will", "They'll"),
    ("they will", "they'll"),
    ("He will", "He'll"),
    ("he will", "he'll"),
    ("She will", "She'll"),
    ("she will", "she'll"),
    ("It will", "It'll"),
    ("it will", "it'll"),
    ("I would", "I'd"),
    ("We would", "We'd"),
    ("we would", "we'd"),
    ("Do not", "Don't"),
    ("do not", "don't"),
    ("Does not", "Doesn't"),
    ("does not", "doesn't"),
    ("Did not", "Didn't"),
    ("did not", "didn't"),
    ("Cannot", "Can't"),
    ("cannot", "can't"),
    ("Can not", "Can't"),
    ("can not", "can't"),
    ("Will not", "Won't"),
    ("will not", "won't"),
    ("Is not", "Isn't"),
    ("is not", "isn't"),
    ("Are not", "Aren't"),
    ("are not", "aren't"),
    ("Was not", "Wasn't"),
    ("was not", "wasn't"),
    ("Were not", "Weren't"),
    ("were not", "weren't"),
    ("Would not", "Wouldn't"),
    ("would not", "wouldn't"),
    ("Should not", "Shouldn't"),
    ("should not", "shouldn't"),
    ("Could not", "Couldn't"),
    ("could not", "couldn't"),
    ("Have not", "Haven't"),
    ("have not", "haven't"),
    ("Has not", "Hasn't"),
    ("has not", "hasn't"),
];

/// Replaces `from` with `to` only at word boundaries (no mid-word hits, and
/// never right before an apostrophe — that would double-contract).
fn replace_word(s: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(from) {
        let prev = rest[..i].chars().next_back().or_else(|| out.chars().next_back());
        let before_ok = prev.map_or(true, |c| !c.is_alphanumeric());
        let after = &rest[i + from.len()..];
        let after_ok = after
            .chars()
            .next()
            .map_or(true, |c| !c.is_alphanumeric() && c != '\'');
        out.push_str(&rest[..i]);
        if before_ok && after_ok {
            out.push_str(to);
        } else {
            out.push_str(from);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

pub fn polish(en: &str) -> String {
    let mut s = en.to_string();
    for (from, to) in PHRASES.iter().chain(CONTRACTIONS.iter()) {
        if s.contains(from) {
            s = replace_word(&s, from, to);
        }
    }
    // Whitespace/punctuation cleanup.
    while s.contains("  ") {
        s = s.replace("  ", " ");
    }
    for p in [" .", " ,", " ?", " !"] {
        if s.contains(p) {
            s = s.replace(p, p.trim_start());
        }
    }
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracts_and_destiffens() {
        assert_eq!(polish("I am on it. We will do it."), "I'm on it. We'll do it.");
        assert_eq!(
            polish("We cannot make it, it is too late."),
            "We can't make it, it's too late."
        );
        assert_eq!(
            polish("Please reply as soon as possible."),
            "Please reply ASAP."
        );
        assert_eq!(
            polish("In order to finish, I will need help."),
            "To finish, I'll need help."
        );
        // Word boundaries: no mid-word or double-contraction damage.
        assert_eq!(polish("Miami is not Miami's"), "Miami isn't Miami's");
        assert_eq!(polish("It's fine"), "It's fine");
        assert_eq!(polish("This  has  extra  spaces ."), "This has extra spaces.");
    }
}

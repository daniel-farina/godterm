//! Text helpers for speech: normalizing a transcript and finding the wake
//! word. There is no command grammar: the instant one word commands are in
//! `crate::instant`, and the assistant interprets everything else.

/// Lowercase, strip punctuation, collapse whitespace.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_alphanumeric()
            || c == '\''
            || c == '/'
            || c == '~'
            || c == '.'
            || c == '-'
            || c == '_'
        {
            out.extend(c.to_lowercase());
        } else {
            out.push(' ');
        }
    }
    // Trailing sentence periods are punctuation, inner dots belong to paths.
    out.split_whitespace()
        .map(|w| w.trim_matches(|c| c == '.' || c == '-' || c == '\''))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Strip a leading wake word. Returns the rest when the transcript starts
/// with (something close to) one of `wake_words`.
pub fn strip_wake<'a>(norm: &'a str, wake_words: &[String]) -> Option<&'a str> {
    let words: Vec<&str> = norm.split(' ').collect();
    // "hey go" (the wake word before the GodTerm rename) still works.
    let mut list = wake_words.to_vec();
    if list.iter().any(|w| normalize(w) == "hey god")
        && !list.iter().any(|w| normalize(w) == "hey go")
    {
        list.push("hey go".into());
    }
    for ww in &list {
        let ww = normalize(ww);
        let wk: Vec<&str> = ww.split(' ').collect();
        if wk.is_empty() || words.len() < wk.len() {
            continue;
        }
        // Run together or split apart: "heygo", "ago", "compute her".
        let squashed: String = ww.chars().filter(|c| *c != ' ').collect();
        for n in (1..=3usize.min(words.len())).rev() {
            for start in 0..=1usize.min(words.len() - n) {
                let joined: String = words[start..start + n].concat();
                let close = edit_distance(&joined, &squashed) <= (squashed.len() / 4).max(1);
                if close || compact_alias(&joined, &squashed) {
                    let skip: usize = words[..start + n].iter().map(|w| w.len() + 1).sum();
                    return Some(norm.get(skip.min(norm.len())..).unwrap_or("").trim());
                }
            }
        }
        // Allow the wake phrase to start at word 0 or 1 ("okay hey go").
        for start in 0..=1usize.min(words.len() - wk.len()) {
            let window = &words[start..start + wk.len()];
            let joined = window.join(" ");
            let ok = window.iter().zip(&wk).all(|(a, b)| wake_like(a, b))
                || edit_distance(&joined, &ww) <= ww.len() / 4;
            if ok {
                let skip: usize = words[..start + wk.len()].iter().map(|w| w.len() + 1).sum();
                return Some(norm.get(skip.min(norm.len())..).unwrap_or("").trim());
            }
        }
    }
    None
}

/// Whole word mishearings of the default wake phrases.
fn compact_alias(heard: &str, squashed_wake: &str) -> bool {
    match squashed_wake {
        "heygo" => matches!(
            heard,
            "ago" | "hago" | "hego" | "kego" | "heygo" | "heygoal" | "hagoo" | "aygo"
        ),
        "computer" => matches!(heard, "commuter" | "computa" | "compooter" | "puter"),
        "heygod" => matches!(
            heard,
            "heygod" | "hagod" | "heygot" | "agod" | "egod" | "hegod" | "heygods" | "haygod"
        ),
        "godterm" => matches!(
            heard,
            "godterm" | "gotterm" | "goodterm" | "godturn" | "godtern" | "gotturn" | "godthem"
        ),
        _ => false,
    }
}

fn wake_like(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // Short wake words ("hey", "go") are commonly heard as these.
    fn alias(w: &str) -> &str {
        match w {
            "hey" | "hay" | "hi" | "a" | "eh" | "hei" | "hey," | "they" | "pay" | "say"
            | "okay" => "hey",
            "go" | "goal" | "goes" | "gold" | "joe" | "ghost" | "girl" | "gopher" | "gogh"
            | "goh" | "though" | "co" | "know" | "no" => "go",
            other => other,
        }
    }
    alias(a) == alias(b) || (b.len() >= 5 && edit_distance(a, b) <= 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("  Hey, GO!  Approve. "), "hey go approve");
        assert_eq!(normalize("new tab in ~/code/api."), "new tab in ~/code/api");
    }

    #[test]
    fn wake_words() {
        let ww = vec!["hey go".to_string(), "computer".to_string()];
        assert_eq!(strip_wake("hey go approve", &ww), Some("approve"));
        assert_eq!(strip_wake("hey goal next tab", &ww), Some("next tab"));
        assert_eq!(strip_wake("a go deny", &ww), Some("deny"));
        assert_eq!(strip_wake("hey joe show usage", &ww), Some("show usage"));
        assert_eq!(strip_wake("computer tab three", &ww), Some("tab three"));
        assert_eq!(strip_wake("computers status", &ww), Some("status"));
        assert_eq!(strip_wake("okay hey go yes", &ww), Some("yes"));
        assert_eq!(strip_wake("hey go", &ww), Some(""));
        let new = vec!["hey god".to_string(), "god term".to_string()];
        assert_eq!(strip_wake("hey god approve", &new), Some("approve"));
        assert_eq!(strip_wake("god term next tab", &new), Some("next tab"));
        assert_eq!(strip_wake("godterm show usage", &new), Some("show usage"));
        assert_eq!(
            strip_wake("hey go approve", &new),
            Some("approve"),
            "the old wake word still works"
        );
        assert_eq!(strip_wake("ago approve", &ww), Some("approve"));
        assert_eq!(strip_wake("heygo next tab", &ww), Some("next tab"));
        assert_eq!(strip_wake("hey gogh deny", &ww), Some("deny"));
        assert_eq!(strip_wake("say go status", &ww), Some("status"));
        assert_eq!(strip_wake("compute her overview", &ww), Some("overview"));
        assert_eq!(strip_wake("commuter tab two", &ww), Some("tab two"));
        assert_eq!(strip_wake("i went to the store", &ww), None);
        assert_eq!(strip_wake("let's go shopping", &ww), None);
        assert_eq!(strip_wake("we know the answer", &ww), None);
        assert_eq!(strip_wake("go home", &ww), None);
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("same", "same"), 0);
    }

    #[test]
    fn junk_never_panics() {
        for s in [
            "",
            " ",
            "...",
            "日本 👍",
            "e\u{301} ~/x",
            "hey hey hey",
            "go",
        ] {
            let _ = strip_wake(&normalize(s), &["hey go".into()]);
        }
    }
}

//! Names said aloud: speech recognition splits, merges and mishears them
//! ("bot mesh", "pod mesh", "bought mesh" for botmesh) and spells out
//! punctuation ("lol forward slash botmesh"). The tools match what they
//! are given against the names they know with an edit distance and a
//! rough sound key, so a near miss still finds the right thing.

/// Spoken punctuation to symbols, case kept: "lol forward slash botmesh"
/// -> "lol/botmesh", "index dot html" -> "index.html".
pub fn spoken_punctuation(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut out = String::new();
    let mut glue = false;
    let mut i = 0;
    while i < words.len() {
        let w = words[i].to_lowercase();
        let next = words.get(i + 1).map(|n| n.to_lowercase());
        let sym = match (w.as_str(), next.as_deref()) {
            ("forward", Some("slash")) | ("back", Some("slash")) => {
                i += 1;
                Some(if w == "forward" { "/" } else { "\\" })
            }
            ("slash", _) => Some("/"),
            ("backslash", _) => Some("\\"),
            ("dot", _) => Some("."),
            ("underscore", _) => Some("_"),
            ("dash" | "hyphen", _) => Some("-"),
            ("tilde", _) => Some("~"),
            _ => None,
        };
        match sym {
            Some(sym) => {
                out.push_str(sym);
                glue = true;
            }
            None => {
                if !out.is_empty() && !glue {
                    out.push(' ');
                }
                out.push_str(words[i]);
                glue = false;
            }
        }
        i += 1;
    }
    out
}

/// Compare key: spoken punctuation resolved, lower case, without spaces,
/// hyphens, underscores and dots ("Bot-Mesh" and "bot mesh" are equal).
pub fn key(s: &str) -> String {
    spoken_punctuation(s)
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_' | '.' | '\t'))
        .collect()
}

/// A rough sound key: consonants by how they sound (b/p, d/t, g/k/c/q,
/// f/v/ph, s/z/sh/ch), silent "gh", vowels dropped after the first letter,
/// repeats folded. "botmesh", "podmesh", "bought mesh", "botmash" share one.
pub fn sound(s: &str) -> String {
    let w: Vec<char> = key(s)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '/')
        .collect();
    let mut out = String::new();
    let mut i = 0;
    while i < w.len() {
        let c = w[i];
        let next = w.get(i + 1).copied();
        let code = match c {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'w' | 'h' => {
                if out.is_empty() || out.ends_with('/') {
                    Some('A')
                } else {
                    None
                }
            }
            'g' if next == Some('h') => {
                i += 1;
                None
            }
            'p' if next == Some('h') => {
                i += 1;
                Some('F')
            }
            's' | 'c' if next == Some('h') => {
                i += 1;
                Some('S')
            }
            'b' | 'p' => Some('B'),
            'd' | 't' => Some('T'),
            'g' | 'k' | 'c' | 'q' => Some('K'),
            'f' | 'v' => Some('F'),
            's' | 'z' | 'x' => Some('S'),
            'j' => Some('K'),
            'm' | 'n' => Some('M'),
            'l' | 'r' => Some('L'),
            '/' => Some('/'),
            d if d.is_ascii_digit() => Some(d),
            _ => None,
        };
        if let Some(code) = code {
            if !out.ends_with(code) {
                out.push(code);
            }
        }
        i += 1;
    }
    out
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j + 1] + 1)
                .min(cur[j] + 1)
                .min(prev[j] + usize::from(ca != cb));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// How alike two names are, 0 to 1: spelling (edit distance on the keys)
/// and sound (the same sound key).
pub fn similarity(a: &str, b: &str) -> f64 {
    let (ka, kb) = (key(a), key(b));
    if ka.is_empty() || kb.is_empty() {
        return 0.0;
    }
    if ka == kb {
        return 1.0;
    }
    let d = levenshtein(&ka, &kb) as f64;
    let spell = 1.0 - d / ka.chars().count().max(kb.chars().count()) as f64;
    let (sa, sb) = (sound(a), sound(b));
    let snd = if sa == sb {
        1.0
    } else {
        1.0 - levenshtein(&sa, &sb) as f64 / sa.len().max(sb.len()).max(1) as f64
    };
    (0.45 * spell + 0.55 * snd).clamp(0.0, 0.99)
}

/// A candidate and how well it matches.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Candidate {
    pub value: String,
    pub score: f64,
}

/// The outcome of matching a spoken name.
#[derive(Debug, Clone, PartialEq)]
pub enum Match {
    /// One candidate is clearly the one (exact, or well ahead of the rest).
    Sure(String),
    /// Several are about as likely: ranked.
    Unsure(Vec<Candidate>),
    None,
}

/// Rank `candidates` against `query`; `label` gives the text compared
/// for each (e.g. the last path components of a folder).
pub fn best<'a, I>(query: &str, candidates: I) -> Match
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let mut v: Vec<Candidate> = candidates
        .into_iter()
        .map(|(value, label)| Candidate {
            value: value.to_string(),
            score: similarity(query, label),
        })
        .filter(|c| c.score >= 0.5)
        .collect();
    v.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    v.dedup_by(|a, b| a.value == b.value);
    match v.as_slice() {
        [] => Match::None,
        [one] if one.score >= 0.6 => Match::Sure(one.value.clone()),
        // An exact match (same key) wins over near ones.
        [first, second, ..] if first.score >= 1.0 && second.score < 1.0 => {
            Match::Sure(first.value.clone())
        }
        [first, second, ..] if first.score >= 0.7 && first.score - second.score >= 0.12 => {
            Match::Sure(first.value.clone())
        }
        _ => {
            v.truncate(5);
            for c in v.iter_mut() {
                c.score = (c.score * 100.0).round() / 100.0;
            }
            Match::Unsure(v)
        }
    }
}

/// The last `n` components of a path ("/Users/x/lol/botmesh", 2 -> "lol/botmesh").
pub fn tail(path: &str, n: usize) -> String {
    let parts: Vec<&str> = path
        .trim_end_matches('/')
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    parts[parts.len().saturating_sub(n)..].join("/")
}

/// Match a spoken folder ("lol slash pod mesh") against known folders,
/// comparing with as many trailing components as the query has.
pub fn best_folder<'a>(query: &str, folders: impl IntoIterator<Item = &'a str>) -> Match {
    let q = spoken_punctuation(query);
    let n = q.trim_matches('/').split('/').count().max(1);
    let labels: Vec<(String, String)> = folders
        .into_iter()
        .map(|f| (f.to_string(), tail(f, n)))
        .collect();
    best(&q, labels.iter().map(|(v, l)| (v.as_str(), l.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOLDERS: &[&str] = &[
        "/Users/me/lol/botmesh",
        "/Users/me/lol/trading-room",
        "/Users/me/hacker-sim",
        "/Users/me/meme",
        "/Users/me/claudego",
    ];

    #[test]
    fn misheard_folders_find_botmesh() {
        for q in [
            "pod mesh",
            "bot mesh",
            "lol slash pod mesh",
            "botmash",
            "bought mesh",
            "lol/bot mesh",
            "lol forward slash podmesh",
            "lol/podmesh",
            "Bot-Mesh",
        ] {
            assert_eq!(
                best_folder(q, FOLDERS.iter().copied()),
                Match::Sure("/Users/me/lol/botmesh".into()),
                "{q}"
            );
        }
        assert_eq!(
            best_folder("trading room", FOLDERS.iter().copied()),
            Match::Sure("/Users/me/lol/trading-room".into())
        );
        assert_eq!(
            best_folder("something else entirely", FOLDERS.iter().copied()),
            Match::None
        );
    }

    #[test]
    fn alike_names_are_offered_not_guessed() {
        let f = ["/a/botmesh", "/a/potmesh", "/a/botmash"];
        match best_folder("bot mesh", f.iter().copied()) {
            Match::Sure(v) => assert_eq!(v, "/a/botmesh", "exact key wins"),
            m => panic!("{m:?}"),
        }
        match best_folder("pod mess", f.iter().copied()) {
            Match::Unsure(c) => assert!(c.len() >= 2, "{c:?}"),
            m => panic!("expected candidates, got {m:?}"),
        }
    }

    #[test]
    fn keys_and_sounds() {
        assert_eq!(
            spoken_punctuation("lol forward slash botmesh"),
            "lol/botmesh"
        );
        assert_eq!(spoken_punctuation("index dot html"), "index.html");
        assert_eq!(
            spoken_punctuation("/Users/Alex/My Project"),
            "/Users/Alex/My Project",
            "case and spaces kept"
        );
        assert_eq!(spoken_punctuation("lol slash pod mesh"), "lol/pod mesh");
        assert_eq!(key("Bot Mesh"), "botmesh");
        assert_eq!(sound("botmesh"), sound("bought mesh"));
        assert_eq!(sound("botmesh"), sound("pod mesh"));
        assert_eq!(sound("botmesh"), sound("botmash"));
    }
}

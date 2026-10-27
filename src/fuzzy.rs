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

pub fn key(s: &str) -> String {
    spoken_punctuation(s)
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_' | '.' | '\t'))
        .collect()
}

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

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Candidate {
    pub value: String,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Match {
    Sure(String),

    Unsure(Vec<Candidate>),
    None,
}

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

pub fn tail(path: &str, n: usize) -> String {
    let parts: Vec<&str> = path
        .trim_end_matches('/')
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    parts[parts.len().saturating_sub(n)..].join("/")
}

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
        "/Users/me/code/fotmesh",
        "/Users/me/code/trading-room",
        "/Users/me/hacker-sim",
        "/Users/me/meme",
        "/Users/me/claudego",
    ];

    #[test]
    fn misheard_folders_find_fotmesh() {
        for q in [
            "vod mesh",
            "fot mesh",
            "code slash vod mesh",
            "fotmash",
            "fought mesh",
            "code/fot mesh",
            "code forward slash vodmesh",
            "code/vodmesh",
            "Fot-Mesh",
        ] {
            assert_eq!(
                best_folder(q, FOLDERS.iter().copied()),
                Match::Sure("/Users/me/code/fotmesh".into()),
                "{q}"
            );
        }
        assert_eq!(
            best_folder("trading room", FOLDERS.iter().copied()),
            Match::Sure("/Users/me/code/trading-room".into())
        );
        assert_eq!(
            best_folder("something else entirely", FOLDERS.iter().copied()),
            Match::None
        );
    }

    #[test]
    fn alike_names_are_offered_not_guessed() {
        let f = ["/a/fotmesh", "/a/votmesh", "/a/fotmash"];
        match best_folder("fot mesh", f.iter().copied()) {
            Match::Sure(v) => assert_eq!(v, "/a/fotmesh", "exact key wins"),
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
            spoken_punctuation("code forward slash fotmesh"),
            "code/fotmesh"
        );
        assert_eq!(spoken_punctuation("index dot html"), "index.html");
        assert_eq!(
            spoken_punctuation("/Users/Alex/My Project"),
            "/Users/Alex/My Project",
            "case and spaces kept"
        );
        assert_eq!(spoken_punctuation("code slash vod mesh"), "code/vod mesh");
        assert_eq!(key("Fot Mesh"), "fotmesh");
        assert_eq!(sound("fotmesh"), sound("fought mesh"));
        assert_eq!(sound("fotmesh"), sound("vod mesh"));
        assert_eq!(sound("fotmesh"), sound("fotmash"));
    }
}

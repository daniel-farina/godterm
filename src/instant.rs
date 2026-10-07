//! Instant one word commands: a tiny, fixed set of controls that run at
//! once, without the assistant, when they are the whole utterance. Nothing
//! else is parsed locally; the assistant interprets everything else.

/// The default set (Settings > Voice > instant set).
pub const DEFAULTS: &[&str] = &[
    "stop",
    "yes",
    "no",
    "approve",
    "deny",
    "sleep",
    "wake up",
    "stop talking",
    "next tab",
    "previous tab",
    "mute",
    "hold on",
    "hang on",
    "pause",
    "resume",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instant {
    /// Stop talking back (and the rest of the current reply).
    Stop,
    /// Approve the waiting tab (or answer yes to the open dialog).
    Approve,
    /// Deny the waiting tab (or no to the open dialog).
    Deny,
    Sleep,
    Wake,
    NextTab,
    PrevTab,
    /// Turn the mic off (only a click or Ctrl-a X turns it back on).
    Mute,
    /// Stop acting on speech for the default pause.
    Pause,
    /// End a pause early.
    Resume,
}

/// An action by name, for "phrase=action" entries.
pub fn action(name: &str) -> Option<Instant> {
    Some(
        match name.trim().to_lowercase().replace([' ', '-'], "_").as_str() {
            "stop" | "stop_talking" | "quiet" => Instant::Stop,
            "approve" | "yes" => Instant::Approve,
            "deny" | "no" => Instant::Deny,
            "sleep" => Instant::Sleep,
            "wake" | "wake_up" => Instant::Wake,
            "next_tab" => Instant::NextTab,
            "previous_tab" | "prev_tab" => Instant::PrevTab,
            "mute" | "mute_mic" => Instant::Mute,
            "pause" | "hold_on" | "hang_on" | "pause_listening" => Instant::Pause,
            "resume" | "resume_listening" => Instant::Resume,
            _ => return None,
        },
    )
}

/// The configured phrases with their actions. A bare phrase is a built in
/// one ("next tab"); "phrase=action" adds a synonym ("yep=approve").
pub fn table(list: &[String]) -> Vec<(String, Instant)> {
    list.iter()
        .filter_map(|e| {
            let (phrase, act) = match e.split_once('=') {
                Some((p, a)) => (p, action(a)?),
                None => (e.as_str(), action(e)?),
            };
            let p = clean(phrase);
            (!p.is_empty()).then_some((p, act))
        })
        .collect()
}

/// Lower case words only: "Next tab!" -> "next tab".
fn clean(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The instant command this whole utterance is, if any. "please" and
/// "okay" around it are allowed; anything else makes it a request for the
/// assistant.
pub fn match_instant(utterance: &str, table: &[(String, Instant)]) -> Option<Instant> {
    let mut words: Vec<String> = clean(utterance)
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    while words
        .first()
        .is_some_and(|w| matches!(w.as_str(), "please" | "okay" | "ok"))
    {
        words.remove(0);
    }
    while words
        .last()
        .is_some_and(|w| matches!(w.as_str(), "please" | "now"))
    {
        words.pop();
    }
    let u = words.join(" ");
    table.iter().find(|(p, _)| *p == u).map(|(_, a)| *a)
}

/// True when the reply is a clear no ("no", "don't", "cancel", "leave
/// them"), not a question.
pub fn negative(reply: &str) -> bool {
    let raw = reply.trim();
    if raw.is_empty() || raw.ends_with('?') {
        return false;
    }
    let c = format!(" {} ", clean(raw));
    [
        " no ",
        " nope ",
        " nah ",
        " don't ",
        " dont ",
        " do not ",
        " cancel ",
        " never mind ",
        " nevermind ",
        " leave them ",
        " leave it ",
        " not now ",
        " forget it ",
    ]
    .iter()
    .any(|n| c.contains(n))
}

/// A question without its question mark, as speech recognition writes it:
/// "do you close them", "did you do it", "is it done", "can you send it".
/// "do it" stays a request.
fn spoken_question(reply: &str) -> bool {
    let c = clean(reply);
    let mut w = c.split(' ');
    let (Some(first), second) = (w.next(), w.next()) else {
        return false;
    };
    const SUBJECTS: &[&str] = &[
        "you", "i", "we", "they", "it", "that", "this", "these", "those", "he", "she", "there",
        "all", "the",
    ];
    let subject = |s: Option<&str>| s.is_some_and(|s| SUBJECTS.contains(&s));
    match first {
        "did" | "does" | "is" | "are" | "was" | "were" | "can" | "could" | "would" | "should"
        | "will" | "shall" | "have" | "has" | "am" | "isn't" | "aren't" | "didn't" => {
            subject(second)
        }
        // "do you close them" asks; "do it" tells.
        "do" => second.is_some_and(|s| ["you", "i", "we", "they", "these", "those"].contains(&s)),
        _ => false,
    }
}

/// True when the reply is a clear yes: an affirmative word or phrase, no
/// question, no negation. Destructive confirmations need this.
pub fn affirmative(reply: &str) -> bool {
    let raw = reply.trim();
    if raw.is_empty() || raw.ends_with('?') || spoken_question(raw) {
        return false;
    }
    let c = format!(" {} ", clean(raw));
    let neg = [
        " no ",
        " not ",
        " don't ",
        " dont ",
        " do not ",
        " wait ",
        " stop ",
        " cancel ",
        " hold on ",
        " never ",
        " nope ",
        " nah ",
        " keep ",
        " why ",
        " what ",
        " which ",
        " how ",
        " maybe ",
        " later ",
    ];
    if neg.iter().any(|n| c.contains(n)) {
        return false;
    }
    let yes = [
        " yes ",
        " yeah ",
        " yep ",
        " yup ",
        " ya ",
        " sure ",
        " do it ",
        " confirm ",
        " confirmed ",
        " go ahead ",
        " go for it ",
        " proceed ",
        " please do ",
        " affirmative ",
        " correct ",
        " ok ",
        " okay ",
        " absolutely ",
        " definitely ",
        " of course ",
        " that's right ",
        " right ",
        " close them ",
        " close it ",
        " close all ",
        " stop them ",
        " send it ",
        " approve ",
        " deny it ",
        " all of them ",
    ];
    yes.iter().any(|y| c.contains(y))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Vec<(String, Instant)> {
        table(&DEFAULTS.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn whole_utterance_only() {
        let t = t();
        for (said, want) in [
            ("Stop.", Some(Instant::Stop)),
            ("yes", Some(Instant::Approve)),
            ("Yes please", Some(Instant::Approve)),
            ("no", Some(Instant::Deny)),
            ("approve", Some(Instant::Approve)),
            ("deny", Some(Instant::Deny)),
            ("sleep", Some(Instant::Sleep)),
            ("Wake up!", Some(Instant::Wake)),
            ("stop talking", Some(Instant::Stop)),
            ("Next tab.", Some(Instant::NextTab)),
            ("previous tab", Some(Instant::PrevTab)),
            ("okay next tab", Some(Instant::NextTab)),
            // Anything more is a request for the assistant.
            ("yes and close the others", None),
            ("stop the api tab", None),
            ("approve the one in account two", None),
            ("next tab on account three", None),
            ("no wait open a new tab", None),
            ("new tab", None),
            ("switch to account two", None),
            ("how much quota is left", None),
            ("", None),
        ] {
            assert_eq!(match_instant(said, &t), want, "{said}");
        }
    }

    #[test]
    fn clear_yes_only() {
        for y in [
            "yes",
            "Yes.",
            "yeah do it",
            "go ahead",
            "confirm",
            "close them",
            "yes please close all of them",
            "sure",
            "okay",
            "do it",
            "yeah do you do it",
            "oh yeah i said yes",
            "that's fine yeah",
        ] {
            assert!(affirmative(y), "{y}");
        }
        for n in [
            "Do you close them?",
            "close them?",
            "no",
            "wait",
            "not yet",
            "what tabs?",
            "maybe",
            "hmm",
            "",
            "it's santa in his house",
            "don't close them",
            "yes? which ones",
            // Spoken: speech recognition drops the question mark.
            "do you close them",
            "did you do it",
            "is it done",
            "can you close them",
            "are they closed",
            "should i do it",
        ] {
            assert!(!affirmative(n), "{n}");
        }
    }

    #[test]
    fn configurable() {
        let t = table(&[
            "yep=approve".to_string(),
            "nope = deny".to_string(),
            "next tab".into(),
            "bogus".into(),
            "x=nothing".into(),
        ]);
        assert_eq!(match_instant("yep", &t), Some(Instant::Approve));
        assert_eq!(match_instant("nope", &t), Some(Instant::Deny));
        assert_eq!(match_instant("yes", &t), None, "only what is configured");
        assert_eq!(t.len(), 3);
        assert!(table(&[]).is_empty());
    }
}

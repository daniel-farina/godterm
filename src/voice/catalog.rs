pub const KOKORO_EN: &[(&str, &str, &str, &str)] = &[
    ("af_heart", "American", "female", "A"),
    ("af_bella", "American", "female", "A-"),
    ("af_nicole", "American", "female", "B-"),
    ("af_aoede", "American", "female", "C+"),
    ("af_kore", "American", "female", "C+"),
    ("af_sarah", "American", "female", "C+"),
    ("af_alloy", "American", "female", "C"),
    ("af_nova", "American", "female", "C"),
    ("af_sky", "American", "female", "C-"),
    ("af_jessica", "American", "female", "D"),
    ("af_river", "American", "female", "D"),
    ("am_fenrir", "American", "male", "C+"),
    ("am_michael", "American", "male", "C+"),
    ("am_puck", "American", "male", "C+"),
    ("am_echo", "American", "male", "D"),
    ("am_eric", "American", "male", "D"),
    ("am_liam", "American", "male", "D"),
    ("am_onyx", "American", "male", "D"),
    ("am_santa", "American", "male", "D-"),
    ("am_adam", "American", "male", "F+"),
    ("bf_emma", "British", "female", "B-"),
    ("bf_isabella", "British", "female", "C"),
    ("bf_alice", "British", "female", "D"),
    ("bf_lily", "British", "female", "D"),
    ("bm_fable", "British", "male", "C"),
    ("bm_george", "British", "male", "C"),
    ("bm_lewis", "British", "male", "D+"),
    ("bm_daniel", "British", "male", "D"),
];

pub fn kokoro_label(id: &str) -> Option<String> {
    KOKORO_EN
        .iter()
        .find(|v| v.0 == id)
        .map(|(_, a, g, grade)| format!("{a} {g}, grade {grade}"))
}

pub fn kokoro_name(id: &str) -> String {
    let n = id.split_once('_').map(|x| x.1).unwrap_or(id);
    let mut c = n.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

pub fn kokoro_choices(in_file: &[String]) -> Vec<String> {
    let mut v: Vec<String> = KOKORO_EN
        .iter()
        .map(|x| x.0)
        .filter(|id| in_file.is_empty() || in_file.iter().any(|f| f == id))
        .map(str::to_string)
        .collect();
    for f in in_file {
        let english = f.starts_with("af_")
            || f.starts_with("am_")
            || f.starts_with("bf_")
            || f.starts_with("bm_");
        if english && !v.contains(f) {
            v.push(f.clone());
        }
    }
    v
}

pub fn engine_choices() -> &'static [&'static str] {
    if super::local_tts::supported() {
        &["kokoro", "chatterbox", "ming", "say", "grok", "off"]
    } else {
        &["kokoro", "say", "grok", "off"]
    }
}

pub fn engine_line(id: &str) -> Option<&'static str> {
    Some(match id {
        "kokoro" => "built in, default, recommended · local · about 1 GB RAM · first audio 0.35 s · Apache 2.0",
        "chatterbox" => "Chatterbox Turbo · local, more natural, voice cloning · about 5 GB RAM · first audio 0.2 s · MIT (watermarked)",
        "ming" => "Ming-3.0 · local, voice design · about 4 GB RAM · first audio 1.5 to 4 s · MIT",
        "say" => "macOS voices · local · no extra RAM · first audio about 1 s · Apple system voices",
        "grok" => "Grok · cloud (xAI): the spoken text is sent to xAI · no local RAM · streamed · xAI terms",
        "off" => "silent: answers show as text",
        _ => return None,
    })
}

pub fn ming_choices() -> Vec<String> {
    let mut v: Vec<String> = super::local_tts::MING_VOICES
        .iter()
        .map(|s| s.to_string())
        .collect();
    v.push("design".into());
    v
}

pub fn ming_label(id: &str) -> Option<&'static str> {
    Some(match id {
        "US_Emma" => "American female",
        "US_Stella" => "American female",
        "US_James" => "American male",
        "UK_George" => "British male",
        "UK_Charlotte" => "British female",
        "design" => "a voice made from the description below",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kokoro_table_is_the_28_english_voices() {
        assert_eq!(KOKORO_EN.len(), 28);
        assert_eq!(KOKORO_EN[0].0, "af_heart");
        assert_eq!(
            kokoro_label("af_heart").unwrap(),
            "American female, grade A"
        );
        assert_eq!(kokoro_label("bm_george").unwrap(), "British male, grade C");
        assert!(kokoro_label("zf_xiaobei").is_none());
        assert_eq!(kokoro_name("af_heart"), "Heart");

        let first_b = KOKORO_EN.iter().position(|v| v.1 == "British").unwrap();
        assert!(KOKORO_EN[first_b..].iter().all(|v| v.1 == "British"));
        let names: std::collections::HashSet<String> =
            KOKORO_EN.iter().map(|v| kokoro_name(v.0)).collect();
        assert_eq!(names.len(), 28, "spoken names are unique");
    }

    #[test]
    fn picker_follows_the_file() {
        let file: Vec<String> = ["am_michael", "af_heart", "zf_xiaobei", "bf_emma", "af_new"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            kokoro_choices(&file),
            vec!["af_heart", "am_michael", "bf_emma", "af_new"]
        );
        assert_eq!(kokoro_choices(&[]).len(), 28);
    }

    #[test]
    fn engines_have_lines_and_kokoro_comes_first() {
        let c = engine_choices();
        assert_eq!(c[0], "kokoro");
        for e in c {
            assert!(engine_line(e).is_some(), "{e}");
        }
        assert!(engine_line("kokoro").unwrap().contains("default"));
        assert!(engine_line("grok").unwrap().contains("cloud"));
        assert!(engine_line("chatterbox").unwrap().contains("5 GB"));
        if !super::super::local_tts::supported() {
            assert!(!c.contains(&"chatterbox"));
        }
        assert_eq!(ming_choices().len(), 6);
    }
}

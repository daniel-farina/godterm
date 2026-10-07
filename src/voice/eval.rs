//! `godterm stt-eval`: compare speech recognition engines on synthetic
//! recordings (macOS `say` voices and Kokoro), clean and with noise mixed
//! in, for word error rate and latency. Files only: nothing is played and
//! no microphone is used.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::stt::{self, Whisper};

/// What people say to GodTerm.
pub const PHRASES: &[&str] = &[
    "close all tabs in all accounts",
    "open a new tab on account two and create a calculator",
    "what is everyone working on",
    "approve the one in account three if it is only running tests",
    "check the files on tab two",
    "move this tab to the account with the most usage left",
    "how much quota do I have left this week",
    "stop all loops and show me the dashboard",
    "tell the docs tab to also update the readme",
    "send the same prompt to every tab on account one",
    "make it a web app with a dark theme",
    "next tab",
    "previous tab",
    "stop talking",
    "yes",
    "no",
    "switch to account four",
    "open GodTerm settings",
    "run the failing tests in the api folder",
    "create five new tabs with an empty folder",
    "deny that and tell it to use cargo instead",
    "what did claude say in the calculator tab",
    "put account three full screen",
    "continue the conversation about the clock page",
];

/// Lower case words, digits spelled out, punctuation gone.
pub fn norm_words(s: &str) -> Vec<String> {
    let digits = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
    ];
    s.to_lowercase()
        .replace("godterm", "god term")
        .replace("readme", "read me")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(|w| match w.parse::<usize>() {
            Ok(n) if n < 10 => digits[n].to_string(),
            _ => w.trim_matches('\'').to_string(),
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word level edit distance.
pub fn word_errors(reference: &[String], hyp: &[String]) -> usize {
    let mut prev: Vec<usize> = (0..=hyp.len()).collect();
    for (i, r) in reference.iter().enumerate() {
        let mut cur = vec![i + 1; hyp.len() + 1];
        for (j, h) in hyp.iter().enumerate() {
            cur[j + 1] = (prev[j + 1] + 1)
                .min(cur[j] + 1)
                .min(prev[j] + usize::from(r != h));
        }
        prev = cur;
    }
    prev[hyp.len()]
}

struct Sample {
    phrase: usize,
    voice: String,
    noise: &'static str,
    wav: PathBuf,
}

fn run(cmd: &mut Command) -> Result<()> {
    let st = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if !st.success() {
        bail!("{cmd:?} failed");
    }
    Ok(())
}

/// Clean 16 kHz mono WAVs, then the same with pink noise at 15 and 5 dB SNR
/// (plus a little room reverb on the noisy ones).
fn build(dir: &Path, ffmpeg: &str) -> Result<Vec<Sample>> {
    std::fs::create_dir_all(dir)?;
    let say_voices = ["Samantha", "Daniel", "Karen"];
    let mut clean: Vec<(usize, String, PathBuf)> = vec![];
    for (i, p) in PHRASES.iter().enumerate() {
        for v in say_voices {
            let wav = dir.join(format!("{i:02}-{v}-clean.wav"));
            if !wav.is_file() {
                let aiff = dir.join("tmp.aiff");
                run(Command::new("say").args(["-v", v, "-o"]).arg(&aiff).arg(p))?;
                run(Command::new(ffmpeg)
                    .args(["-y", "-i"])
                    .arg(&aiff)
                    .args(["-ar", "16000", "-ac", "1"])
                    .arg(&wav))?;
            }
            clean.push((i, v.to_string(), wav));
        }
    }
    // Kokoro voices, when the model is here.
    let cfg = crate::config::Config::load_or_init()
        .map(|c| c.voice)
        .unwrap_or_default();
    if super::tts::kokoro_problem(&cfg).is_none() {
        let mut k = super::tts::load_kokoro(&cfg)?;
        for v in ["am_michael", "bf_emma"] {
            for (i, p) in PHRASES.iter().enumerate() {
                let wav = dir.join(format!("{i:02}-{v}-clean.wav"));
                if !wav.is_file() {
                    let (s, _) = k.synth(p, v, 1.0)?;
                    let raw = dir.join("tmp24.wav");
                    std::fs::write(&raw, super::kokoro::wav(&s))?;
                    run(Command::new(ffmpeg)
                        .args(["-y", "-i"])
                        .arg(&raw)
                        .args(["-ar", "16000", "-ac", "1"])
                        .arg(&wav))?;
                }
                clean.push((i, v.to_string(), wav));
            }
        }
    }
    let mut out = vec![];
    for (i, v, wav) in clean {
        out.push(Sample {
            phrase: i,
            voice: v.clone(),
            noise: "clean",
            wav: wav.clone(),
        });
        for (label, snr) in [("snr15", 15.0f32), ("snr5", 5.0)] {
            let noisy = dir.join(format!("{i:02}-{v}-{label}.wav"));
            if !noisy.is_file() {
                // Speech level is about -20 dBFS after leveling; noise at
                // -20 - SNR, half a second of lead in and out.
                let amp = 10f32.powf((-20.0 - snr) / 20.0) * 3.0;
                let filter = format!(
                    "[0:a]volume=1,apad=pad_dur=0.5,adelay=500|500,aecho=0.8:0.5:20:0.2[s];anoisesrc=color=pink:amplitude={amp}:sample_rate=16000[n];[s][n]amix=inputs=2:duration=first:normalize=0"
                );
                run(Command::new(ffmpeg)
                    .args(["-y", "-i"])
                    .arg(&wav)
                    .args(["-filter_complex", &filter, "-ar", "16000", "-ac", "1"])
                    .arg(&noisy))?;
            }
            out.push(Sample {
                phrase: i,
                voice: v.clone(),
                noise: label,
                wav: noisy,
            });
        }
    }
    Ok(out)
}

fn read_wav(p: &Path) -> Result<Vec<i16>> {
    let b = std::fs::read(p)?;
    let data = b
        .windows(4)
        .position(|w| w == b"data")
        .context("no data chunk")?
        + 8;
    Ok(b[data..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

/// One engine under test.
enum Engine {
    Whisper {
        w: Whisper,
        level: bool,
    },
    Apple {
        bin: PathBuf,
        context: Vec<String>,
    },
    /// xAI's recognizer (the configured login), with the vocabulary.
    Grok {
        g: Box<super::grok_stt::GrokStt>,
    },
}

impl Engine {
    fn run(&mut self, s: &Sample) -> Result<String> {
        match self {
            Engine::Whisper { w, level } => {
                let mut pcm = read_wav(&s.wav)?;
                if *level {
                    super::audio::normalize(&mut pcm, -20.0, 24.0);
                }
                Ok(w.transcribe_full(&pcm)?.text)
            }
            Engine::Apple { bin, context } => {
                let mut c = Command::new(bin);
                c.arg("--file").arg(&s.wav);
                if !context.is_empty() {
                    c.args(["--context", &context.join(",")]);
                }
                let out = c.output()?;
                let v: serde_json::Value =
                    serde_json::from_slice(&out.stdout).context("helper gave no JSON")?;
                if let Some(e) = v["error"].as_str() {
                    bail!("{e}");
                }
                Ok(v["text"].as_str().unwrap_or("").to_string())
            }
            Engine::Grok { g } => {
                let pcm = read_wav(&s.wav)?;
                g.transcribe(&pcm, &super::grok_stt::terms(&vocab()))
                    .map_err(|e| anyhow::anyhow!(e))
            }
        }
    }
}

fn vocab() -> String {
    "GodTerm, hey god, Claude, account one, account two, account three, account four, tab, approve, deny, dashboard, loops, quota, readme, cargo".into()
}

/// `grok`: also Grok, on only the first that many recordings (each is a
/// paid request), and the local engines on the same ones.
pub fn command(out: Option<PathBuf>, grok: Option<usize>) -> Result<()> {
    let cfg = crate::config::Config::load_or_init()?.voice;
    let dir = out.unwrap_or_else(|| crate::config::app_home().join("voice").join("eval"));
    println!(
        "building recordings in {} (files only, nothing is played)",
        dir.display()
    );
    let mut samples = build(&dir, &cfg.ffmpeg)?;
    if let Some(n) = grok {
        // A spread over phrases, voices and noise levels.
        let step = (samples.len() / n.max(1)).max(1);
        samples = samples
            .into_iter()
            .enumerate()
            .filter(|(i, _)| i % step == 0)
            .map(|(_, s)| s)
            .take(n.max(1))
            .collect();
    }
    println!(
        "{} recordings: {} phrases, voices x clean / 15 dB / 5 dB SNR",
        samples.len(),
        PHRASES.len()
    );
    let model = crate::config::expand_tilde(&cfg.model);
    let mk = |beam: u32, vad: bool, prompt: bool| -> Result<Whisper> {
        let mut w = Whisper::new(
            model.clone(),
            cfg.whisper_cli.clone(),
            cfg.whisper_server.clone(),
            cfg.language.clone(),
        );
        w.beam = beam;
        if vad {
            w.vad_model = Some(stt::ensure_vad_model()?);
        }
        if prompt {
            w.prompt = vocab();
        }
        w.start_server()?;
        Ok(w)
    };
    let mut engines: Vec<(&str, Box<dyn FnMut() -> Result<Engine>>)> = vec![
        (
            "whisper turbo, greedy (before)",
            Box::new(|| {
                Ok(Engine::Whisper {
                    w: mk(1, false, false)?,
                    level: false,
                })
            }),
        ),
        (
            "whisper turbo, beam 5 + prompt + leveling (now)",
            Box::new(|| {
                Ok(Engine::Whisper {
                    w: mk(5, false, true)?,
                    level: true,
                })
            }),
        ),
        (
            "whisper turbo, beam 5 + prompt + leveling + VAD",
            Box::new(|| {
                Ok(Engine::Whisper {
                    w: mk(5, true, true)?,
                    level: true,
                })
            }),
        ),
    ];
    if grok.is_some() {
        let c = cfg.clone();
        engines.push((
            "Grok (xAI) + keyterms",
            Box::new(move || {
                Ok(Engine::Grok {
                    g: Box::new(super::grok_stt::GrokStt::new(
                        c.grok.clone(),
                        &c.language,
                        c.grok_stt.timeout_ms,
                    )),
                })
            }),
        ));
    }
    let helper = super::apple_helper_path();
    if helper.is_file() {
        let h1 = helper.clone();
        engines.push((
            "Apple SpeechTranscriber",
            Box::new(move || {
                Ok(Engine::Apple {
                    bin: h1.clone(),
                    context: vec![],
                })
            }),
        ));
        let h2 = helper.clone();
        engines.push((
            "Apple SpeechTranscriber + context",
            Box::new(move || {
                Ok(Engine::Apple {
                    bin: h2.clone(),
                    context: vocab().split(", ").map(str::to_string).collect(),
                })
            }),
        ));
    } else {
        println!(
            "(no {}: run godterm install to build the Apple helper)",
            helper.display()
        );
    }
    let mut rows: Vec<String> = vec![];
    let mut report = vec![];
    for (name, make) in engines.iter_mut() {
        let mut e = match make() {
            Ok(e) => e,
            Err(err) => {
                println!("{name}: skipped ({err:#})");
                continue;
            }
        };
        let mut errs = [0usize; 3];
        let mut words = [0usize; 3];
        let mut exact = 0;
        let mut times = vec![];
        let mut misses = vec![];
        for s in &samples {
            let t = Instant::now();
            let hyp = e.run(s).unwrap_or_default();
            times.push(t.elapsed().as_millis() as u64);
            let r = norm_words(PHRASES[s.phrase]);
            let h = norm_words(&hyp);
            let k = match s.noise {
                "clean" => 0,
                "snr15" => 1,
                _ => 2,
            };
            let ne = word_errors(&r, &h);
            errs[k] += ne;
            words[k] += r.len();
            if ne == 0 {
                exact += 1;
            } else if misses.len() < 6 && s.noise != "snr5" {
                misses.push(format!(
                    "    {} [{} {}] heard \"{}\"",
                    PHRASES[s.phrase],
                    s.voice,
                    s.noise,
                    hyp.trim()
                ));
            }
            report.push(serde_json::json!({"engine": name, "voice": s.voice, "noise": s.noise, "ref": PHRASES[s.phrase], "hyp": hyp}));
        }
        times.sort();
        let wer = |k: usize| 100.0 * errs[k] as f64 / words[k].max(1) as f64;
        let all =
            100.0 * errs.iter().sum::<usize>() as f64 / words.iter().sum::<usize>().max(1) as f64;
        let line = format!(
            "| {name} | {:.1}% | {:.1}% | {:.1}% | {:.1}% | {:.0}% | {} ms | {} ms |",
            wer(0),
            wer(1),
            wer(2),
            all,
            100.0 * exact as f64 / samples.len() as f64,
            times[times.len() / 2],
            times[times.len() * 9 / 10]
        );
        println!("{line}");
        for m in &misses {
            println!("{m}");
        }
        rows.push(line);
        if let Engine::Whisper { w, .. } = &mut e {
            w.shutdown();
        }
    }
    println!("\n| engine | WER clean | WER 15 dB | WER 5 dB | WER all | exact | median | p90 |\n|---|---|---|---|---|---|---|---|");
    for r in &rows {
        println!("{r}");
    }
    let p = dir.join("results.jsonl");
    std::fs::write(
        &p,
        report
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    )?;
    println!("\nevery transcript: {}", p.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wer_counts_words() {
        let r = norm_words("Open a new tab on account 2.");
        assert_eq!(r, vec!["open", "a", "new", "tab", "on", "account", "two"]);
        assert_eq!(
            word_errors(&r, &norm_words("open new tab on account two")),
            1
        );
        assert_eq!(
            word_errors(&r, &norm_words("Open a new tab on account two")),
            0
        );
        assert_eq!(
            word_errors(&norm_words("GodTerm"), &norm_words("god term")),
            0
        );
        assert_eq!(word_errors(&[], &norm_words("x y")), 2);
    }
}

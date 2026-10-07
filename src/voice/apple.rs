//! Apple's on-device recognition through the `godterm-speech` helper
//! (SpeechAnalyzer + SpeechTranscriber), kept running in `--serve` mode:
//! one WAV path in, one JSON result out.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use super::stt::Decoded;

pub struct AppleSpeech {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    tmp: PathBuf,
    /// Contextual strings (account labels, tab names, command words).
    pub context: Vec<String>,
}

/// A transcript with the alternatives Apple offers.
#[derive(Debug, Clone, Default)]
pub struct Heard {
    pub decoded: Decoded,
    pub alternatives: Vec<String>,
}

impl AppleSpeech {
    pub fn available() -> bool {
        let bin = super::apple_helper_path();
        bin.is_file()
            && Command::new(&bin)
                .arg("--check")
                .stderr(Stdio::null())
                .output()
                .is_ok_and(|o| {
                    o.status.success()
                        && String::from_utf8_lossy(&o.stdout).contains("\"available\": true")
                })
    }

    pub fn start(locale: &str, tag: &str) -> Result<AppleSpeech> {
        let bin = super::apple_helper_path();
        if !bin.is_file() {
            bail!("{} is missing (godterm install builds it)", bin.display());
        }
        let mut child = Command::new(&bin)
            .args(["--serve", "--locale", locale])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting godterm-speech")?;
        super::track(child.id());
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("no stdout")?);
        let tmp = crate::config::app_home()
            .join("voice")
            .join(format!("apple-{tag}-{}.wav", std::process::id()));
        if let Some(d) = tmp.parent() {
            std::fs::create_dir_all(d)?;
        }
        Ok(AppleSpeech {
            child,
            stdin,
            stdout,
            tmp,
            context: vec![],
        })
    }

    pub fn transcribe(&mut self, samples: &[i16]) -> Result<Heard> {
        std::fs::write(&self.tmp, super::audio::wav_bytes(samples))?;
        let req = serde_json::json!({"wav": self.tmp, "context": self.context});
        writeln!(self.stdin, "{req}")?;
        self.stdin.flush()?;
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            bail!("godterm-speech stopped");
        }
        let v: serde_json::Value =
            serde_json::from_str(&line).context("godterm-speech gave no JSON")?;
        if let Some(e) = v["error"].as_str() {
            bail!("{e}");
        }
        Ok(Heard {
            decoded: Decoded {
                text: v["text"].as_str().unwrap_or("").trim().to_string(),
                conf: v["confidence"].as_f64().map(|c| c as f32),
                no_speech: None,
                logprob: None,
            },
            alternatives: v["alternatives"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(str::to_string))
                .collect(),
        })
    }
}

impl Drop for AppleSpeech {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        super::untrack(self.child.id());
        let _ = std::fs::remove_file(&self.tmp);
    }
}

/// n-best: when the best transcript is not an instant command but one of
/// the alternatives is exactly one, prefer it (short utterances only).
pub fn prefer_instant(
    best: &str,
    alternatives: &[String],
    instant: &[(String, crate::instant::Instant)],
) -> Option<String> {
    if best.split_whitespace().count() > 3 || crate::instant::match_instant(best, instant).is_some()
    {
        return None;
    }
    alternatives
        .iter()
        .find(|a| crate::instant::match_instant(a, instant).is_some())
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn n_best_prefers_an_exact_instant_command() {
        let t = crate::instant::table(
            &crate::instant::DEFAULTS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            prefer_instant("Next ten", &["next tab".into(), "next time".into()], &t).as_deref(),
            Some("next tab")
        );
        assert_eq!(
            prefer_instant("next tab", &["next time".into()], &t),
            None,
            "already one"
        );
        assert_eq!(
            prefer_instant("open a new tab on two", &["yes".into()], &t),
            None,
            "long utterances keep what was said"
        );
    }
}

//! Target speaker verification: only the enrolled user's voice gets
//! through. Each finished utterance is turned into a speaker embedding by a
//! small open model (WeSpeaker ResNet34 by default, ONNX through `ort`) and
//! compared with the centroid of the user's enrollment clips by cosine
//! similarity. Background talkers (a phone call across the room, a TV)
//! score low and are dropped before transcription.
//!
//! When the user talks over background chatter the whole utterance is
//! mixed, so the score is the best of the full clip and its 1.5 s windows:
//! a window where the user dominates still matches.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::speaker_fbank::{subtract_mean, Fbank, NUM_BINS};

/// A downloadable speaker model.
#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    pub name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
    pub license: &'static str,
    pub source: &'static str,
}

/// Known models; the first is the default.
pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        name: "wespeaker-resnet34",
        file: "wespeaker_en_voxceleb_resnet34_LM.onnx",
        url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx",
        sha256: "e9848563da86f263117134dfd7ad63c92355b37de492b55e325400c9d9c39012",
        bytes: 26_530_550,
        license: "CC BY 4.0 (WeSpeaker pretrained models; VoxCeleb data)",
        source: "WeSpeaker ResNet34-LM trained on VoxCeleb2, ONNX export by sherpa-onnx (k2-fsa)",
    },
];

pub fn models_dir() -> PathBuf {
    crate::config::home_dir()
        .join(".cache")
        .join("godterm-models")
}

/// The configured model: "auto" or a known name means that model in
/// ~/.cache/godterm-models; anything else is a path.
pub fn model_path(setting: &str) -> PathBuf {
    let s = setting.trim();
    if s.is_empty() || s == "auto" {
        return models_dir().join(MODELS[0].file);
    }
    if let Some(m) = MODELS.iter().find(|m| m.name == s) {
        return models_dir().join(m.file);
    }
    crate::config::expand_tilde(s)
}

pub fn model_info(path: &Path) -> Option<&'static ModelInfo> {
    let f = path.file_name()?.to_str()?;
    MODELS.iter().find(|m| m.file == f)
}

pub fn sha256_file(p: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// `godterm voice install-speaker [name]`: download once, check the hash.
pub fn install(name: Option<&str>) -> Result<PathBuf> {
    let m = match name {
        None => &MODELS[0],
        Some(n) => MODELS.iter().find(|m| m.name == n).ok_or_else(|| {
            anyhow!(
                "unknown speaker model {n} (known: {})",
                MODELS.iter().map(|m| m.name).collect::<Vec<_>>().join(", ")
            )
        })?,
    };
    let dir = models_dir();
    std::fs::create_dir_all(&dir)?;
    let p = dir.join(m.file);
    if p.is_file() && sha256_file(&p)? == m.sha256 {
        return Ok(p);
    }
    println!(
        "downloading  {} ({} MB) from {}",
        m.name,
        m.bytes / 1_000_000,
        m.url
    );
    let mut resp = ureq::get(m.url)
        .call()
        .context("downloading the speaker model")?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(200 << 20)
        .read_to_vec()?;
    use sha2::{Digest, Sha256};
    let got: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if got != m.sha256 {
        bail!(
            "checksum mismatch for {} (got {got}, want {})",
            m.file,
            m.sha256
        );
    }
    let tmp = p.with_extension("part");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

/// 1.5 s windows every 0.5 s (in 10 ms feature frames).
pub const WIN_FRAMES: usize = 150;
pub const HOP_FRAMES: usize = 50;

/// Window start frames over `frames` feature frames whose speech share
/// (frames flagged in `speech`) is at least 40%. Clips no longer than a
/// window and a bit get no windows (the full clip is the window).
pub fn windows(frames: usize, speech: &[bool]) -> Vec<usize> {
    if frames < WIN_FRAMES + HOP_FRAMES / 2 {
        return vec![];
    }
    let mut v = vec![];
    let mut s = 0;
    loop {
        let e = s + WIN_FRAMES;
        let n = speech[s.min(speech.len())..e.min(speech.len())]
            .iter()
            .filter(|b| **b)
            .count();
        if n * 10 >= WIN_FRAMES * 4 {
            v.push(s);
        }
        if e >= frames {
            break;
        }
        // The last window ends at the end of the clip.
        s = (s + HOP_FRAMES).min(frames - WIN_FRAMES);
    }
    v.dedup();
    v
}

/// Per feature frame (10 ms hop): does it carry speech energy (within 30
/// dB of the loudest frame and over -60 dBFS)?
pub fn speech_frames(samples: &[i16]) -> Vec<bool> {
    let n = Fbank::num_frames(samples.len());
    let lv: Vec<f32> = (0..n)
        .map(|f| {
            let s = (f * 160).min(samples.len());
            let e = (s + 400).min(samples.len());
            super::audio::db(super::audio::rms(&samples[s..e]))
        })
        .collect();
    let peak = lv.iter().cloned().fold(-90.0, f32::max);
    lv.iter().map(|d| *d > peak - 30.0 && *d > -60.0).collect()
}

pub fn l2_normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut d, mut na, mut nb) = (0f32, 0f32, 0f32);
    for (x, y) in a.iter().zip(b) {
        d += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    d / (na.sqrt() * nb.sqrt())
}

/// The normalized mean of normalized embeddings.
pub fn centroid<'a>(embs: impl IntoIterator<Item = &'a Vec<f32>>) -> Vec<f32> {
    let mut c: Vec<f32> = vec![];
    for e in embs {
        if c.is_empty() {
            c = vec![0.0; e.len()];
        }
        let n = e.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        for (ci, x) in c.iter_mut().zip(e) {
            *ci += x / n;
        }
    }
    l2_normalize(&mut c);
    c
}

/// The embeddings of one utterance: the full clip first, then its windows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipEmbs {
    pub full: Vec<f32>,
    #[serde(default)]
    pub windows: Vec<Vec<f32>>,
}

impl ClipEmbs {
    /// Best match to the centroid over the full clip and its windows.
    pub fn score(&self, c: &[f32]) -> f32 {
        std::iter::once(&self.full)
            .chain(&self.windows)
            .map(|e| cosine(e, c))
            .fold(-1.0, f32::max)
    }
}

/// A loaded speaker embedding model.
pub struct SpeakerModel {
    session: ort::session::Session,
    /// Multiply int16 samples by this (1 for kaldi scale, 1/32768).
    scale: f32,
    /// Subtract the per utterance feature mean.
    cmn: bool,
    pub dim: usize,
    pub load_ms: u128,
    pub path: PathBuf,
    fbank: Fbank,
    /// Batched windows work (dynamic batch axis).
    batch: bool,
}

impl SpeakerModel {
    pub fn load(path: &Path, threads: usize) -> Result<SpeakerModel> {
        use ort::session::builder::GraphOptimizationLevel;
        use ort::session::Session;
        if !path.is_file() {
            bail!(
                "{} is missing (godterm voice install-speaker fetches it)",
                path.display()
            );
        }
        let t = Instant::now();
        let session = Session::builder()
            .map_err(|e| anyhow!("onnxruntime: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!("{e}"))?
            .with_intra_threads(threads.max(1))
            .map_err(|e| anyhow!("{e}"))?
            .commit_from_file(path)
            .map_err(|e| anyhow!("loading {}: {e}", path.display()))?;
        let meta = |k: &str| -> Option<String> { session.metadata().ok()?.custom(k).ok()? };
        if std::env::var_os("GODTERM_SPEAKER_META").is_some() {
            let keys = session
                .metadata()
                .ok()
                .and_then(|m| m.custom_keys().ok())
                .unwrap_or_default();
            for k in keys {
                eprintln!("meta {k} = {:?}", meta(&k));
            }
        }
        // sherpa-onnx exports say how they were trained; WeSpeaker uses
        // int16 scale samples and mean normalized features.
        let normalize = meta("normalize_samples").map(|v| v.trim() == "1");
        let framework = meta("framework").unwrap_or_default();
        let scale = match normalize {
            Some(true) => 1.0 / 32768.0,
            Some(false) => 1.0,
            None if framework.contains("3d") => 1.0 / 32768.0,
            None => 1.0,
        };
        let cmn = meta("feature_normalize_type")
            .map(|v| v.contains("mean"))
            .unwrap_or(true);
        let dim = meta("output_dim")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        let mut m = SpeakerModel {
            session,
            scale,
            cmn,
            dim,
            load_ms: 0,
            path: path.to_path_buf(),
            fbank: Fbank::new(),
            batch: true,
        };
        // Probe once: output size, and whether a batch of 2 works.
        let probe: Vec<i16> = (0..24_000)
            .map(|i| ((i as f32 * 0.07).sin() * 3000.0) as i16)
            .collect();
        let feats = m.features(&probe);
        let t_frames = feats.len() / NUM_BINS;
        let one = m.run(&feats, 1, t_frames)?;
        m.dim = one[0].len();
        let mut two = feats.clone();
        two.extend_from_slice(&feats);
        m.batch = m.run(&two, 2, t_frames).is_ok_and(|v| v.len() == 2);
        m.load_ms = t.elapsed().as_millis();
        Ok(m)
    }

    /// Windows run as one batch.
    pub fn batch_ok(&self) -> bool {
        self.batch
    }

    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn features(&self, samples: &[i16]) -> Vec<f32> {
        let x: Vec<f32> = samples.iter().map(|s| *s as f32 * self.scale).collect();
        self.fbank.compute(&x)
    }

    /// Run the model on `b` utterances of `t` frames each (row major).
    fn run(&mut self, feats: &[f32], b: usize, t: usize) -> Result<Vec<Vec<f32>>> {
        let name = self
            .session
            .inputs
            .first()
            .map(|i| i.name.clone())
            .ok_or_else(|| anyhow!("model has no input"))?;
        let input = ort::value::Tensor::from_array(([b, t, NUM_BINS], feats.to_vec()))
            .map_err(|e| anyhow!("{e}"))?;
        let out = self
            .session
            .run(ort::inputs![name.as_str() => input])
            .map_err(|e| anyhow!("speaker model: {e}"))?;
        let arr = out[0]
            .try_extract_array::<f32>()
            .map_err(|e| anyhow!("{e}"))?;
        let flat: Vec<f32> = arr.iter().copied().collect();
        if flat.is_empty() || !flat.len().is_multiple_of(b) {
            bail!("unexpected speaker model output ({} values)", flat.len());
        }
        let d = flat.len() / b;
        Ok(flat
            .chunks(d)
            .map(|c| {
                let mut v = c.to_vec();
                l2_normalize(&mut v);
                v
            })
            .collect())
    }

    /// Embeddings of one utterance (full clip plus its speech windows).
    pub fn embed(&mut self, samples: &[i16]) -> Result<ClipEmbs> {
        self.embed_until(samples, None)
    }

    /// Like `embed`, but stop after the full clip when it already scores
    /// at least `stop.1` against `stop.0` (the user alone, the common case:
    /// the windows only matter when the clip is mixed).
    pub fn embed_until(
        &mut self,
        samples: &[i16],
        stop: Option<(&[f32], f32)>,
    ) -> Result<ClipEmbs> {
        if samples.len() < 8_000 {
            bail!("too short for a speaker check ({} ms)", samples.len() / 16);
        }
        let raw = self.features(samples);
        let frames = raw.len() / NUM_BINS;
        let mut feats = raw.clone();
        if self.cmn {
            subtract_mean(&mut feats, NUM_BINS);
        }
        let full = self.run(&feats, 1, frames)?.remove(0);
        if stop.is_some_and(|(c, thr)| cosine(&full, c) >= thr) {
            return Ok(ClipEmbs {
                full,
                windows: vec![],
            });
        }
        let starts = windows(frames, &speech_frames(samples));
        let mut wins: Vec<f32> = Vec::with_capacity(starts.len() * WIN_FRAMES * NUM_BINS);
        for s in &starts {
            let mut w = raw[s * NUM_BINS..(s + WIN_FRAMES) * NUM_BINS].to_vec();
            if self.cmn {
                subtract_mean(&mut w, NUM_BINS);
            }
            wins.extend(w);
        }
        let windows = if starts.is_empty() {
            vec![]
        } else if self.batch {
            self.run(&wins, starts.len(), WIN_FRAMES)?
        } else {
            let mut v = vec![];
            for w in wins.chunks(WIN_FRAMES * NUM_BINS) {
                v.push(self.run(w, 1, WIN_FRAMES)?.remove(0));
            }
            v
        };
        Ok(ClipEmbs { full, windows })
    }
}

/// Threads for the speaker model: it is small, two or four are plenty.
pub fn auto_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 4).clamp(1, 4))
        .unwrap_or(2)
}

/// When the speaker check applies (`voice.speaker_lock`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lock {
    Off,
    /// Only hands free open mic utterances (where background talkers bite).
    OpenMicOnly,
    /// Every hands free utterance (wake word and open mic).
    Always,
}

impl Lock {
    pub fn parse(s: &str) -> Lock {
        match s.trim() {
            "off" | "false" | "none" => Lock::Off,
            "always" | "on" | "true" => Lock::Always,
            _ => Lock::OpenMicOnly,
        }
    }

    /// Push to talk is always the user at the keyboard: never checked.
    pub fn applies(self, ptt: bool, open_mic: bool) -> bool {
        match self {
            Lock::Off => false,
            Lock::OpenMicOnly => open_mic && !ptt,
            Lock::Always => !ptt,
        }
    }
}

/// How loud and how bright the user sounds (see speaker_gate).
pub use super::speaker_gate::Acoustic;

/// The enrolled voice: `~/.godterm/voice/speaker_profile.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpeakerProfile {
    /// Model file the embeddings came from (a new model needs retraining).
    pub model: String,
    pub dim: usize,
    pub centroid: Vec<f32>,
    pub clips: Vec<ClipEmbs>,
    #[serde(default)]
    pub acoustics: Vec<Acoustic>,
    /// Leave one out scores of the user's own clips.
    pub user_scores: Vec<f32>,
    /// Scores of other voices (synthesized at enrollment).
    #[serde(default)]
    pub negative_scores: Vec<f32>,
    /// Calibrated accept threshold.
    pub threshold: f32,
    pub trained_at: String,
}

/// The lowest and highest threshold calibration may pick.
pub const MIN_THRESHOLD: f32 = 0.30;
pub const MAX_THRESHOLD: f32 = 0.65;
/// Without any negatives: this far under the user's low scores.
pub const NO_NEG_GAP: f32 = 0.20;
/// Keep at most this many enrollment clips (the newest).
pub const MAX_CLIPS: usize = 40;

fn percentile(v: &[f32], p: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let i = ((s.len() - 1) as f32 * p).round() as usize;
    s[i.min(s.len() - 1)]
}

/// Centroid, leave one out user scores, negative scores, threshold.
pub type Calibration = (Vec<f32>, Vec<f32>, Vec<f32>, f32);

/// Centroid, leave one out user scores, negative scores and the threshold.
/// The threshold sits halfway between the user's low scores (20th
/// percentile) and the best scoring other voice, clamped to a sane range
/// and always a little under the user's low scores.
pub fn calibrate(clips: &[ClipEmbs], negatives: &[ClipEmbs]) -> Result<Calibration> {
    if clips.len() < 2 {
        bail!("need at least 2 clips of your voice (have {})", clips.len());
    }
    let c = centroid(clips.iter().map(|k| &k.full));
    let user: Vec<f32> = (0..clips.len())
        .map(|i| {
            let others = centroid(
                clips
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, k)| &k.full),
            );
            clips[i].score(&others)
        })
        .collect();
    let neg: Vec<f32> = negatives.iter().map(|n| n.score(&c)).collect();
    let user_lo = percentile(&user, 0.2);
    let thr = match neg.iter().cloned().reduce(f32::max) {
        Some(hi) => (user_lo + hi) / 2.0,
        None => user_lo - NO_NEG_GAP,
    };
    let thr = thr.min(user_lo - 0.05).clamp(MIN_THRESHOLD, MAX_THRESHOLD);
    Ok((c, user, neg, thr))
}

impl SpeakerProfile {
    pub fn path() -> PathBuf {
        crate::config::app_home()
            .join("voice")
            .join("speaker_profile.json")
    }

    pub fn load() -> Option<SpeakerProfile> {
        let s = std::fs::read_to_string(Self::path()).ok()?;
        serde_json::from_str(&s).ok()
    }

    pub fn save(&self) -> Result<()> {
        let p = Self::path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, serde_json::to_string(self)?)?;
        Ok(())
    }

    pub fn reset() -> Result<()> {
        match std::fs::remove_file(Self::path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    pub fn build(
        model: &str,
        clips: Vec<ClipEmbs>,
        acoustics: Vec<Acoustic>,
        negatives: &[ClipEmbs],
    ) -> Result<SpeakerProfile> {
        let (centroid, user_scores, negative_scores, threshold) = calibrate(&clips, negatives)?;
        Ok(SpeakerProfile {
            model: model.to_string(),
            dim: centroid.len(),
            centroid,
            clips,
            acoustics,
            user_scores,
            negative_scores,
            threshold,
            trained_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        })
    }

    /// The threshold in use: a fixed `speaker_threshold` (over 0), else the
    /// calibrated one plus `speaker_margin`.
    pub fn effective_threshold(&self, fixed: f32, margin: f32) -> f32 {
        if fixed > 0.0 {
            fixed.clamp(0.0, 1.0)
        } else {
            (self.threshold + margin).clamp(0.0, 1.0)
        }
    }

    /// One line for the training dialog, doctor and the log.
    pub fn summary(&self) -> String {
        let lo = self.user_scores.iter().cloned().fold(1.0, f32::min);
        let hi = self.user_scores.iter().cloned().fold(-1.0, f32::max);
        let neg = self
            .negative_scores
            .iter()
            .cloned()
            .reduce(f32::max)
            .map(|n| format!(", other voices up to {n:.2}"))
            .unwrap_or_default();
        format!(
            "{} clips, your voice {lo:.2} to {hi:.2}{neg}, threshold {:.2}",
            self.clips.len(),
            self.threshold
        )
    }
}

/// One scored utterance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    pub score: f32,
    pub threshold: f32,
    pub accepted: bool,
    /// Embedding time.
    pub ms: u32,
    /// Windows scored (0 when the full clip already matched).
    pub windows: usize,
}

/// Score an utterance against the profile.
pub fn verify(
    model: &mut SpeakerModel,
    profile: &SpeakerProfile,
    samples: &[i16],
    threshold: f32,
) -> Result<Verdict> {
    let t = Instant::now();
    let e = model.embed_until(samples, Some((&profile.centroid, threshold)))?;
    let score = e.score(&profile.centroid);
    Ok(Verdict {
        score,
        threshold,
        accepted: score >= threshold,
        ms: t.elapsed().as_millis() as u32,
        windows: e.windows.len(),
    })
}

/// Commands for the speaker guard (through the voice thread).
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    /// Start enrolling; `append` keeps the saved clips.
    EnrollStart {
        append: bool,
    },
    EnrollCalibrate,
    EnrollSave,
    EnrollCancel,
    Reset,
}

/// What the guard reports back to the app.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Checked(Check),
    /// Enrollment clips so far.
    Clips(usize),
    Calibrated(Result<String, String>),
    Saved(Result<String, String>),
    Error(String),
}

impl Guard {
    /// Run a command; what to tell the app.
    pub fn command(&mut self, c: Cmd) -> Option<Event> {
        let err = |e: anyhow::Error| format!("{e:#}");
        match c {
            Cmd::EnrollStart { append } => self
                .enroll_start(append)
                .err()
                .map(|e| Event::Error(err(e))),
            Cmd::EnrollCalibrate => Some(Event::Calibrated(self.enroll_calibrate().map_err(err))),
            Cmd::EnrollSave => Some(Event::Saved(self.enroll_save().map_err(err))),
            Cmd::EnrollCancel => {
                self.enroll_cancel();
                None
            }
            Cmd::Reset => self.reset().err().map(|e| Event::Error(err(e))),
        }
    }
}

/// What the speaker guard made of one utterance.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Check {
    /// The speaker score, when a profile and the model are there.
    pub verdict: Option<Verdict>,
    /// Why it sounds far away (near field gate), if it does.
    pub far: Option<String>,
    /// The lock applies to this utterance (hands free, per the setting).
    pub applies: bool,
}

impl Check {
    /// Drop it: the lock applies and the voice or the distance says no.
    pub fn reject(&self) -> bool {
        self.applies && (self.verdict.is_some_and(|v| !v.accepted) || self.far.is_some())
    }

    /// "speaker mismatch, ignored (score 0.41)" and the like.
    pub fn reason(&self) -> Option<String> {
        if !self.reject() {
            return None;
        }
        Some(match (&self.verdict, &self.far) {
            (Some(v), _) if !v.accepted => format!("speaker mismatch (score {:.2})", v.score),
            (_, Some(f)) => f.clone(),
            _ => "rejected".into(),
        })
    }
}

/// Enrollment and per utterance checks, owned by the voice thread.
pub struct Guard {
    pub lock: Lock,
    fixed: f32,
    margin: f32,
    near_field: bool,
    near_db: f32,
    model_path: PathBuf,
    model: Option<SpeakerModel>,
    model_err: Option<String>,
    pub profile: Option<SpeakerProfile>,
    near: super::speaker_gate::NearField,
    fbank: Fbank,
    /// Clips collected while enrolling.
    enroll: Option<Vec<(ClipEmbs, Acoustic)>>,
    /// Calibrated, not saved yet.
    pending: Option<SpeakerProfile>,
    /// Synthesized other voices (made once per run).
    negatives: Option<Vec<ClipEmbs>>,
}

impl Guard {
    pub fn new(c: &crate::config::VoiceCfg) -> Guard {
        let model_path = model_path(&c.speaker_model);
        let file = model_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        // A profile from another model is useless (retrain).
        let profile = SpeakerProfile::load().filter(|p| p.model == file);
        let near = super::speaker_gate::NearField::new(
            profile
                .as_ref()
                .map(|p| p.acoustics.clone())
                .unwrap_or_default(),
        );
        let mut g = Guard {
            lock: Lock::parse(&c.speaker_lock),
            fixed: c.speaker_threshold,
            margin: c.speaker_margin,
            near_field: c.near_field,
            near_db: c.near_field_db,
            model_path,
            model: None,
            model_err: None,
            profile,
            near,
            fbank: Fbank::new(),
            enroll: None,
            pending: None,
            negatives: None,
        };
        if g.lock != Lock::Off && g.profile.is_some() && !cfg!(test) {
            let _ = g.ensure_model();
        }
        g
    }

    /// The model, loaded on first use.
    pub fn ensure_model(&mut self) -> Result<&mut SpeakerModel> {
        if self.model.is_none() {
            if let Some(e) = &self.model_err {
                bail!("{e}");
            }
            match SpeakerModel::load(&self.model_path, auto_threads()) {
                Ok(m) => {
                    crate::log::info(&format!(
                        "voice: speaker model {} loaded in {} ms",
                        m.file_name(),
                        m.load_ms
                    ));
                    self.model = Some(m);
                }
                Err(e) => {
                    self.model_err = Some(format!("{e:#}"));
                    bail!("{e:#}");
                }
            }
        }
        self.model
            .as_mut()
            .ok_or_else(|| anyhow!("no speaker model"))
    }

    pub fn threshold(&self) -> Option<f32> {
        self.profile
            .as_ref()
            .map(|p| p.effective_threshold(self.fixed, self.margin))
    }

    /// Score one finished utterance. Push to talk and accepted hands free
    /// speech teach the near field gate the user's level.
    pub fn check(&mut self, samples: &[i16], ptt: bool, open_mic: bool) -> Check {
        let applies = self.lock.applies(ptt, open_mic);
        let ac = Acoustic::of(samples, &self.fbank);
        let mut verdict = None;
        if let (Some(thr), true) = (self.threshold(), self.lock != Lock::Off || ptt) {
            if self.ensure_model().is_ok() {
                let p = self.profile.clone().unwrap_or_default();
                if let Some(m) = self.model.as_mut() {
                    verdict = verify(m, &p, samples, thr).ok();
                }
            }
        }
        let far = (self.near_field && applies)
            .then(|| self.near.check(ac, self.near_db))
            .flatten();
        let c = Check {
            verdict,
            far,
            applies,
        };
        if ptt || (!c.reject() && c.verdict.is_some_and(|v| v.accepted)) {
            self.near.accept(ac);
        }
        c
    }

    pub fn enrolling(&self) -> bool {
        self.enroll.is_some()
    }

    /// Start collecting clips; `append` keeps the saved profile's clips.
    pub fn enroll_start(&mut self, append: bool) -> Result<()> {
        self.ensure_model()?;
        let mut v = vec![];
        if append {
            if let Some(p) = &self.profile {
                let ac = p
                    .acoustics
                    .iter()
                    .copied()
                    .chain(std::iter::repeat(Acoustic::default()));
                v = p.clips.iter().cloned().zip(ac).collect();
            }
        }
        self.enroll = Some(v);
        self.pending = None;
        Ok(())
    }

    /// Add one enrollment clip; the count so far.
    pub fn enroll_clip(&mut self, samples: &[i16]) -> Result<usize> {
        let ac = Acoustic::of(samples, &self.fbank);
        let e = self.ensure_model()?.embed(samples)?;
        let v = self
            .enroll
            .as_mut()
            .ok_or_else(|| anyhow!("not enrolling"))?;
        v.push((e, ac));
        if v.len() > MAX_CLIPS {
            v.remove(0);
        }
        Ok(v.len())
    }

    /// Calibrate on the clips (other voices synthesized as negatives).
    pub fn enroll_calibrate(&mut self) -> Result<String> {
        let clips = self
            .enroll
            .clone()
            .ok_or_else(|| anyhow!("not enrolling"))?;
        let m = self.ensure_model()?;
        let name = m.file_name();
        if self.negatives.is_none() {
            let m = self.ensure_model()?;
            self.negatives = Some(super::speaker_cli::synth_negatives(m));
        }
        let negs = self.negatives.clone().unwrap_or_default();
        let (embs, acs): (Vec<ClipEmbs>, Vec<Acoustic>) = clips.into_iter().unzip();
        let acs = acs.into_iter().filter(|a| a.level_db > -89.0).collect();
        let p = SpeakerProfile::build(&name, embs, acs, &negs)?;
        let s = p.summary();
        self.pending = Some(p);
        Ok(s)
    }

    /// Save the calibrated profile and use it from now on.
    pub fn enroll_save(&mut self) -> Result<String> {
        let p = self
            .pending
            .take()
            .ok_or_else(|| anyhow!("nothing calibrated"))?;
        p.save()?;
        let s = p.summary();
        self.near = super::speaker_gate::NearField::new(p.acoustics.clone());
        self.profile = Some(p);
        self.enroll = None;
        Ok(s)
    }

    pub fn enroll_cancel(&mut self) {
        self.enroll = None;
        self.pending = None;
    }

    pub fn reset(&mut self) -> Result<()> {
        SpeakerProfile::reset()?;
        self.profile = None;
        self.near = Default::default();
        Ok(())
    }
}

/// 16 kHz mono 16-bit WAV (any chunk layout) to samples.
pub fn read_wav16(p: &Path) -> Result<Vec<i16>> {
    let b = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        bail!("{} is not a WAV file", p.display());
    }
    let mut i = 12;
    let mut fmt_ok = false;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        let body = &b[i + 8..(i + 8 + len).min(b.len())];
        if id == b"fmt " && body.len() >= 16 {
            let ch = u16::from_le_bytes([body[2], body[3]]);
            let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            let bits = u16::from_le_bytes([body[14], body[15]]);
            if ch != 1 || rate != super::audio::RATE || bits != 16 {
                bail!(
                    "{}: need 16 kHz mono 16-bit ({ch} ch, {rate} Hz, {bits} bit)",
                    p.display()
                );
            }
            fmt_ok = true;
        }
        if id == b"data" {
            if !fmt_ok {
                bail!("{}: data before fmt", p.display());
            }
            return Ok(body
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect());
        }
        i += 8 + len + (len & 1);
    }
    bail!("{}: no data chunk", p.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: &[f32]) -> Vec<f32> {
        let mut v = v.to_vec();
        l2_normalize(&mut v);
        v
    }

    #[test]
    fn cosine_scoring() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert!((cosine(&[1.0, 1.0], &[-1.0, -1.0]) + 1.0).abs() < 1e-6);
        // Scale does not matter.
        assert!((cosine(&[3.0, 4.0], &[6.0, 8.0]) - 1.0).abs() < 1e-6);
        // Degenerate input scores 0, never NaN.
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0);
        let c = centroid([&vec![2.0, 0.0], &vec![0.0, 1.0]]);
        assert!((c[0] - c[1]).abs() < 1e-6, "{c:?}");
        assert!((c.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn best_window_wins() {
        let me = unit(&[1.0, 0.1, 0.0]);
        let clip = ClipEmbs {
            // The full clip is mixed with someone else...
            full: unit(&[0.5, 0.8, 0.0]),
            // ...but one window is mostly the user.
            windows: vec![unit(&[0.0, 1.0, 0.2]), unit(&[0.95, 0.15, 0.0])],
        };
        let s = clip.score(&me);
        assert!(s > 0.99, "{s}");
        assert!(cosine(&clip.full, &me) < 0.7);
    }

    #[test]
    fn windows_cover_speech_only() {
        // Short clips: the full clip only.
        assert!(windows(100, &[true; 100]).is_empty());
        assert!(windows(174, &[true; 174]).is_empty());
        // 3 s of speech: 1.5 s windows every 0.5 s, the last at the end.
        let w = windows(300, &[true; 300]);
        assert_eq!(w, vec![0, 50, 100, 150]);
        let w = windows(320, &[true; 320]);
        assert_eq!(w, vec![0, 50, 100, 150, 170]);
        // Silence in the middle: windows that are mostly silence are skipped.
        let mut sp = vec![true; 400];
        sp[120..330].iter_mut().for_each(|b| *b = false);
        let w = windows(400, &sp);
        assert!(w.contains(&0) && w.contains(&250), "{w:?}");
        assert!(!w.contains(&150), "{w:?}");
        for s in &w {
            assert!(s + WIN_FRAMES <= 400);
        }
    }

    #[test]
    fn calibration_separates_user_from_others() {
        // The user's clips cluster around one direction, others elsewhere.
        let user: Vec<ClipEmbs> = (0..6)
            .map(|i| ClipEmbs {
                full: unit(&[1.0, 0.15 * (i as f32 - 2.5), 0.1, 0.0]),
                windows: vec![],
            })
            .collect();
        let neg: Vec<ClipEmbs> = (0..4)
            .map(|i| ClipEmbs {
                full: unit(&[0.2, 0.0, 1.0, 0.3 * i as f32]),
                windows: vec![],
            })
            .collect();
        let (c, us, ns, thr) = calibrate(&user, &neg).unwrap();
        assert_eq!(c.len(), 4);
        assert_eq!(us.len(), 6);
        let neg_hi = ns.iter().cloned().fold(-1.0, f32::max);
        let user_lo = us.iter().cloned().fold(1.0, f32::min);
        assert!(
            thr > neg_hi && thr < user_lo,
            "{neg_hi} < {thr} < {user_lo}"
        );
        assert!((MIN_THRESHOLD..=MAX_THRESHOLD).contains(&thr));
        // No negatives: under the user's scores, never below the floor.
        let (_, _, _, t2) = calibrate(&user, &[]).unwrap();
        assert!(t2 >= MIN_THRESHOLD && t2 < user_lo);
        // One clip is not enough.
        assert!(calibrate(&user[..1], &neg).is_err());
    }

    #[test]
    fn threshold_override_and_margin() {
        let p = SpeakerProfile {
            threshold: 0.42,
            ..Default::default()
        };
        assert!((p.effective_threshold(0.0, 0.0) - 0.42).abs() < 1e-6);
        assert!((p.effective_threshold(0.0, 0.05) - 0.47).abs() < 1e-6);
        assert!((p.effective_threshold(0.6, 0.05) - 0.6).abs() < 1e-6);
    }

    #[test]
    fn lock_modes() {
        assert_eq!(Lock::parse("off"), Lock::Off);
        assert_eq!(Lock::parse("always"), Lock::Always);
        assert_eq!(Lock::parse("open_mic_only"), Lock::OpenMicOnly);
        assert_eq!(Lock::parse("weird"), Lock::OpenMicOnly);
        // Push to talk is never checked.
        for l in [Lock::Off, Lock::OpenMicOnly, Lock::Always] {
            assert!(!l.applies(true, true));
        }
        assert!(Lock::OpenMicOnly.applies(false, true));
        assert!(!Lock::OpenMicOnly.applies(false, false));
        assert!(Lock::Always.applies(false, false));
        assert!(!Lock::Off.applies(false, true));
    }

    #[test]
    fn profile_round_trips() {
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let p = SpeakerProfile {
            model: "m.onnx".into(),
            centroid: vec![0.6, 0.8],
            threshold: 0.4,
            ..Default::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: SpeakerProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn wav_reader_skips_extra_chunks() {
        let mut w = super::super::audio::wav_bytes(&[1, -2, 3]);
        // Insert a LIST chunk between fmt and data.
        let at = w.windows(4).position(|c| c == b"data").unwrap();
        let mut extra = b"LIST".to_vec();
        extra.extend_from_slice(&3u32.to_le_bytes());
        extra.extend_from_slice(&[0, 0, 0, 0]); // 3 bytes + pad
        w.splice(at..at, extra);
        let p = std::env::temp_dir().join(format!("gt-wav-{}.wav", std::process::id()));
        std::fs::write(&p, &w).unwrap();
        assert_eq!(read_wav16(&p).unwrap(), vec![1, -2, 3]);
        let _ = std::fs::remove_file(&p);
    }

    /// With the real model installed: a Samantha profile accepts Samantha
    /// and rejects other macOS voices (skipped without the model or `say`).
    #[test]
    fn real_model_accepts_the_enrolled_voice() {
        let path = model_path("auto");
        if !path.is_file() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("gt-spk-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let say = |voice: &str, text: &str, name: &str| -> Option<Vec<i16>> {
            let p = dir.join(name);
            let ok = std::process::Command::new("say")
                .args(["-v", voice, "--data-format=LEI16@16000", "-o"])
                .arg(&p)
                .arg(text)
                .status()
                .is_ok_and(|s| s.success());
            ok.then(|| read_wav16(&p).ok()).flatten()
        };
        let lines = [
            "The build finished a few minutes ago.",
            "Can you send me the report later?",
            "Let's go over the plan tomorrow morning.",
            "Please read the last reply back to me.",
        ];
        let Some(first) = say("Samantha", lines[0], "e0.wav") else {
            return; // no `say` here
        };
        let mut m = SpeakerModel::load(&path, 2).unwrap();
        let mut clips = vec![m.embed(&first).unwrap()];
        for (i, l) in lines.iter().enumerate().skip(1) {
            let s = say("Samantha", l, &format!("e{i}.wav")).unwrap();
            clips.push(m.embed(&s).unwrap());
        }
        let p = SpeakerProfile::build(&m.file_name(), clips, vec![], &[]).unwrap();
        let test = "Open the sessions view and sort by newest.";
        let me = say("Samantha", test, "t.wav").unwrap();
        let v = verify(&mut m, &p, &me, p.threshold).unwrap();
        assert!(v.accepted, "{v:?} {}", p.summary());
        for other in ["Daniel", "Karen", "Fred"] {
            let Some(o) = say(other, test, &format!("o-{other}.wav")) else {
                continue;
            };
            let v = verify(&mut m, &p, &o, p.threshold).unwrap();
            assert!(!v.accepted, "{other}: {v:?} {}", p.summary());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The guard end to end with the real model and `say` voices: enroll,
    /// calibrate, save, then open mic accepts the user and drops another
    /// voice and a far away one; push to talk is never dropped.
    #[test]
    fn guard_enrolls_and_locks_open_mic() {
        if !model_path("auto").is_file() || !cfg!(target_os = "macos") {
            return;
        }
        let _g = crate::config::testing::LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("gt-guard-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let say = |voice: &str, text: &str, name: &str| -> Option<Vec<i16>> {
            let p = dir.join(name);
            std::process::Command::new("say")
                .args(["-v", voice, "--data-format=LEI16@16000", "-o"])
                .arg(&p)
                .arg(text)
                .status()
                .ok()?
                .success()
                .then(|| read_wav16(&p).ok())
                .flatten()
        };
        let mut g = Guard::new(&crate::config::VoiceCfg::default());
        assert_eq!(g.lock, Lock::OpenMicOnly);
        g.enroll_start(false).unwrap();
        let lines = [
            "Hey god, open a new tab on account two.",
            "The build finished a few minutes ago.",
            "Can you send me the report later?",
            "Please read the last reply back to me.",
            "Approve the request and move on to the next one.",
        ];
        for (i, l) in lines.iter().enumerate() {
            let Some(s) = say("Samantha", l, &format!("e{i}.wav")) else {
                return; // no `say`
            };
            assert_eq!(g.enroll_clip(&s).unwrap(), i + 1);
        }
        let summary = g.enroll_calibrate().unwrap();
        assert!(summary.contains("5 clips"), "{summary}");
        g.enroll_save().unwrap();
        assert!(SpeakerProfile::load().is_some(), "saved in the test home");
        let text = "Tell the assistant to run the tests again.";
        let me = say("Samantha", text, "me.wav").unwrap();
        let c = g.check(&me, false, true);
        assert!(c.applies && !c.reject(), "{c:?}");
        let other = say("Daniel", text, "daniel.wav").unwrap();
        let c = g.check(&other, false, true);
        assert!(c.reject(), "{c:?}");
        assert!(c
            .reason()
            .unwrap()
            .starts_with("speaker mismatch (score 0."));
        // Wake word mode with open_mic_only: scored, not enforced.
        assert!(!g.check(&other, false, false).reject());
        // Push to talk: never dropped.
        assert!(!g.check(&other, true, true).reject());
        // The user, far away (20 dB down, muffled): the near field gate.
        let far: Vec<i16> = {
            let mut y = 0f32;
            me.iter()
                .map(|s| {
                    y += 0.15 * (*s as f32 * 0.1 - y);
                    y as i16
                })
                .collect()
        };
        let c = g.check(&far, false, true);
        assert!(c.far.is_some(), "{c:?}");
        g.reset().unwrap();
        assert!(SpeakerProfile::load().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

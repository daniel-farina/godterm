//! `godterm voice ...`: install the speaker model, show the enrolled
//! profile, and evaluate speaker verification offline on WAV folders.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::speaker::{self, ClipEmbs, SpeakerModel, SpeakerProfile};
use super::speaker_fbank::Fbank;
use super::speaker_gate::{Acoustic, NearField};

/// macOS voices synthesized as "other people" when calibrating an
/// enrollment (classic voices present on every Mac).
pub const NEGATIVE_VOICES: &[&str] = &["Fred", "Junior", "Kathy", "Ralph", "Albert"];
const NEGATIVE_LINES: &[&str] = &[
    "Can you send me the report later?",
    "Let's go over the plan tomorrow morning.",
];

/// Embeddings of synthesized other voices (macOS `say`; none elsewhere).
/// Writes only temporary files, plays nothing.
pub fn synth_negatives(model: &mut SpeakerModel) -> Vec<ClipEmbs> {
    if !cfg!(target_os = "macos") {
        return vec![];
    }
    let dir = std::env::temp_dir().join(format!("godterm-neg-{}", std::process::id()));
    if std::fs::create_dir_all(&dir).is_err() {
        return vec![];
    }
    let mut out = vec![];
    for (i, v) in NEGATIVE_VOICES.iter().enumerate() {
        let p = dir.join(format!("{i}.wav"));
        let ok = std::process::Command::new("say")
            .args(["-v", v, "--data-format=LEI16@16000", "-o"])
            .arg(&p)
            .arg(NEGATIVE_LINES[i % NEGATIVE_LINES.len()])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if let Some(e) = ok
            .then(|| speaker::read_wav16(&p).ok())
            .flatten()
            .and_then(|s| model.embed(&s).ok())
        {
            out.push(e);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

pub fn command(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("install-speaker") => {
            let p = speaker::install(args.get(1).map(String::as_str))?;
            let m = speaker::model_info(&p);
            println!("speaker model {}", p.display());
            if let Some(m) = m {
                println!("source       {}\nlicense      {}\nsha256       {}", m.source, m.license, m.sha256);
            }
            let t = Instant::now();
            let model = SpeakerModel::load(&p, speaker::auto_threads())?;
            println!(
                "loads        in {} ms, {}-dim embeddings{}",
                t.elapsed().as_millis(),
                model.dim,
                if model.batch_ok() { ", batched windows" } else { "" }
            );
            println!("next         Settings > Voice and Audio > Train my voice");
            Ok(())
        }
        Some("speaker") => {
            let p = speaker::model_path(&crate::config::Config::load_or_init()?.voice.speaker_model);
            println!(
                "model        {} ({})",
                p.display(),
                if p.is_file() { "installed" } else { "missing: godterm voice install-speaker" }
            );
            match SpeakerProfile::load() {
                Some(pr) => println!("profile      {} (trained {})", pr.summary(), pr.trained_at),
                None => println!("profile      none: Settings > Voice and Audio > Train my voice"),
            }
            Ok(())
        }
        Some("speaker-eval") => {
            let dir = PathBuf::from(flag(args, "--dir").context("--dir <corpus> is required")?);
            let model = speaker::model_path(&flag(args, "--model").unwrap_or_else(|| "auto".into()));
            let threads = flag(args, "--threads")
                .and_then(|t| t.parse().ok())
                .unwrap_or_else(speaker::auto_threads);
            eval(&dir, &model, threads)
        }
        Some("denoise") => {
            let input = PathBuf::from(flag(args, "--file").context("--file <16 kHz wav> is required")?);
            let samples = speaker::read_wav16(&input)?;
            let mut d = super::denoise::Denoiser::new();
            let t = Instant::now();
            let mut out = Vec::with_capacity(samples.len());
            for f in samples.chunks(super::audio::FRAME) {
                let mut f = f.to_vec();
                d.process(&mut f);
                out.extend(f);
            }
            let cpu = t.elapsed().as_secs_f32();
            let audio = samples.len() as f32 / super::audio::RATE as f32;
            println!(
                "denoise      {audio:.1} s of audio in {:.1} ms: {:.2}% of one core",
                cpu * 1000.0,
                cpu / audio * 100.0
            );
            if let Some(o) = flag(args, "--out") {
                std::fs::write(&o, super::audio::wav_bytes(&out))?;
                println!("wrote        {o}");
            }
            Ok(())
        }
        _ => bail!(
            "usage: godterm voice install-speaker [wespeaker-resnet34] | speaker | speaker-eval --dir D [--model M] [--threads N] | denoise --file F [--out O]"
        ),
    }
}

fn wavs(dir: &Path) -> Vec<(String, Vec<i16>)> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.retain(|p| p.extension().is_some_and(|e| e == "wav"));
    v.sort();
    v.into_iter()
        .filter_map(|p| {
            let n = p.file_stem()?.to_string_lossy().into_owned();
            Some((n, speaker::read_wav16(&p).ok()?))
        })
        .collect()
}

/// Mix `b` into `a` at `db` (relative gain), starting `offset` samples in.
pub fn mix(a: &[i16], b: &[i16], db: f32, offset: usize) -> Vec<i16> {
    let g = super::audio::gain_factor(db);
    let n = a.len().max(offset + b.len());
    (0..n)
        .map(|i| {
            let x = a.get(i).copied().unwrap_or(0) as f32;
            let y = i
                .checked_sub(offset)
                .and_then(|j| b.get(j))
                .copied()
                .unwrap_or(0) as f32;
            (x + y * g).clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

fn rate(ok: usize, n: usize) -> String {
    if n == 0 {
        return "n/a".into();
    }
    format!("{ok}/{n} ({:.0}%)", ok as f32 * 100.0 / n as f32)
}

/// The offline evaluation: enroll on DIR/enroll, then score DIR/user (the
/// same voice), DIR/other (other voices), DIR/distant (other voices far
/// away), and the user mixed with other voices at -10, -15 and -20 dB.
fn eval(dir: &Path, model_path: &Path, threads: usize) -> Result<()> {
    let t = Instant::now();
    let mut m = SpeakerModel::load(model_path, threads)?;
    println!(
        "model        {} ({} ms to load, {} threads, dim {})",
        m.file_name(),
        t.elapsed().as_millis(),
        threads,
        m.dim
    );
    let fb = Fbank::new();
    let enroll = wavs(&dir.join("enroll"));
    if enroll.len() < 2 {
        bail!("{} needs at least 2 WAVs", dir.join("enroll").display());
    }
    let mut clips = vec![];
    let mut acoustics = vec![];
    for (_, s) in &enroll {
        clips.push(m.embed(s)?);
        acoustics.push(Acoustic::of(s, &fb));
    }
    let t = Instant::now();
    let negs = synth_negatives(&mut m);
    println!(
        "negatives    {} synthesized voices in {} ms",
        negs.len(),
        t.elapsed().as_millis()
    );
    let prof = SpeakerProfile::build(&m.file_name(), clips, acoustics.clone(), &negs)?;
    let thr = prof.threshold;
    println!("profile      {}", prof.summary());
    let gate = NearField::new(acoustics);
    let mut score = |s: &[i16]| -> Result<(f32, u32)> {
        let v = speaker::verify(&mut m, &prof, s, thr)?;
        Ok((v.score, v.ms))
    };
    let held_out = |n: &str| !NEGATIVE_VOICES.iter().any(|v| n.starts_with(v));
    let user = wavs(&dir.join("user"));
    let other = wavs(&dir.join("other"));
    let distant = wavs(&dir.join("distant"));
    let mut lat = vec![];
    let mut ok = 0;
    let mut user_scores = vec![];
    for (_, s) in &user {
        let (sc, ms) = score(s)?;
        lat.push((s.len(), ms));
        user_scores.push(sc);
        ok += usize::from(sc >= thr && gate.check(Acoustic::of(s, &fb), 12.0).is_none());
    }
    println!(
        "user         accepted {}   scores {:.2} to {:.2}",
        rate(ok, user.len()),
        user_scores.iter().cloned().fold(1.0, f32::min),
        user_scores.iter().cloned().fold(-1.0, f32::max)
    );
    for (label, set) in [
        (
            "other",
            other
                .iter()
                .filter(|(n, _)| held_out(n))
                .collect::<Vec<_>>(),
        ),
        (
            "other (calibration voices)",
            other.iter().filter(|(n, _)| !held_out(n)).collect(),
        ),
    ] {
        let mut rej = 0;
        let mut worst = (-1.0f32, String::new());
        for (n, s) in &set {
            let (sc, _) = score(s)?;
            rej += usize::from(sc < thr);
            if sc > worst.0 {
                worst = (sc, n.clone());
            }
        }
        println!(
            "{label:<12} rejected {}   highest {:.2} ({})",
            rate(rej, set.len()),
            worst.0,
            worst.1
        );
    }
    let (mut by_spk, mut by_near, mut either) = (0, 0, 0);
    for (_, s) in &distant {
        let (sc, _) = score(s)?;
        let near = gate.check(Acoustic::of(s, &fb), 12.0).is_some();
        by_spk += usize::from(sc < thr);
        by_near += usize::from(near);
        either += usize::from(sc < thr || near);
    }
    println!(
        "distant      rejected {} (speaker {}, near field {})",
        rate(either, distant.len()),
        rate(by_spk, distant.len()),
        rate(by_near, distant.len())
    );
    let bg: Vec<&Vec<i16>> = other
        .iter()
        .filter(|(n, _)| held_out(n))
        .map(|(_, s)| s)
        .collect();
    for db in [-10.0, -15.0, -20.0] {
        let mut ok = 0;
        let mut n = 0;
        let mut lo = 1.0f32;
        for (i, (_, u)) in user.iter().enumerate() {
            let Some(b) = bg.get((i * 7) % bg.len().max(1)) else {
                continue;
            };
            let mixed = mix(u, b, db, 4_000);
            let (sc, _) = score(&mixed)?;
            lo = lo.min(sc);
            ok += usize::from(sc >= thr);
            n += 1;
        }
        println!("mixed {db:>3} dB accepted {}   lowest {lo:.2}", rate(ok, n));
    }
    // Latency on 3 s clips (several runs, after warm up): the user (the
    // full clip matches, done) and another voice (all windows scored).
    let joined: Vec<i16> = user.iter().flat_map(|(_, s)| s.iter().copied()).collect();
    let three = joined[..joined.len().min(48_000)].to_vec();
    let them: Vec<i16> = bg
        .iter()
        .flat_map(|s| s.iter().copied())
        .take(48_000)
        .collect();
    for threads in [1, 2, 4] {
        let mut m = SpeakerModel::load(model_path, threads)?;
        let mut med = |s: &[i16]| -> Result<(u32, u32, usize)> {
            let _ = speaker::verify(&mut m, &prof, s, thr)?;
            let mut runs = vec![];
            let mut w = 0;
            for _ in 0..10 {
                let v = speaker::verify(&mut m, &prof, s, thr)?;
                runs.push(v.ms);
                w = v.windows;
            }
            runs.sort();
            Ok((runs[runs.len() / 2], runs[runs.len() - 1], w))
        };
        let (a, amax, aw) = med(&three)?;
        let (r, rmax, rw) = med(&them)?;
        println!(
            "latency      {threads} threads, 3 s clip: you {a} ms (max {amax}, {aw} windows), someone else {r} ms (max {rmax}, {rw} windows)"
        );
    }
    let t = Instant::now();
    for _ in 0..10 {
        let _ = fb.compute(&three.iter().map(|s| *s as f32).collect::<Vec<_>>());
    }
    println!(
        "fbank        3 s clip: {:.1} ms",
        t.elapsed().as_secs_f32() * 100.0
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_offsets_and_scales() {
        let a = vec![1000i16; 4];
        let b = vec![1000i16; 4];
        let m = mix(&a, &b, -20.0, 2);
        assert_eq!(m.len(), 6);
        assert_eq!(m[0], 1000);
        assert_eq!(m[2], 1100);
        assert_eq!(m[5], 100);
    }
}

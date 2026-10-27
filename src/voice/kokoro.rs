use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};

pub const SAMPLE_RATE: u32 = 24_000;
pub const MAX_PHONEMES: usize = 510;
const STYLE_DIM: usize = 256;

const MARKS: &str = ";:,.!?¡¿—…\"«»“”(){}[]";

pub fn vocab() -> HashMap<char, i64> {
    let v: HashMap<String, i64> =
        serde_json::from_str(include_str!("kokoro_vocab.json")).unwrap_or_default();
    v.into_iter()
        .filter_map(|(k, id)| {
            let mut c = k.chars();
            let ch = c.next()?;
            c.next().is_none().then_some((ch, id))
        })
        .collect()
}

pub fn apply_fixes(text: &str, fixes: &[(String, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        match fixes.iter().find(|(w, _)| w.eq_ignore_ascii_case(word)) {
            Some((_, say)) => out.push_str(say),
            None => out.push_str(word),
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' || c == '\'' {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

pub fn spell_decimals(text: &str) -> String {
    let c: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    for (i, &ch) in c.iter().enumerate() {
        let digit = |j: Option<usize>| j.and_then(|j| c.get(j)).is_some_and(|d| d.is_ascii_digit());
        if ch == '.' && digit(i.checked_sub(1)) && digit(Some(i + 1)) {
            out.push_str(" point ");
        } else {
            out.push(ch);
        }
    }
    out
}

#[derive(Debug, PartialEq)]
pub enum Piece {
    Words(String),
    Marks(String),
}

pub fn split_marks(text: &str) -> Vec<Piece> {
    let c: Vec<char> = text.chars().collect();
    let is_mark = |i: usize| -> bool {
        let ch = c[i];
        if !MARKS.contains(ch) {
            return false;
        }
        if ch == ',' || ch == '.' {
            let before = i > 0 && c[i - 1].is_ascii_digit();
            let after = c.get(i + 1).is_some_and(|d| d.is_ascii_digit());
            return !(before && after);
        }
        true
    };
    let mut out = Vec::new();
    let mut words = String::new();
    let mut i = 0;
    while i < c.len() {
        if is_mark(i) {
            let trimmed = words.trim_end_matches(char::is_whitespace).len();
            let mut run: String = words[trimmed..].to_string();
            words.truncate(trimmed);
            if !words.is_empty() {
                out.push(Piece::Words(std::mem::take(&mut words)));
            }

            loop {
                while i < c.len() && is_mark(i) {
                    run.push(c[i]);
                    i += 1;
                }
                let mut j = i;
                while j < c.len() && c[j].is_whitespace() {
                    j += 1;
                }
                let spaces: String = c[i..j].iter().collect();
                run.push_str(&spaces);
                i = j;
                if !(i < c.len() && is_mark(i)) {
                    break;
                }
            }
            out.push(Piece::Marks(run));
        } else {
            words.push(c[i]);
            i += 1;
        }
    }
    if !words.is_empty() {
        out.push(Piece::Words(words));
    }
    out
}

pub fn clean_espeak(raw: &str) -> String {
    let line = raw.trim().replace('\n', " ").replace("  ", " ");

    let mut out = String::with_capacity(line.len());
    let mut skip = false;
    let chars: Vec<char> = line.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        if ch == '(' {
            let close = chars[i..].iter().position(|&c| c == ')');
            if let Some(n) = close {
                if n > 1
                    && n <= 7
                    && chars[i + 1..i + n]
                        .iter()
                        .all(|c| c.is_ascii_lowercase() || *c == '-')
                {
                    skip = true;
                }
            }
        }
        if !skip {
            out.push(ch);
        }
        if ch == ')' {
            skip = false;
        }
    }
    out.trim().to_string()
}

pub struct Espeak {
    pub bin: String,
    pub voice: String,
}

impl Espeak {
    fn piece(&self, text: &str) -> Result<String> {
        let mut child = Command::new(&self.bin)
            .args(["-q", "--ipa", "-v", &self.voice])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("cannot run {}", self.bin))?;
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
            stdin.write_all(text.as_bytes())?;
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("espeak-ng failed: {}", out.status);
        }
        Ok(clean_espeak(&String::from_utf8_lossy(&out.stdout)))
    }

    pub fn phonemize(&self, text: &str, vocab: &HashMap<char, i64>) -> Result<String> {
        let mut out = String::new();
        for p in split_marks(text.trim()) {
            match p {
                Piece::Marks(m) => out.push_str(&m),
                Piece::Words(w) if w.trim().is_empty() => out.push_str(&w),
                Piece::Words(w) => out.push_str(&self.piece(w.trim())?),
            }
        }
        let kept: String = out.chars().filter(|c| vocab.contains_key(c)).collect();
        Ok(kept.split_whitespace().collect::<Vec<_>>().join(" "))
    }
}

pub fn tokenize(phonemes: &str, vocab: &HashMap<char, i64>) -> Vec<i64> {
    phonemes
        .chars()
        .filter_map(|c| vocab.get(&c).copied())
        .take(MAX_PHONEMES)
        .collect()
}

pub struct Voices {
    map: HashMap<String, (usize, Vec<f32>)>,
}

impl Voices {
    pub fn load(path: &Path) -> Result<Voices> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&data)
    }

    pub fn parse(data: &[u8]) -> Result<Voices> {
        let mut map = HashMap::new();
        let mut i = 0;
        let u16at = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]) as usize;
        let u32at = |o: usize| {
            u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]) as usize
        };
        while i + 46 <= data.len() {
            if &data[i..i + 4] != b"PK\x01\x02" {
                i += 1;
                continue;
            }
            let method = u16at(i + 10);
            let size = u32at(i + 20);
            let n = u16at(i + 28);
            let extra = u16at(i + 30);
            let comment = u16at(i + 32);
            let local = u32at(i + 42);

            let Some(name) = data.get(i + 46..i + 46 + n) else {
                bail!("truncated zip directory");
            };
            let name = String::from_utf8_lossy(name).into_owned();
            i += 46 + n + extra + comment;
            let Some(voice) = name.strip_suffix(".npy") else {
                continue;
            };
            if method != 0 {
                bail!("{name} is compressed; expected the stored npz from kokoro-onnx");
            }
            if local + 30 > data.len() {
                bail!("bad zip offset for {name}");
            }
            let start = local + 30 + u16at(local + 26) + u16at(local + 28);
            let npy = data
                .get(start..start + size)
                .ok_or_else(|| anyhow!("truncated {name}"))?;
            let (rows, values) = parse_npy(npy).with_context(|| name.clone())?;
            map.insert(voice.to_string(), (rows, values));
        }
        if map.is_empty() {
            bail!("no voices found");
        }
        Ok(Voices { map })
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.map.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn style(&self, voice: &str, n: usize) -> Option<&[f32]> {
        let (rows, v) = self.map.get(voice)?;
        let row = n.clamp(1, *rows) - 1;
        v.get(row * STYLE_DIM..(row + 1) * STYLE_DIM)
    }
}

pub fn parse_npy(b: &[u8]) -> Result<(usize, Vec<f32>)> {
    if b.len() < 10 || &b[..6] != b"\x93NUMPY" {
        bail!("not an npy file");
    }
    let (hlen, off) = if b[6] == 1 {
        (u16::from_le_bytes([b[8], b[9]]) as usize, 10)
    } else if b.len() < 12 {
        bail!("short npy header");
    } else {
        (u32::from_le_bytes([b[8], b[9], b[10], b[11]]) as usize, 12)
    };
    let header = String::from_utf8_lossy(
        b.get(off..off + hlen)
            .ok_or_else(|| anyhow!("short header"))?,
    );
    if !header.contains("'<f4'") || header.contains("'fortran_order': True") {
        bail!("unexpected npy header {header}");
    }
    let body = &b[off + hlen..];
    let values: Vec<f32> = body
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    if !values.len().is_multiple_of(STYLE_DIM) || values.is_empty() {
        bail!("npy size {} is not a multiple of {STYLE_DIM}", values.len());
    }
    Ok((values.len() / STYLE_DIM, values))
}

pub fn trim(y: &[f32]) -> &[f32] {
    const FRAME: usize = 2048;
    const HOP: usize = 512;
    if y.is_empty() {
        return y;
    }

    let frames = 1 + y.len() / HOP;
    let pad = FRAME / 2;
    let mut rms = Vec::with_capacity(frames);
    for t in 0..frames {
        let lo = (t * HOP).saturating_sub(pad);
        let hi = (t * HOP + FRAME).saturating_sub(pad).min(y.len());
        let sum: f64 = y[lo.min(hi)..hi]
            .iter()
            .map(|v| (*v as f64) * (*v as f64))
            .sum();
        rms.push((sum / FRAME as f64).sqrt());
    }
    let max = rms.iter().cloned().fold(0.0f64, f64::max);

    let amin = 1e-5f64;
    let db = |r: f64| 20.0 * (r.max(amin) / max.max(amin)).log10();
    let loud: Vec<usize> = (0..frames).filter(|&t| db(rms[t]) > -60.0).collect();
    match (loud.first(), loud.last()) {
        (Some(&a), Some(&b)) => {
            let start = a * HOP;
            let end = ((b + 1) * HOP).min(y.len());
            &y[start.min(end)..end]
        }
        _ => &y[..0],
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Provider {
    Cpu,
    CoreMl,
}

pub struct Kokoro {
    session: ort::session::Session,
    pub voices: Voices,
    vocab: HashMap<char, i64>,
    pub espeak: Espeak,
    pub provider: Provider,
    pub threads: usize,
    pub load_ms: u128,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Timing {
    pub phonemize_ms: u128,
    pub infer_ms: u128,
    pub audio_s: f32,
}

impl Timing {
    pub fn rtf(&self) -> f32 {
        if self.audio_s <= 0.0 {
            return 0.0;
        }
        (self.phonemize_ms + self.infer_ms) as f32 / 1000.0 / self.audio_s
    }
}

pub fn auto_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(1, 8))
        .unwrap_or(4)
}

impl Kokoro {
    pub fn load(
        model: &Path,
        voices: &Path,
        espeak_bin: &str,
        provider: Provider,
        threads: usize,
    ) -> Result<Kokoro> {
        use ort::session::builder::GraphOptimizationLevel;
        use ort::session::Session;
        let t = Instant::now();
        let voices = Voices::load(voices)?;
        let threads = if threads == 0 {
            auto_threads()
        } else {
            threads
        };
        let mut b = Session::builder()
            .map_err(|e| anyhow!("onnxruntime: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!("{e}"))?
            .with_intra_threads(threads)
            .map_err(|e| anyhow!("{e}"))?;

        {
            use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
            b = b
                .with_memory_pattern(false)
                .map_err(|e| anyhow!("{e}"))?
                .with_allocator(
                    MemoryInfo::new(
                        AllocationDevice::CPU,
                        0,
                        AllocatorType::Device,
                        MemoryType::Default,
                    )
                    .map_err(|e| anyhow!("{e}"))?,
                )
                .map_err(|e| anyhow!("{e}"))?;
        }
        if provider == Provider::CoreMl {
            let cache = crate::config::app_home().join("voice").join("coreml-cache");
            let _ = std::fs::create_dir_all(&cache);
            let ep = ort::execution_providers::CoreMLExecutionProvider::default()
                .with_model_cache_dir(cache.to_string_lossy())
                .build()
                .error_on_failure();
            b = b
                .with_execution_providers([ep])
                .map_err(|e| anyhow!("CoreML: {e}"))?;
        }
        let session = b
            .commit_from_file(model)
            .map_err(|e| anyhow!("loading {}: {e}", model.display()))?;
        Ok(Kokoro {
            session,
            voices,
            vocab: vocab(),
            espeak: Espeak {
                bin: espeak_bin.to_string(),
                voice: "en-us".into(),
            },
            provider,
            threads,
            load_ms: t.elapsed().as_millis(),
        })
    }

    pub fn phonemes(&self, text: &str) -> Result<String> {
        self.espeak.phonemize(&spell_decimals(text), &self.vocab)
    }

    pub fn synth(&mut self, text: &str, voice: &str, speed: f32) -> Result<(Vec<f32>, Timing)> {
        let t = Instant::now();
        let ph = self.phonemes(text)?;
        let tokens = tokenize(&ph, &self.vocab);
        let phonemize_ms = t.elapsed().as_millis();
        if tokens.is_empty() {
            bail!("no phonemes for {text:?}");
        }
        let style = self
            .voices
            .style(voice, tokens.len())
            .ok_or_else(|| anyhow!("no voice {voice}"))?
            .to_vec();
        let mut ids = Vec::with_capacity(tokens.len() + 2);
        ids.push(0i64);
        ids.extend(&tokens);
        ids.push(0);
        let t = Instant::now();
        let n = ids.len();
        let inputs = ort::inputs![
            "tokens" => ort::value::Tensor::from_array(([1usize, n], ids)).map_err(|e| anyhow!("{e}"))?,
            "style" => ort::value::Tensor::from_array(([1usize, STYLE_DIM], style)).map_err(|e| anyhow!("{e}"))?,
            "speed" => ort::value::Tensor::from_array(([1usize], vec![speed.clamp(0.5, 2.0)])).map_err(|e| anyhow!("{e}"))?,
        ];
        let out = self
            .session
            .run(inputs)
            .map_err(|e| anyhow!("inference: {e}"))?;
        let audio = out[0]
            .try_extract_array::<f32>()
            .map_err(|e| anyhow!("{e}"))?;
        let samples: Vec<f32> = audio.iter().copied().collect();
        let infer_ms = t.elapsed().as_millis();
        let trimmed = trim(&samples).to_vec();
        let audio_s = trimmed.len() as f32 / SAMPLE_RATE as f32;
        Ok((
            trimmed,
            Timing {
                phonemize_ms,
                infer_ms,
                audio_s,
            },
        ))
    }
}

pub fn wav(samples: &[f32]) -> Vec<u8> {
    let pcm: Vec<i16> = samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect();
    let data_len = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    v.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_split_like_phonemizer() {
        use Piece::*;
        assert_eq!(
            split_marks("Hello, world."),
            vec![
                Words("Hello".into()),
                Marks(", ".into()),
                Words("world".into()),
                Marks(".".into())
            ]
        );
        assert_eq!(
            split_marks("\"Quoted\" text (with parens) ok"),
            vec![
                Marks("\"".into()),
                Words("Quoted".into()),
                Marks("\" ".into()),
                Words("text".into()),
                Marks(" (".into()),
                Words("with parens".into()),
                Marks(") ".into()),
                Words("ok".into()),
            ]
        );

        assert_eq!(
            split_marks("1,200 or 3.5."),
            vec![Words("1,200 or 3.5".into()), Marks(".".into())]
        );
        assert_eq!(
            split_marks("a . , b"),
            vec![Words("a".into()), Marks(" . , ".into()), Words("b".into())]
        );
    }

    #[test]
    fn espeak_cleanup() {
        assert_eq!(clean_espeak(" həlˈoʊ\n wˈɜːld\n"), "həlˈoʊ wˈɜːld");
        assert_eq!(clean_espeak("(fr)bɔ̃ʒˈuʁ(en) tˈuː"), "bɔ̃ʒˈuʁ tˈuː");
    }

    #[test]
    fn fixes_and_decimals() {
        let fixes = vec![
            ("godterm".to_string(), "clawed go".to_string()),
            ("CLI".to_string(), "C L I".to_string()),
        ];
        assert_eq!(
            apply_fixes("GodTerm uses the cli, not clis.", &fixes),
            "clawed go uses the C L I, not clis."
        );
        assert_eq!(
            spell_decimals("v3.5 and 1,200. Done."),
            "v3 point 5 and 1,200. Done."
        );
    }

    #[test]
    fn tokens_from_vocab() {
        let v = vocab();
        assert_eq!(v.len(), 114);
        assert_eq!(v[&' '], 16);
        assert_eq!(v[&'ˈ'], 156);
        assert_eq!(tokenize("hə, x", &v), vec![50, 83, 3, 16, 66]);
        assert_eq!(tokenize(&"a".repeat(600), &v).len(), MAX_PHONEMES);
    }

    #[test]
    fn npy_and_npz() {
        let mut npy = b"\x93NUMPY\x01\x00".to_vec();
        let header = "{'descr': '<f4', 'fortran_order': False, 'shape': (2, 1, 256), }";
        let mut h = header.to_string();
        while (10 + h.len() + 1) % 64 != 0 {
            h.push(' ');
        }
        h.push('\n');
        npy.extend((h.len() as u16).to_le_bytes());
        npy.extend(h.as_bytes());
        for i in 0..512 {
            npy.extend((i as f32).to_le_bytes());
        }
        let (rows, v) = parse_npy(&npy).unwrap();
        assert_eq!((rows, v.len(), v[300]), (2, 512, 300.0));

        let name = b"af_test.npy";
        let mut z = b"PK\x03\x04".to_vec();
        z.extend([0u8; 22]);
        z.extend((name.len() as u16).to_le_bytes());
        z.extend(0u16.to_le_bytes());
        z.extend(name);
        z.extend(&npy);
        let mut cd = b"PK\x01\x02".to_vec();
        cd.extend([0u8; 16]);
        cd.extend((npy.len() as u32).to_le_bytes());
        cd.extend((npy.len() as u32).to_le_bytes());
        cd.extend((name.len() as u16).to_le_bytes());
        cd.extend([0u8; 12]);
        cd.extend(0u32.to_le_bytes());
        cd.extend(name);
        z.extend(cd);
        let voices = Voices::parse(&z).unwrap();
        assert_eq!(voices.names(), vec!["af_test"]);

        assert_eq!(voices.style("af_test", 1).unwrap()[0], 0.0);
        assert_eq!(voices.style("af_test", 2).unwrap()[0], 256.0);
        assert_eq!(voices.style("af_test", 999).unwrap()[0], 256.0);
        assert!(voices.style("nope", 1).is_none());
    }

    #[test]
    fn trims_silence() {
        let mut y = vec![0.0f32; 24_000];
        for (i, v) in y.iter_mut().enumerate().skip(8_000).take(4_000) {
            *v = (i as f32 * 0.05).sin() * 0.5;
        }
        let t = trim(&y);
        assert!(
            t.len() <= 4_000 + 2 * 2048 && t.len() >= 4_000,
            "{}",
            t.len()
        );
        assert!(trim(&[0.0; 100]).len() <= 100);
        assert!(trim(&[]).is_empty());
    }

    #[test]
    fn real_voices_file() {
        let p = crate::config::expand_tilde("~/.cache/kokoro-onnx/voices-v1.0.bin");
        if !p.exists() {
            return;
        }
        let v = Voices::load(&p).unwrap();
        let names = v.names();
        assert!(names.len() >= 50, "{}", names.len());
        assert!(names.contains(&"af_heart".to_string()));
        assert_eq!(v.style("af_heart", 10).unwrap().len(), STYLE_DIM);
    }
}

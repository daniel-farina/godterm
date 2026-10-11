//! Images in the assistant panel: attaching them to the input (clipboard
//! or pasted paths) as chips, sending them with the turn, and finding the
//! ones a tool call names (send_prompt and open_tab `images`). See
//! `assistant_images`.

use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;

use crate::app::App;
use crate::assistant_images::{self as img, Attachment, ClipImage, Clipboard};

impl App {
    /// The clipboard's image. Tests never read the real clipboard: only
    /// a fake one they set.
    fn clipboard_image(&self) -> anyhow::Result<Option<ClipImage>> {
        match &self.assistant.clipboard {
            Some(c) => c.image(),
            None if cfg!(test) => Ok(None),
            None => img::SystemClipboard.image(),
        }
    }

    /// Ctrl-V (or an empty paste) in the input: the clipboard's image
    /// becomes an attachment. True when one was attached. `quiet`: say
    /// nothing when it holds no image.
    pub fn paste_clipboard_image(&mut self, quiet: bool) -> bool {
        let got = match self.clipboard_image() {
            Ok(Some(ClipImage::Png(b))) => img::save_png(&b),
            Ok(Some(ClipImage::File(p))) => img::import_file(&p),
            Ok(None) => {
                if !quiet {
                    self.flash("No image on the clipboard");
                }
                return false;
            }
            Err(e) => Err(e),
        };
        match got {
            Ok((p, w, h)) => {
                self.attach_image(p, w, h);
                true
            }
            Err(e) => {
                self.flash(format!("Could not paste the image: {e:#}"));
                false
            }
        }
    }

    /// Pasted text that is only image file paths: each file attached.
    /// True when it was (the text is then not pasted).
    pub fn paste_image_paths(&mut self, text: &str) -> bool {
        let Some(paths) = img::pasted_images(text) else {
            return false;
        };
        let mut n = 0;
        for p in paths {
            match img::import_file(&p) {
                Ok((c, w, h)) => {
                    self.attach_image(c, w, h);
                    n += 1;
                }
                Err(e) => self.flash(format!("Could not attach {}: {e:#}", p.display())),
            }
        }
        n > 0
    }

    /// A saved image joins the input as a chip.
    pub fn attach_image(&mut self, path: PathBuf, w: u32, h: u32) {
        if !std::mem::replace(&mut self.assistant.images_pruned, true) {
            let n = img::prune();
            if n > 0 {
                crate::log::info(&format!("assistant: pruned {n} images older than 7 days"));
            }
        }
        self.assistant.image_seq += 1;
        let a = Attachment {
            id: self.assistant.image_seq,
            path,
            w,
            h,
            at: Instant::now(),
        };
        let input = &mut self.assistant.input;
        if !input.is_empty() && !input.ends_with([' ', '\n']) {
            input.push(' ');
        }
        input.push_str(&a.chip());
        input.push(' ');
        self.assistant.hist_pos = None;
        crate::log::info(&format!("assistant: attached {}", a.label()));
        self.assistant.pending_images.push(a);
    }

    /// Backspace at a chip (or the space after it): the whole chip goes,
    /// and its attachment with it. True when one went.
    pub fn backspace_chip(&mut self) -> bool {
        let input = &self.assistant.input;
        let Some(i) = self.assistant.pending_images.iter().position(|a| {
            let c = a.chip();
            input.ends_with(&c) || input.ends_with(&format!("{c} "))
        }) else {
            return false;
        };
        let a = self.assistant.pending_images.remove(i);
        let c = a.chip();
        let input = &mut self.assistant.input;
        if input.ends_with(' ') && !input.ends_with(&c) {
            input.pop();
        }
        input.truncate(input.len() - c.len());
        let _ = std::fs::remove_file(&a.path);
        true
    }

    /// The attachments whose chips are in the text being sent (the input
    /// then holds none).
    pub fn take_input_images(&mut self, text: &str) -> Vec<Attachment> {
        let all = std::mem::take(&mut self.assistant.pending_images);
        all.into_iter()
            .filter(|a| text.contains(&a.chip()))
            .collect()
    }

    /// What the brain gets for this turn's images: a note naming each one
    /// (number, size, file) and the image blocks its model sees.
    pub(crate) fn turn_images(&mut self, imgs: &[Attachment]) -> (String, Vec<img::ModelImage>) {
        if imgs.is_empty() {
            return (String::new(), vec![]);
        }
        let sees = self
            .assistant
            .brain
            .as_ref()
            .is_some_and(|b| b.takes_images());
        let mut blocks = vec![];
        let mut lines = vec![];
        for a in imgs {
            let file = a.path.display();
            if !sees {
                lines.push(format!("image {} ({}x{}): {file}", a.id, a.w, a.h));
                continue;
            }
            match img::for_model(&a.path) {
                Ok(m) => {
                    lines.push(format!(
                        "image {} ({}x{}, shown below): {file}",
                        a.id, a.w, a.h
                    ));
                    blocks.push(m);
                }
                Err(e) => lines.push(format!(
                    "image {} ({}x{}, not shown to you: {e:#}): {file}",
                    a.id, a.w, a.h
                )),
            }
        }
        let how = if sees {
            ""
        } else {
            " You cannot see images in this mode; a tab's agent can, from its file."
        };
        let note = format!(
            "\n(The user attached {}: {}.{how} Pass them to a tab only when the user asks to, with images [numbers] in send_prompt or open_tab; they stay available by number for 30 minutes.)",
            if imgs.len() == 1 { "an image" } else { "images" },
            lines.join("; ")
        );
        // Referable by number from now on.
        self.assistant.images.retain(Attachment::fresh);
        self.assistant.images.extend(imgs.iter().cloned());
        (note, blocks)
    }

    /// The image files a tool call names: attachment numbers ("1",
    /// "image 1", 1) from the last 30 minutes, or image file paths.
    pub fn resolve_images(&self, v: Option<&Value>) -> Result<Vec<PathBuf>, String> {
        let items: Vec<&Value> = match v {
            None | Some(Value::Null) => return Ok(vec![]),
            Some(Value::Array(a)) => a.iter().collect(),
            Some(x) => vec![x],
        };
        let mut out = vec![];
        for it in items {
            let s = match it {
                Value::Number(n) => n.to_string(),
                Value::String(s) => s.trim().to_string(),
                _ => return Err("images: a list of attachment numbers or image paths".into()),
            };
            let num = s
                .trim_start_matches(|c: char| {
                    c.is_alphabetic() || c == ' ' || c == '#' || c == '-' || c == '_'
                })
                .parse::<u32>()
                .ok()
                .filter(|_| !s.starts_with(['/', '~']) && !s.starts_with("file:"));
            let p = if let Some(n) = num {
                let a = self
                    .assistant
                    .images
                    .iter()
                    .rev()
                    .find(|a| a.id == n)
                    .ok_or(format!(
                        "image {n} is not an attachment of this conversation"
                    ))?;
                if !a.fresh() {
                    return Err(format!(
                        "image {n} was attached over 30 minutes ago; ask the user to paste it again"
                    ));
                }
                a.path.clone()
            } else {
                let p = img::paste_paths(&s)
                    .and_then(|v| v.into_iter().next())
                    .or_else(|| Some(crate::config::expand_tilde(&s)))
                    .filter(|p| p.is_absolute())
                    .ok_or(format!(
                        "images: {s} is not an attachment number or an image path"
                    ))?;
                if !img::is_image_path(&p) || !p.is_file() {
                    return Err(format!("images: {} is not an image file", p.display()));
                }
                let ours = crate::guard::canon(&p).starts_with(crate::guard::canon(&img::dir()));
                if !ours {
                    if let Some(why) = crate::guard::denied(&p, &self.workspaces()) {
                        return Err(format!("images: refused: {why}"));
                    }
                }
                p
            };
            if !out.contains(&p) {
                out.push(p);
            }
        }
        Ok(out)
    }
}

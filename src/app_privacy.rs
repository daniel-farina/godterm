//! Account emails: shown in pane headers unless turned off (globally or
//! per account), and hidden everywhere in privacy mode.

use crate::app::App;

/// "jordan.rivera@gmail.com" in at most `max` columns, cutting the middle
/// of the name and keeping the domain: "jor…era@gmail.com".
pub fn middle_truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max < 4 {
        return String::new();
    }
    let (name, domain) = match s.split_once('@') {
        Some((a, b)) if b.chars().count() + 5 <= max => (a, format!("@{b}")),
        _ => (s, String::new()),
    };
    let room = max - domain.chars().count() - 1; // the ellipsis
    let head = room.div_ceil(2);
    let tail = room - head;
    let chars: Vec<char> = name.chars().collect();
    let h: String = chars[..head.min(chars.len())].iter().collect();
    let t: String = chars[chars.len().saturating_sub(tail)..].iter().collect();
    format!("{h}…{t}{domain}")
}

/// Hide an email in text: "a@b.c" -> "(hidden)".
pub fn mask_emails(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|w| {
            let core = w.trim_end();
            let tail = &w[core.len()..];
            let bare = core.trim_matches(|c: char| "()<>,;'\"".contains(c));
            if bare.contains('@') && bare.contains('.') && !bare.starts_with('@') {
                format!("{}{tail}", core.replace(bare, "(email hidden)"))
            } else {
                w.to_string()
            }
        })
        .collect()
}

impl App {
    pub fn privacy(&self) -> bool {
        self.rt_privacy.unwrap_or(self.cfg.privacy)
    }

    pub fn show_email_global(&self) -> bool {
        self.rt_show_email.unwrap_or(self.cfg.show_email)
    }

    /// Whether account `a`'s email may be shown.
    pub fn show_email_for(&self, a: usize) -> bool {
        if self.privacy() {
            return false;
        }
        match self.cfg.accounts.get(a).and_then(|c| c.show_email) {
            Some(v) if self.rt_show_email.is_none() => v,
            _ => self.show_email_global(),
        }
    }

    /// The email to show for account `a`, middle truncated to `max`.
    pub fn email_label(&self, a: usize, max: usize) -> Option<String> {
        if !self.show_email_for(a) {
            return None;
        }
        let e = self.accounts.get(a)?.profile.email.as_ref()?;
        let t = middle_truncate(e, max);
        (!t.is_empty()).then_some(t)
    }

    pub fn toggle_show_email(&mut self) {
        let on = !self.show_email_global();
        self.set_show_email(on);
    }

    pub fn set_show_email(&mut self, on: bool) {
        self.rt_show_email = Some(on);
        self.state_dirty = true;
        self.flash(if self.privacy() {
            "Emails stay hidden: privacy mode is on (Ctrl-a E)".to_string()
        } else if on {
            "Showing account emails (Ctrl-a e hides them)".to_string()
        } else {
            "Account emails hidden (Ctrl-a e shows them)".to_string()
        });
    }

    pub fn toggle_privacy(&mut self) {
        let on = !self.privacy();
        self.set_privacy(on);
    }

    pub fn set_privacy(&mut self, on: bool) {
        self.rt_privacy = Some(on);
        self.state_dirty = true;
        self.flash(if on {
            "Privacy mode on: emails hidden everywhere (Ctrl-a E to turn off)"
        } else {
            "Privacy mode off"
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_the_domain() {
        assert_eq!(middle_truncate("a@b.co", 20), "a@b.co");
        let t = middle_truncate("jordan.rivera@gmail.com", 17);
        assert_eq!(t.chars().count(), 17);
        assert!(t.ends_with("@gmail.com") && t.contains('…'), "{t}");
        // Too narrow for the domain: cut the middle of the whole thing.
        let t = middle_truncate("jordan.rivera@gmail.com", 9);
        assert_eq!(t.chars().count(), 9);
        assert!(t.contains('…'));
        assert_eq!(middle_truncate("abc@d.e", 3), "");
    }

    #[test]
    fn masking() {
        assert_eq!(
            mask_emails("[one] ok (a@b.co)"),
            "[one] ok ((email hidden))"
        );
        assert_eq!(
            mask_emails("email  x.y@z.com\nnext"),
            "email  (email hidden)\nnext"
        );
        assert_eq!(mask_emails("no @ here"), "no @ here");
    }
}

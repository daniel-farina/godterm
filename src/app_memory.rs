//! Memory saver and buffer settings: history per tab (smaller for tabs
//! not on screen), idle background tabs suspended (their claude stopped,
//! resumed with --resume when shown), caches and helpers dropped.

use std::time::{Duration, Instant};

use crate::app::App;
use crate::pane::{Activity, LaunchKind};

/// While the memory saver is on.
pub const SAVER_VISIBLE_LINES: usize = 200;
pub const SAVER_BACKGROUND_LINES: usize = 0;
pub const SAVER_WRITER_QUEUE: usize = 64;
pub const SAVER_READ_BUFFER_KB: usize = 4;
pub const SAVER_TRANSCRIPT_CACHE: usize = 10;
pub const SAVER_VOICE_IDLE_MIN: u32 = 5;
pub const SAVER_TTS_UNLOAD_S: u32 = 60;

/// "10m", "30s", "1h", "90" (seconds); "off" or "0" disables.
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim().to_lowercase();
    if s.is_empty() || s == "off" || s == "0" || s == "never" {
        return None;
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num.parse().ok()?;
    let secs = match unit.trim() {
        "" | "s" | "sec" | "secs" => n,
        "m" | "min" | "mins" => n * 60,
        "h" | "hr" | "hrs" => n * 3600,
        _ => return None,
    };
    (secs > 0).then(|| Duration::from_secs(secs))
}

impl App {
    pub fn memory_saver_on(&self) -> bool {
        self.rt_mem_saver.unwrap_or(self.cfg.memory_saver)
    }

    /// The Mem button, Ctrl-a Z, "save memory".
    pub fn toggle_memory_saver(&mut self) {
        let on = !self.memory_saver_on();
        self.set_memory_saver(on);
    }

    pub fn set_memory_saver(&mut self, on: bool) {
        self.rt_mem_saver = Some(on);
        self.state_dirty = true;
        self.apply_buffers();
        if on {
            self.drop_caches();
            self.flash(format!(
                "Memory saver on: {SAVER_VISIBLE_LINES} lines of history on screen, none in the background, idle background tabs pause after {} (click Mem again to undo)",
                self.cfg.suspend_idle_after
            ));
        } else {
            self.flash("Memory saver off: normal history; paused tabs resume when you open them");
        }
        self.mem_ticked = Instant::now() - Duration::from_secs(5);
        self.memory_tick();
    }

    /// Writer queue, read buffer, scrollback default and caches.
    pub fn apply_buffers(&mut self) {
        let saver = self.memory_saver_on();
        let (q, r) = if saver {
            (SAVER_WRITER_QUEUE, SAVER_READ_BUFFER_KB)
        } else {
            (self.cfg.writer_queue_kb, self.cfg.pty_read_buffer_kb)
        };
        crate::pane::set_buffers(q, r);
        crate::sessions::set_cache_limit(if saver {
            SAVER_TRANSCRIPT_CACHE
        } else {
            self.cfg.transcript_cache
        });
        let assistant = self.assistant_on();
        if let Some(v) = self.voice.engine.as_mut() {
            let mut vc = self.cfg.voice.clone();
            if saver && !assistant {
                vc.tts_unload_after_s = vc.tts_unload_after_s.clamp(1, SAVER_TTS_UNLOAD_S);
            }
            if assistant {
                vc.tts_unload_after_s = 0; // keep the voice warm for replies
            }
            v.configure_tts(&vc);
        }
    }

    /// Restore mode: the memory saver always restores lazily.
    pub fn restore_mode(&self) -> &str {
        if self.memory_saver_on() {
            "lazy"
        } else {
            &self.cfg.restore
        }
    }

    /// On screen right now (the active tab of a pane on the current page).
    pub fn tab_visible(&self, s: usize, t: usize) -> bool {
        self.view == crate::app::View::Grid
            && self
                .panes
                .get(s)
                .is_some_and(|p| p.active == t && !p.hidden)
            && self.pane_rects.get(s).is_some_and(|r| r.width > 0)
    }

    /// History lines for a tab.
    pub fn history_for(&self, s: usize, t: usize) -> usize {
        let visible = self.tab_visible(s, t) || self.panes[s].active == t;
        if self.memory_saver_on() {
            return if visible {
                SAVER_VISIBLE_LINES
            } else {
                SAVER_BACKGROUND_LINES
            };
        }
        let base = self.panes[s]
            .account
            .and_then(|a| self.cfg.accounts.get(a))
            .and_then(|a| a.scrollback_lines)
            .unwrap_or(self.cfg.scrollback_lines);
        if visible {
            base
        } else {
            base.min(self.cfg.background_scrollback_lines)
        }
    }

    /// A tab the saver may pause: idle, off screen, its conversation
    /// known, no loops, not waiting for you, not being moved.
    pub fn suspendable(&self, s: usize, t: usize, idle_for: Duration) -> bool {
        let tab = &self.panes[s].tabs[t];
        tab.is_running()
            && tab.activity == Activity::Ready
            && tab.activity_since.elapsed() >= idle_for
            && tab.session_id.is_some()
            && !matches!(tab.kind, LaunchKind::Login)
            && !self.tab_visible(s, t)
            && self.loops_in(tab.uid) == 0
            && !self
                .pending_moves
                .iter()
                .any(|m| m.source_uid == tab.uid || m.target_uid == tab.uid)
            && !self.queued_moves.iter().any(|m| m.uid == tab.uid)
            && !self.stop_reqs.iter().any(|r| r.uid == tab.uid)
    }

    /// Stop a tab's claude; it resumes its conversation when shown.
    pub fn suspend_tab(&mut self, s: usize, t: usize) {
        let tab = &mut self.panes[s].tabs[t];
        let Some(id) = tab.session_id.clone() else {
            return;
        };
        crate::log::info(&format!(
            "memory saver: pausing pane {} tab {} ({})",
            s + 1,
            t + 1,
            tab.name()
        ));
        tab.kill();
        tab.state = crate::pane::PaneState::Idle;
        tab.activity = Activity::Idle;
        tab.pending = Some(LaunchKind::Resume(id));
        tab.suspended = true;
        tab.set_history(0);
    }

    /// About once a second: history caps, idle tabs, voice helpers.
    pub fn memory_tick(&mut self) {
        if self.mem_ticked.elapsed() < Duration::from_millis(900) {
            return;
        }
        self.mem_ticked = Instant::now();
        for s in 0..self.panes.len() {
            for t in 0..self.panes[s].tabs.len() {
                let lines = self.history_for(s, t);
                self.panes[s].tabs[t].set_history(lines);
            }
        }
        if self.memory_saver_on() {
            if let Some(after) = crate::app_memory::parse_duration(&self.cfg.suspend_idle_after) {
                let mut paused = 0;
                for s in 0..self.panes.len() {
                    for t in 0..self.panes[s].tabs.len() {
                        if self.suspendable(s, t, after) {
                            self.suspend_tab(s, t);
                            paused += 1;
                        }
                    }
                }
                if paused > 0 {
                    self.flash(format!(
                        "Memory saver paused {paused} idle tab(s); they resume when you open them"
                    ));
                }
            }
        }
        // Voice helpers stop after a while without use (push to talk only).
        let idle_min = if self.memory_saver_on() {
            SAVER_VOICE_IDLE_MIN
        } else {
            self.cfg.voice_idle_stop_min
        };
        if idle_min > 0
            && self.voice.engine.is_some()
            && !self.voice.always_on
            && self.voice.updated.elapsed() > Duration::from_secs(idle_min as u64 * 60)
        {
            crate::log::info("memory saver: stopping idle voice helpers");
            self.stop_voice();
            self.voice.info = Some("voice stopped while idle; Ctrl-a space starts it again".into());
        }
    }

    /// Free now: pause every idle background tab regardless of how long it
    /// has been idle, drop caches, trim history.
    pub fn free_now(&mut self) {
        let mut paused = 0;
        for s in 0..self.panes.len() {
            for t in 0..self.panes[s].tabs.len() {
                if self.suspendable(s, t, Duration::ZERO) {
                    self.suspend_tab(s, t);
                    paused += 1;
                }
            }
        }
        self.drop_caches();
        for s in 0..self.panes.len() {
            for t in 0..self.panes[s].tabs.len() {
                if !self.tab_visible(s, t) {
                    self.panes[s].tabs[t].set_history(0);
                }
            }
        }
        self.flash(format!(
            "Freed: paused {paused} idle background tab(s), dropped caches and background history"
        ));
        self.mem_ticked = Instant::now();
        self.refresh_mem();
    }

    /// Parsed transcript caches and loop scanners of tabs not running.
    pub fn drop_caches(&mut self) {
        for st in &mut self.accounts {
            *st.cache.lock().unwrap_or_else(|e| e.into_inner()) = Default::default();
            st.sessions.shrink_to_fit();
        }
        self.main_cache = Default::default();
        self.main_sessions.clear();
        let live: Vec<std::path::PathBuf> = (0..self.panes.len())
            .flat_map(|s| (0..self.panes[s].tabs.len()).map(move |t| (s, t)))
            .filter(|&(s, t)| self.panes[s].tabs[t].is_running())
            .filter_map(|(s, t)| self.transcript_path(s, t))
            .collect();
        self.loop_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .scanners
            .retain(|p, _| live.contains(p));
        for st in &mut self.accounts {
            let cap = self.cfg.usage_history;
            while st.usage_hist.len() > cap {
                st.usage_hist.pop_front();
            }
        }
    }

    pub fn suspended_count(&self) -> usize {
        self.panes
            .iter()
            .flat_map(|s| s.tabs.iter())
            .filter(|t| t.suspended && !t.is_running())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("10m"), Some(Duration::from_secs(600)));
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("2h"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("off"), None);
        assert_eq!(parse_duration("0"), None);
        assert_eq!(parse_duration("ten"), None);
    }
}

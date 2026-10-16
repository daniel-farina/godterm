//! How GodTerm starts: the assistant panel is always open, and the mic is
//! off unless it was on when GodTerm last quit (a restart, a relaunch
//! after an update, a crash: the saved voice mode is kept up to date, so
//! an open mic reopens). `voice.enabled` no longer forces it on: a mic
//! that was off stays off.

use crate::app::App;
use crate::app_panelwin::PanelWin;

impl App {
    /// Open the assistant panel at its normal size, whatever the panel
    /// mode (docked, overlay or auto: the mode only decides where it
    /// sits) and whatever it was when GodTerm last ran. The keys stay with
    /// the grid until the user goes to the panel. Off with the assistant.
    pub fn open_panel_at_start(&mut self) {
        if !self.assistant_on() {
            return;
        }
        self.assistant.show = true;
        self.assistant.win = PanelWin::Normal;
    }

    /// The voice mode to start in ("open", "wake" or "push"), or None: the
    /// mic stays off. It is on only when it was on at the last quit, or
    /// `--voice` asks (then the saved mode, else `voice.mode`).
    pub fn startup_voice(&self, flag: bool) -> Option<String> {
        match self.rt_voice.as_deref() {
            Some(m @ ("open" | "wake" | "push")) => Some(m.to_string()),
            _ if flag => Some(self.cfg.voice.mode.clone()),
            _ => None,
        }
    }
}

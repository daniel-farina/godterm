use crate::app::App;
use crate::app_panelwin::PanelWin;

impl App {
    pub fn open_panel_at_start(&mut self) {
        if !self.assistant_on() {
            return;
        }
        self.assistant.show = true;
        self.assistant.win = PanelWin::Normal;
    }

    pub fn startup_voice(&self, flag: bool) -> Option<String> {
        match self.rt_voice.as_deref() {
            Some(m @ ("open" | "wake" | "push")) => Some(m.to_string()),
            _ if flag => Some(self.cfg.voice.mode.clone()),
            _ => None,
        }
    }
}

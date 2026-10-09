//! Which view the assistant panel shows, and the way back: every view
//! other than the conversation (History, Admin, and the Rules and Prompt
//! screens it opens) gets "‹ Conversation" as the first toolbar button,
//! Esc goes back too, and the button counts replies that came while the
//! conversation was out of sight.

use crate::app::{App, View};
use crate::app_assistant::Who;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelView {
    Conversation,
    History,
    Admin,
    Rules,
    Prompt,
}

impl PanelView {
    pub fn name(self) -> &'static str {
        match self {
            PanelView::Conversation => "Conversation",
            PanelView::History => "History",
            PanelView::Admin => "Admin",
            PanelView::Rules => "Rules",
            PanelView::Prompt => "Prompt",
        }
    }
}

impl App {
    pub fn panel_view(&self) -> PanelView {
        if self.assistant.history.is_some() {
            PanelView::History
        } else if self.assistant.show_admin {
            PanelView::Admin
        } else if self.view == View::Learned {
            PanelView::Rules
        } else if self.view == View::Prompt {
            PanelView::Prompt
        } else {
            PanelView::Conversation
        }
    }

    /// Back to the conversation going on, as it was.
    pub fn back_to_conversation(&mut self) {
        self.assistant.history = None;
        self.assistant.show_admin = false;
        if matches!(self.view, View::Learned | View::Prompt) {
            self.view = View::Grid;
        }
        self.mark_replies_seen();
    }

    /// Every reply of this conversation so far (trimmed ones included).
    pub fn replies_total(&self) -> usize {
        self.assistant.trimmed_replies
            + self
                .assistant
                .log
                .iter()
                .filter(|e| e.who == Who::Reply)
                .count()
    }

    /// Replies that came while the conversation was out of sight.
    pub fn unread_replies(&self) -> usize {
        self.replies_total()
            .saturating_sub(self.assistant.seen_replies.get())
    }

    pub fn mark_replies_seen(&self) {
        self.assistant.seen_replies.set(self.replies_total());
    }

    /// The conversation going on, for History's pinned row: (turns, the
    /// first thing the user said).
    pub fn current_conversation(&self) -> (usize, Option<String>) {
        let users: Vec<&str> = self
            .assistant
            .log
            .iter()
            .filter(|e| e.who == Who::User)
            .map(|e| e.text.as_str())
            .collect();
        (users.len(), users.first().map(|s| s.to_string()))
    }
}

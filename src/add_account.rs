//! The "Add account" modal: one centered dialog for every entry point
//! (Settings > Accounts, the account menu, the empty slot, the palette,
//! Ctrl-a A). Label, agent, color, default folder and permission mode,
//! then the new pane opens focused and starts that agent's login.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::app::{App, Modal};
use crate::hits::{AddUi, UiAction};
use crate::theme::{self, BAR_BG, DIM, FAINT, FG, SEL_BG};

/// Agents in card order (also the numbered keyboard shortcuts).
pub const AGENTS: [(&str, &str, &str); 6] = [
    ("claude", "Claude Code", "Anthropic subscription login"),
    ("grok", "Grok Build", "xAI Grok Build login"),
    ("codex", "Codex CLI", "A login for each account"),
    ("cursor", "Cursor CLI", "Shared login on this machine"),
    (
        "antigravity",
        "Antigravity CLI",
        "Shared login on this machine",
    ),
    ("opencode", "OpenCode", "Shared login on this machine"),
];

/// The form's fields, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Label,
    Agent,
    Color,
    Folder,
    Mode,
}

const FIELDS: [Field; 5] = [
    Field::Label,
    Field::Agent,
    Field::Color,
    Field::Folder,
    Field::Mode,
];

#[derive(Debug, Clone, PartialEq)]
pub struct AddForm {
    pub label: String,
    /// The label is still the suggestion: typing replaces it.
    pub label_pristine: bool,
    /// Index into AGENTS.
    pub agent: usize,
    /// Index into theme::SWATCHES.
    pub color: usize,
    pub folder: String,
    /// Index into config::PERMISSION_MODES.
    pub mode: usize,
    pub focus: Field,
    /// Each agent's CLI is installed (AGENTS order).
    pub ok: [bool; 6],
    /// How to get each missing CLI, in one line (empty when installed).
    pub how: [String; 6],
    /// GodTerm can install that CLI itself (i in the form).
    pub can_install: [bool; 6],
    /// The first run's picker: a welcome title and the step it is.
    pub first: bool,
    /// Recent folders to pick from, newest first ("~" style).
    pub recents: Vec<String>,
    pub error: Option<String>,
}

impl AddForm {
    pub fn new(app: &App) -> AddForm {
        let n = app.cfg.accounts.len();
        let mut recents: Vec<(i64, String)> = vec![];
        for r in &app.recents {
            let p = crate::picker::tilde(&r.path);
            match recents.iter_mut().find(|(_, x)| *x == p) {
                Some(e) => e.0 = e.0.max(r.last_used),
                None => recents.push((r.last_used, p)),
            }
        }
        recents.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        let mut recents: Vec<String> = recents
            .into_iter()
            .map(|(_, p)| p)
            .filter(|p| p != "~")
            .collect();
        recents.truncate(5);
        let mode = crate::config::PERMISSION_MODES
            .iter()
            .position(|m| *m == "bypass")
            .unwrap_or(0);
        let mut label = format!("Account {}", n + 1);
        let mut i = n + 2;
        while app.cfg.accounts.iter().any(|a| a.display() == label) {
            label = format!("Account {i}");
            i += 1;
        }
        let mut f = AddForm {
            label,
            label_pristine: true,
            agent: 0,
            color: n % theme::SWATCHES.len(),
            folder: "~".into(),
            mode,
            focus: Field::Agent,
            ok: [true; 6],
            how: Default::default(),
            can_install: [false; 6],
            first: false,
            recents,
            error: None,
        };
        f.refresh(&app.deps());
        // Start on an agent that is installed (Claude Code when it is).
        if let Some(i) = (0..AGENTS.len()).find(|&i| f.ok[i]) {
            f.agent = i;
        }
        f
    }

    /// Read again which CLIs are installed (after an install, or a key).
    pub fn refresh(&mut self, deps: &[crate::deps::Dep]) {
        for (i, (id, _, _)) in AGENTS.iter().enumerate() {
            match deps.iter().find(|d| d.id == *id) {
                Some(d) if !d.status.ok() => {
                    self.ok[i] = false;
                    self.can_install[i] = d.installable();
                    self.how[i] = crate::first_run::how_to_install(d);
                }
                _ => {
                    self.ok[i] = true;
                    self.can_install[i] = false;
                    self.how[i].clear();
                }
            }
        }
    }

    fn step(&mut self, d: isize) {
        let i = FIELDS.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        self.focus = FIELDS[(i + d).rem_euclid(FIELDS.len() as isize) as usize];
    }

    /// Pick an agent card. A missing one can be picked too: the form then
    /// says how to install it, and Enter waits until it is there.
    pub fn pick_agent(&mut self, i: usize) {
        self.agent = i.min(AGENTS.len() - 1);
        self.error = if self.ok[self.agent] {
            None
        } else {
            Some(self.missing_line())
        };
    }

    /// The selected agent is not installed: what to do, in one line.
    pub fn missing_line(&self) -> String {
        let i = self.agent;
        let name = AGENTS[i].1;
        if self.can_install[i] {
            format!(
                "{name} is not installed. Press i to install it ({}).",
                self.how[i]
            )
        } else if self.how[i].is_empty() {
            format!("{name} is not installed.")
        } else {
            format!("{name} is not installed. {}", self.how[i])
        }
    }

    fn cycle(&mut self, d: isize) {
        let wrap = |v: usize, n: usize| (v as isize + d).rem_euclid(n as isize) as usize;
        match self.focus {
            Field::Agent => self.pick_agent(wrap(self.agent, AGENTS.len())),
            Field::Color => self.color = wrap(self.color, theme::SWATCHES.len()),
            Field::Mode => self.mode = wrap(self.mode, crate::config::PERMISSION_MODES.len()),
            Field::Folder if !self.recents.is_empty() => {
                // Step through the recent folders (and back to ~).
                let mut all = vec!["~".to_string()];
                all.extend(self.recents.iter().cloned());
                let i = all
                    .iter()
                    .position(|p| *p == self.folder)
                    .map(|i| i as isize)
                    .unwrap_or(-1);
                let j = (i + d).rem_euclid(all.len() as isize) as usize;
                self.folder = all[j].clone();
            }
            _ => {}
        }
    }

    /// What Enter would add, or why it cannot.
    pub fn check(&self) -> Result<(), String> {
        let l = self.label.trim();
        if l.is_empty() {
            return Err("Give the account a name".into());
        }
        if l.chars().count() > 40 {
            return Err("Keep the name under 40 characters".into());
        }
        if !crate::config::expand_tilde(self.folder.trim()).is_dir() {
            return Err(format!("{} is not a folder", self.folder.trim()));
        }
        if !self.ok[self.agent] {
            return Err(self.missing_line());
        }
        Ok(())
    }

    /// A key in the form. Returns true when Enter asks to add.
    pub fn key(&mut self, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let text = matches!(self.focus, Field::Label | Field::Folder);
        match k.code {
            KeyCode::Enter => return true,
            KeyCode::Tab | KeyCode::Down => self.step(1),
            KeyCode::BackTab | KeyCode::Up => self.step(-1),
            KeyCode::Left => self.cycle(-1),
            KeyCode::Right => self.cycle(1),
            KeyCode::Backspace if self.focus == Field::Label => {
                if self.label_pristine {
                    self.label.clear();
                    self.label_pristine = false;
                } else {
                    self.label.pop();
                }
            }
            KeyCode::Backspace if self.focus == Field::Folder => {
                self.folder.pop();
            }
            KeyCode::Char(c @ '1'..='6') if !text => {
                self.focus = Field::Agent;
                self.pick_agent((c as u8 - b'1') as usize);
            }
            KeyCode::Char(c) if ctrl && c == 'u' && text => {
                if self.focus == Field::Label {
                    self.label.clear();
                    self.label_pristine = false;
                } else {
                    self.folder.clear();
                }
            }
            KeyCode::Char(c) if !ctrl && self.focus == Field::Label => {
                if self.label_pristine {
                    self.label.clear();
                    self.label_pristine = false;
                }
                if self.label.chars().count() < 40 {
                    self.label.push(c);
                }
            }
            KeyCode::Char(c) if !ctrl && self.focus == Field::Folder => self.folder.push(c),
            _ => {}
        }
        // A missing agent stays explained while it is the one picked.
        self.error =
            (self.focus == Field::Agent && !self.ok[self.agent]).then(|| self.missing_line());
        false
    }

    pub fn paste(&mut self, s: &str) {
        let s = s.trim().replace(['\r', '\n'], " ");
        match self.focus {
            Field::Folder => self.folder.push_str(&s),
            _ => {
                if self.label_pristine {
                    self.label.clear();
                    self.label_pristine = false;
                }
                self.label = format!("{}{s}", self.label).chars().take(40).collect();
                self.focus = Field::Label;
            }
        }
    }
}

impl App {
    pub fn open_add_account(&mut self) {
        self.modal = Modal::AddAccount(Box::new(AddForm::new(self)));
    }

    pub fn on_add_account_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Esc {
            let first = matches!(&self.modal, Modal::AddAccount(f) if f.first);
            self.modal = Modal::None;
            if first {
                self.flash(crate::first_run::SKIPPED_FIRST_ACCOUNT);
            }
            return;
        }
        // What is installed may have changed (an install, a new terminal).
        let deps = self.deps();
        let Modal::AddAccount(form) = &mut self.modal else {
            return;
        };
        form.refresh(&deps);
        let text = matches!(form.focus, Field::Label | Field::Folder);
        if !text
            && matches!(k.code, KeyCode::Char('i' | 'I'))
            && !k.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.add_form_install();
            return;
        }
        if form.key(k) {
            self.submit_add_account();
        }
    }

    /// i in the form: install the picked agent's CLI (the first press says
    /// what runs, the second runs it), or say how to get it.
    pub fn add_form_install(&mut self) {
        let Modal::AddAccount(form) = &self.modal else {
            return;
        };
        let i = form.agent;
        let (id, name, _) = AGENTS[i];
        let msg = if form.ok[i] {
            format!("{name} is installed already. Enter adds the account.")
        } else if !form.can_install[i] {
            form.missing_line()
        } else if self.setup.running.iter().any(|r| r == id) {
            format!("Installing {name}. This takes a minute; the card turns green when done.")
        } else {
            let again =
                self.setup.confirm.as_ref().is_some_and(|(k, t)| {
                    k == id && t.elapsed() < std::time::Duration::from_secs(15)
                });
            if again {
                self.setup.confirm = None;
                self.run_install(&[id.to_string()]);
                format!("Installing {name}. This takes a minute; the card turns green when done.")
            } else {
                match self.install_question(&[id]) {
                    Ok(q) => {
                        self.setup.confirm = Some((id.to_string(), std::time::Instant::now()));
                        format!("{q} Press i again to go ahead.")
                    }
                    Err(e) => e,
                }
            }
        };
        if let Modal::AddAccount(form) = &mut self.modal {
            form.error = Some(msg);
        }
    }

    /// Add the account in the form, or show why not.
    pub fn submit_add_account(&mut self) {
        let Modal::AddAccount(form) = &mut self.modal else {
            return;
        };
        if let Err(e) = form.check() {
            form.error = Some(e);
            return;
        }
        let f = (**form).clone();
        self.modal = Modal::None;
        let n0 = self.cfg.accounts.len();
        let mode = crate::config::PERMISSION_MODES[f.mode];
        let spec = crate::app::NewAccount {
            label: f.label.trim().to_string(),
            harness: AGENTS[f.agent].0.to_string(),
            color: theme::SWATCHES[f.color].to_string(),
            cwd: f.folder.trim().to_string(),
            // Only written when it differs from the global default.
            permission_mode: (mode != self.cfg.permission_mode).then(|| mode.to_string()),
        };
        self.add_account(spec);
        if f.first && self.cfg.accounts.len() > n0 {
            self.first_account_added(n0);
        }
    }

    pub fn on_add_account_click(&mut self, a: AddUi) {
        let Modal::AddAccount(form) = &mut self.modal else {
            return;
        };
        match a {
            AddUi::Inside => {}
            AddUi::Field(i) => form.focus = FIELDS[(i as usize).min(FIELDS.len() - 1)],
            AddUi::Agent(i) => {
                form.focus = Field::Agent;
                form.pick_agent(i as usize);
            }
            AddUi::Color(i) => {
                form.focus = Field::Color;
                form.color = (i as usize).min(theme::SWATCHES.len() - 1);
            }
            AddUi::Recent(i) => {
                form.focus = Field::Folder;
                if let Some(p) = form.recents.get(i as usize) {
                    form.folder = p.clone();
                    form.error = None;
                }
            }
            AddUi::Mode(d) => {
                form.focus = Field::Mode;
                form.cycle(if d == 0 { -1 } else { 1 });
            }
        }
    }
}

fn fill(buf: &mut Buffer, r: Rect, st: Style) {
    for y in r.y..r.y + r.height {
        for x in r.x..r.x + r.width {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_char(' ');
                c.set_style(st);
            }
        }
    }
}

fn frame(buf: &mut Buffer, r: Rect, st: Style) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    let (x1, y1) = (r.x + r.width - 1, r.y + r.height - 1);
    let put = |buf: &mut Buffer, x: u16, y: u16, ch: &str| {
        if let Some(c) = buf.cell_mut((x, y)) {
            c.set_symbol(ch);
            c.set_style(st);
        }
    };
    for x in r.x + 1..x1 {
        put(buf, x, r.y, "─");
        put(buf, x, y1, "─");
    }
    for y in r.y + 1..y1 {
        put(buf, r.x, y, "│");
        put(buf, x1, y, "│");
    }
    put(buf, r.x, r.y, "╭");
    put(buf, x1, r.y, "╮");
    put(buf, r.x, y1, "╰");
    put(buf, x1, y1, "╯");
}

pub fn draw(buf: &mut Buffer, area: Rect, app: &App, form: &AddForm) {
    use crate::hits::{button, text};
    let w = 115u16.min(area.width);
    let h = 23u16.min(area.height);
    let r = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    fill(buf, r, Style::default().bg(BAR_BG));
    frame(buf, r, Style::default().fg(DIM).bg(BAR_BG));
    let mut hits = app.hits.borrow_mut();
    // Clicks inside the dialog never fall through and close it.
    hits.add(r, UiAction::AddAcct(AddUi::Inside), "");
    let limit = (r.x + r.width).saturating_sub(2);
    let tx = text(
        buf,
        r.x + 2,
        r.y,
        limit,
        if form.first {
            " Welcome: add your first account "
        } else {
            " Add account "
        },
        Style::default()
            .fg(FG)
            .bg(BAR_BG)
            .add_modifier(Modifier::BOLD),
    );
    if form.first {
        text(
            buf,
            tx + 1,
            r.y,
            limit,
            &format!(" {} ", crate::first_run::step_label(2)),
            Style::default().fg(DIM).bg(BAR_BG),
        );
    }
    let bg = BAR_BG;
    let x0 = r.x + 2;
    let lx = x0 + 13; // where values start
    let label_style = |f: Field| {
        if form.focus == f {
            Style::default()
                .fg(theme::SAND)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM).bg(bg)
        }
    };
    let caption = |buf: &mut Buffer, y: u16, f: Field, s: &str| {
        let mark = if form.focus == f { "› " } else { "  " };
        text(buf, x0, y, limit, &format!("{mark}{s}"), label_style(f));
    };
    let field_box = |buf: &mut Buffer,
                     hits: &mut crate::hits::Hits,
                     y: u16,
                     f: Field,
                     idx: u8,
                     val: &str,
                     hint: &str| {
        let fw = limit.saturating_sub(lx).min(44);
        let on = form.focus == f;
        let fbg = if on { SEL_BG } else { Color::Rgb(44, 47, 51) };
        fill(buf, Rect::new(lx, y, fw, 1), Style::default().bg(fbg));
        let st = if on && f == Field::Label && form.label_pristine {
            Style::default().fg(DIM).bg(fbg)
        } else {
            Style::default().fg(FG).bg(fbg).add_modifier(Modifier::BOLD)
        };
        let x = text(buf, lx + 1, y, lx + fw, val, st);
        if on {
            text(
                buf,
                x,
                y,
                lx + fw,
                "▏",
                Style::default().fg(theme::SAND).bg(fbg),
            );
        }
        hits.add(
            Rect::new(lx, y, fw, 1),
            UiAction::AddAcct(AddUi::Field(idx)),
            hint,
        );
    };
    if r.height < 8 || r.width < 40 {
        drop(hits);
        crate::ui_chrome::modal_chrome(
            buf,
            app,
            r,
            &[("Cancel", UiAction::ModalCancel, "Close (Esc)")],
        );
        return;
    }
    let mut y = r.y + 2;
    // Name.
    caption(buf, y, Field::Label, "Name");
    field_box(
        buf,
        &mut hits,
        y,
        Field::Label,
        0,
        &form.label,
        "The account's name, shown on its panes",
    );
    y += 2;
    // Agent cards.
    caption(buf, y + 1, Field::Agent, "Agent");
    let cw = (limit.saturating_sub(lx) / 3)
        .saturating_sub(1)
        .clamp(10, 34);
    for (i, (_, name, sub)) in AGENTS.iter().enumerate() {
        let cx = lx + (i % 3) as u16 * (cw + 2);
        if cx + cw > r.x + r.width - 1 {
            break;
        }
        let cy = y + (i / 3) as u16 * 4;
        let card = Rect::new(cx, cy, cw, 4);
        let chosen = form.agent == i;
        let disabled = !form.ok[i];
        let cbg = if chosen { SEL_BG } else { bg };
        fill(buf, card, Style::default().bg(cbg));
        let border = if disabled {
            FAINT
        } else if chosen {
            theme::SAGE
        } else {
            DIM
        };
        frame(buf, card, Style::default().fg(border).bg(cbg));
        let tst = if disabled {
            Style::default().fg(FAINT).bg(cbg)
        } else if chosen {
            Style::default().fg(FG).bg(cbg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(FG).bg(cbg)
        };
        let dot = if chosen { "●" } else { "○" };
        let tx = text(
            buf,
            cx + 2,
            cy + 1,
            cx + cw - 1,
            &format!("{dot} {}  {name}", i + 1),
            tst,
        );
        // Installed or not, on the right of the name where it fits.
        let (mark, mc) = if disabled {
            ("missing", theme::CLAY)
        } else {
            ("✓", theme::SAGE)
        };
        let mw = mark.chars().count() as u16;
        if cx + cw >= tx + mw + 3 {
            text(
                buf,
                cx + cw - 2 - mw,
                cy + 1,
                cx + cw - 1,
                mark,
                Style::default().fg(mc).bg(cbg),
            );
        }
        let sub = if disabled && form.can_install[i] {
            "press i to install it"
        } else if disabled {
            "how to get it: see below"
        } else {
            sub
        };
        text(
            buf,
            cx + 2,
            cy + 2,
            cx + cw - 1,
            sub,
            Style::default()
                .fg(if disabled { theme::SAND } else { DIM })
                .bg(cbg),
        );
        let hint = if disabled {
            form.how[i].clone()
        } else {
            format!("{name}: {sub} ({})", i + 1)
        };
        hits.add(card, UiAction::AddAcct(AddUi::Agent(i as u8)), hint);
    }
    y += 9;
    // Color swatches.
    caption(buf, y, Field::Color, "Color");
    let mut x = lx;
    for (i, name) in theme::SWATCHES.iter().enumerate() {
        if x + 3 > limit {
            break;
        }
        let c = theme::parse_color(name);
        let on = form.color == i;
        let s = if on { "[●]" } else { " ● " };
        let st = Style::default().fg(c).bg(if on { SEL_BG } else { bg });
        text(buf, x, y, limit, s, st);
        hits.add(
            Rect::new(x, y, 3, 1),
            UiAction::AddAcct(AddUi::Color(i as u8)),
            format!("Color: {name}"),
        );
        x += 4;
    }
    y += 2;
    // Default folder and recents.
    caption(buf, y, Field::Folder, "Folder");
    field_box(
        buf,
        &mut hits,
        y,
        Field::Folder,
        3,
        &form.folder,
        "Where this account's new tabs start",
    );
    y += 1;
    if form.recents.is_empty() {
        text(
            buf,
            lx,
            y,
            limit,
            "new tabs start here",
            Style::default().fg(FAINT).bg(bg),
        );
    } else {
        let mut x = text(
            buf,
            lx,
            y,
            limit,
            "recent ",
            Style::default().fg(FAINT).bg(bg),
        );
        for (i, p) in form.recents.iter().enumerate() {
            let short = crate::paths::shorten_path(p, 18);
            if x + short.chars().count() as u16 + 3 > limit {
                break;
            }
            x = button(
                buf,
                &mut hits,
                app.mouse_pos,
                x,
                y,
                limit,
                &short,
                UiAction::AddAcct(AddUi::Recent(i as u8)),
                &format!("Use {p}"),
                theme::SLATE,
            ) + 1;
        }
    }
    y += 2;
    // Permission mode.
    caption(buf, y, Field::Mode, "Permissions");
    let mode =
        crate::config::PERMISSION_MODES[form.mode.min(crate::config::PERMISSION_MODES.len() - 1)];
    let x = button(
        buf,
        &mut hits,
        app.mouse_pos,
        lx,
        y,
        limit,
        "‹",
        UiAction::AddAcct(AddUi::Mode(0)),
        "Previous permission mode",
        theme::STONE,
    );
    let x = text(
        buf,
        x + 1,
        y,
        limit,
        mode,
        Style::default()
            .fg(FG)
            .bg(if form.focus == Field::Mode {
                SEL_BG
            } else {
                bg
            })
            .add_modifier(Modifier::BOLD),
    );
    let x = button(
        buf,
        &mut hits,
        app.mouse_pos,
        x + 1,
        y,
        limit,
        "›",
        UiAction::AddAcct(AddUi::Mode(1)),
        "Next permission mode",
        theme::STONE,
    );
    let what = match mode {
        "bypass" => "skips permission prompts",
        "default" => "the agent's own default",
        "plan" => "plans before changing files",
        "accept-edits" => "accepts file edits",
        _ => "",
    };
    text(
        buf,
        x + 2,
        y,
        limit,
        what,
        Style::default().fg(FAINT).bg(bg),
    );
    y += 2;
    // Error, or what Enter does.
    let (msg, c) = match &form.error {
        Some(e) if e.to_lowercase().contains("install") => (e.clone(), theme::SAND),
        Some(e) => (e.clone(), theme::CLAY),
        None => (
            format!(
                "Tab moves, ←/→ change, 1 to 6 pick the agent. Enter adds it and starts its login in {}.",
                AGENTS[form.agent].1
            ),
            FAINT,
        ),
    };
    // Up to two lines (an install command can be long).
    let width = limit.saturating_sub(x0) as usize;
    for (k, line) in crate::first_run::wrap(&msg, width, 2).iter().enumerate() {
        let yy = y + k as u16;
        if yy >= r.y + r.height - 1 {
            break;
        }
        text(buf, x0, yy, limit, line, Style::default().fg(c).bg(bg));
    }
    drop(hits);
    crate::ui_chrome::modal_chrome(
        buf,
        app,
        r,
        &[
            (
                "Add & log in",
                UiAction::ModalOk,
                "Add the account and start its login (Enter)",
            ),
            ("Cancel", UiAction::ModalCancel, "Close (Esc)"),
        ],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn form() -> AddForm {
        AddForm {
            label: "Account 3".into(),
            label_pristine: true,
            agent: 0,
            color: 0,
            folder: "~".into(),
            mode: 0,
            focus: Field::Agent,
            ok: [true; 6],
            how: Default::default(),
            can_install: [false; 6],
            first: false,
            recents: vec!["~/code".into()],
            error: None,
        }
    }

    #[test]
    fn native_agent_shortcuts_and_missing_binary() {
        let mut f = form();
        for (i, c) in ['3', '4', '5', '6'].into_iter().enumerate() {
            f.key(key(KeyCode::Char(c)));
            assert_eq!(f.agent, i + 2);
            assert!(f.error.is_none());
        }
        // A missing one can be picked, says how to get it, and Enter waits.
        f.ok[2] = false;
        f.how[2] = "Install Codex CLI from https://x, or set codex_bin in config.toml.".into();
        f.pick_agent(2);
        assert_eq!(f.agent, 2);
        assert!(f.error.as_ref().unwrap().contains("codex_bin"));
        assert!(f
            .check()
            .unwrap_err()
            .contains("Codex CLI is not installed"));
        // Another key on the agent row keeps the explanation up.
        f.key(key(KeyCode::Char('3')));
        assert!(f.error.is_some());
        f.key(key(KeyCode::Char('1')));
        assert!(f.error.is_none());
    }

    #[test]
    fn keyboard_moves_and_changes() {
        let mut f = form();
        f.key(key(KeyCode::Char('2')));
        assert_eq!(f.agent, 1);
        f.key(key(KeyCode::Left));
        assert_eq!(f.agent, 0);
        f.key(key(KeyCode::Up));
        assert_eq!(f.focus, Field::Label);
        // Typing replaces the suggestion; digits are text here.
        f.key(key(KeyCode::Char('W')));
        f.key(key(KeyCode::Char('2')));
        assert_eq!(f.label, "W2");
        assert_eq!(f.agent, 0);
        f.key(key(KeyCode::Tab));
        f.key(key(KeyCode::Tab));
        assert_eq!(f.focus, Field::Color);
        f.key(key(KeyCode::Right));
        assert_eq!(f.color, 1);
        f.key(key(KeyCode::Tab));
        f.key(key(KeyCode::Right));
        assert_eq!(f.folder, "~/code");
        for _ in 0..4 {
            f.key(key(KeyCode::BackTab));
        }
        assert_eq!(f.focus, Field::Mode, "wraps around");
        f.key(key(KeyCode::Right));
        assert_eq!(f.mode, 1);
        assert!(f.key(key(KeyCode::Enter)));
    }

    #[test]
    fn grok_missing_offers_the_install() {
        let mut f = form();
        f.ok[1] = false;
        f.can_install[1] = true;
        f.how[1] = "curl -fsSL https://x.ai/cli/install.sh | bash".into();
        f.key(key(KeyCode::Char('2')));
        assert_eq!(f.agent, 1);
        let e = f.error.clone().unwrap();
        assert!(
            e.contains("Grok Build is not installed. Press i to install it"),
            "{e}"
        );
        assert!(f.check().is_err());
        f.key(key(KeyCode::Right));
        assert_eq!(f.agent, 2);
        assert!(f.error.is_none());
    }

    #[test]
    fn checks() {
        let mut f = form();
        assert!(f.check().is_ok());
        f.label = "  ".into();
        assert!(f.check().is_err());
        f.label = "x".into();
        f.folder = "/definitely/not/here".into();
        assert!(f.check().unwrap_err().contains("not a folder"));
    }
}

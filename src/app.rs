//! Ядро приложения: состояние, действия, горячие клавиши и маршрутизация ввода.
//! Отрисовка — в `ui.rs`.

use crate::config::{AuthMethod, Config, OpenTab, SessionDef, SessionKind};
use crate::terminal::{self, TermSession};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    Single,
    Cols2,
    Rows2,
    Grid4,
}

impl Layout {
    pub fn count(self) -> usize {
        match self {
            Layout::Single => 1,
            Layout::Cols2 | Layout::Rows2 => 2,
            Layout::Grid4 => 4,
        }
    }
}

#[derive(PartialEq, Eq)]
pub enum Page {
    Terminal,
    Settings,
}

/// Действия приложения (горячие клавиши, палитра, меню).
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Palette,
    NewSsh,
    NewLocal(Option<String>, String),
    CloseTab,
    NextTab,
    PrevTab,
    Copy,
    Paste,
    Search,
    Broadcast,
    ToggleSftp,
    Reconnect,
    Duplicate,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Layout(Layout),
    Settings,
    ImportSshConfig,
    Snippet(usize),
    ScrollPage(bool),
    ScrollEdge(bool),
    Connect(u64),
    QuickConnect(String, String, u16),
}

/// Форма редактора сессии.
pub struct Editor {
    pub id: u64,
    pub name: String,
    pub folder: String,
    pub host: String,
    pub port: String,
    pub user: String,
    pub identity: String,
    pub jump: String,
    pub tags: String,
    pub color: Option<[u8; 3]>,
    pub error: Option<String>,
}

impl Editor {
    pub fn new(folder: &str) -> Self {
        Self {
            id: 0,
            name: String::new(),
            folder: folder.to_string(),
            host: String::new(),
            port: "22".into(),
            user: std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_default(),
            identity: String::new(),
            jump: String::new(),
            tags: String::new(),
            color: None,
            error: None,
        }
    }

    pub fn from_def(d: &SessionDef) -> Self {
        let mut e = Self::new(&d.group);
        e.id = d.id;
        e.name = d.name.clone();
        e.color = d.color;
        e.tags = d.tags.join(", ");
        if let SessionKind::Ssh { host, port, user, auth, jump } = &d.kind {
            e.host = host.clone();
            e.port = port.to_string();
            e.user = user.clone();
            e.identity = auth.identity().unwrap_or_default().to_string();
            e.jump = jump.clone();
        }
        e
    }

    /// Проверка и сборка определения сессии.
    pub fn build(&self) -> Result<SessionDef, String> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err("Укажите хост".into());
        }
        let port: u16 = self.port.trim().parse().map_err(|_| "Порт — число от 1 до 65535".to_string())?;
        if self.user.trim().is_empty() {
            return Err("Укажите пользователя".into());
        }
        let name = if self.name.trim().is_empty() {
            format!("{}@{}", self.user.trim(), host)
        } else {
            self.name.trim().to_string()
        };
        let identity = self.identity.trim();
        Ok(SessionDef {
            id: self.id,
            name,
            group: if self.folder.trim().is_empty() { "SSH".into() } else { self.folder.trim().into() },
            kind: SessionKind::Ssh {
                host: host.into(),
                port,
                user: self.user.trim().into(),
                auth: if identity.is_empty() {
                    AuthMethod::Password
                } else {
                    AuthMethod::Key { path: identity.into() }
                },
                jump: self.jump.trim().into(),
            },
            color: self.color,
            tags: self.tags.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
            use_count: 0,
            last_used: 0,
        })
    }
}

pub struct Palette {
    pub query: String,
    pub selected: usize,
    pub focus: bool,
}

pub struct MobaApp {
    pub(crate) cfg: Config,
    pub(crate) tabs: Vec<TermSession>,
    pub(crate) active: Option<usize>,
    pub(crate) layout: Layout,
    pub(crate) panes: Vec<usize>,
    pub(crate) broadcast: bool,
    pub(crate) shells: Vec<terminal::local::ShellProfile>,
    pub(crate) page: Page,
    pub(crate) show_sftp: bool,

    pub(crate) filter: String,
    pub(crate) selected_session: Option<u64>,
    pub(crate) new_folder: Option<String>,
    pub(crate) rename_folder: Option<(String, String)>,
    pub(crate) confirm_delete: Option<u64>,
    pub(crate) confirm_folder_delete: Option<String>,

    pub(crate) palette: Option<Palette>,
    pub(crate) editor: Option<Editor>,
    pub(crate) paste_confirm: Option<String>,
    pub(crate) prompt_inputs: Vec<String>,
    pub(crate) prompt_remember: bool,
    pub(crate) prompt_focus: bool,
    pub(crate) forget_confirm: bool,

    pub(crate) sftp_path: String,
    pub(crate) sftp_path_for: String,
    pub(crate) sftp_rename: Option<(String, String)>,
    pub(crate) sftp_mkdir: Option<String>,
    pub(crate) sftp_delete: Option<(String, bool)>,
    pub(crate) sftp_selected: Option<String>,

    pub(crate) status: String,
    pub(crate) toast: Option<(String, Instant)>,
    pub(crate) dirty_cfg: bool,
    pub(crate) last_title: String,
    clipboard: Option<arboard::Clipboard>,
}

impl MobaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::ui::install_theme(&cc.egui_ctx);
        let cfg = Config::load();
        let mut app = Self {
            cfg,
            tabs: Vec::new(),
            active: None,
            layout: Layout::Single,
            panes: Vec::new(),
            broadcast: false,
            shells: terminal::local::detect_shells(),
            page: Page::Terminal,
            show_sftp: true,
            filter: String::new(),
            selected_session: None,
            new_folder: None,
            rename_folder: None,
            confirm_delete: None,
            confirm_folder_delete: None,
            palette: None,
            editor: None,
            paste_confirm: None,
            prompt_inputs: Vec::new(),
            prompt_remember: false,
            prompt_focus: true,
            forget_confirm: false,
            sftp_path: String::new(),
            sftp_path_for: String::new(),
            sftp_rename: None,
            sftp_mkdir: None,
            sftp_delete: None,
            sftp_selected: None,
            status: String::new(),
            toast: None,
            dirty_cfg: false,
            last_title: String::new(),
            clipboard: None,
        };
        app.show_sftp = app.cfg.settings.sftp_auto_open;
        // REL-04: восстановить вкладки прошлого запуска.
        if app.cfg.settings.restore_tabs {
            let tabs = app.cfg.open_tabs.clone();
            for t in tabs {
                match t {
                    OpenTab::Session(id) => {
                        if app.cfg.find(id).is_some() {
                            app.connect(&cc.egui_ctx, id);
                        }
                    }
                    OpenTab::Local { title, shell } => app.open_local(&cc.egui_ctx, &title, shell),
                }
            }
        }
        if app.tabs.is_empty() && app.cfg.sessions.is_empty() {
            app.status = "Добро пожаловать! Ctrl+Shift+K — подключиться, или импортируйте ~/.ssh/config".into();
        }
        app
    }

    pub(crate) fn toast(&mut self, s: impl Into<String>) {
        self.toast = Some((s.into(), Instant::now()));
    }

    pub(crate) fn active_tab(&self) -> Option<&TermSession> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    pub(crate) fn active_tab_mut(&mut self) -> Option<&mut TermSession> {
        self.active.and_then(|i| self.tabs.get_mut(i))
    }

    fn scrollback(&self) -> usize {
        self.cfg.settings.scrollback.clamp(500, 100_000)
    }

    // ── вкладки ─────────────────────────────────────

    fn push_tab(&mut self, t: TermSession) {
        self.tabs.push(t);
        let i = self.tabs.len() - 1;
        self.set_active(i);
        self.persist_tabs();
    }

    pub(crate) fn set_active(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if self.layout != Layout::Single && !self.panes.contains(&i) {
            if let Some(slot) = self.active.and_then(|a| self.panes.iter().position(|&p| p == a)) {
                self.panes[slot] = i;
            } else if !self.panes.is_empty() {
                self.panes[0] = i;
            }
        }
        self.active = Some(i);
        self.page = Page::Terminal;
    }

    pub(crate) fn open_local(&mut self, ctx: &egui::Context, title: &str, shell: Option<String>) {
        match terminal::local::spawn(ctx, title, shell, 24, 80, self.scrollback()) {
            Ok(t) => self.push_tab(t),
            Err(e) => self.toast(format!("Не удалось запустить shell: {e}")),
        }
    }

    /// Подключиться к сохранённой сессии.
    pub(crate) fn connect(&mut self, ctx: &egui::Context, id: u64) {
        let Some(def) = self.cfg.find(id).cloned() else { return };
        match &def.kind {
            SessionKind::Local { shell } => {
                self.open_local(ctx, &def.name, shell.clone());
            }
            SessionKind::Ssh { .. } => {
                #[cfg(feature = "ssh")]
                {
                    let target = self.target_for(&def);
                    let mut t = terminal::ssh::spawn(ctx, &def.name, target, 24, 80, self.scrollback());
                    t.def_id = Some(def.id);
                    t.color = def.color;
                    self.push_tab(t);
                }
                #[cfg(not(feature = "ssh"))]
                self.toast("Сборка без SSH");
            }
        }
        self.cfg.touch(id);
        self.dirty_cfg = true;
    }

    /// Собрать цепочку хостов: jump-токен может ссылаться на сохранённую сессию по имени.
    #[cfg(feature = "ssh")]
    fn target_for(&self, def: &SessionDef) -> terminal::ssh::Target {
        use terminal::ssh::Hop;
        let SessionKind::Ssh { host, port, user, auth, jump } = &def.kind else { unreachable!() };
        let mut hops = Vec::new();
        for token in jump.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()) {
            let named = self.cfg.sessions.iter().find(|s| s.name.eq_ignore_ascii_case(token) && s.is_ssh());
            if let Some(SessionDef { kind: SessionKind::Ssh { host, port, user, auth, .. }, .. }) = named {
                hops.push(Hop { host: host.clone(), port: *port, user: user.clone(), identity: auth.identity().map(Into::into) });
            } else {
                for (u, h, p) in crate::sshconfig::parse_jump(token, user) {
                    hops.push(Hop { host: h, port: p, user: u, identity: None });
                }
            }
        }
        hops.push(Hop { host: host.clone(), port: *port, user: user.clone(), identity: auth.identity().map(Into::into) });
        terminal::ssh::Target {
            hops,
            keepalive: self.cfg.settings.keepalive_secs,
            auto_reconnect: self.cfg.settings.auto_reconnect,
        }
    }

    /// Быстрое подключение `user@host:port`: сессия сохраняется автоматически.
    pub(crate) fn quick_connect(&mut self, ctx: &egui::Context, user: &str, host: &str, port: u16) {
        let id = match self.cfg.find_ssh(host, user, port) {
            Some(id) => id,
            None => self.cfg.upsert(SessionDef {
                id: 0,
                name: format!("{user}@{host}"),
                group: "SSH".into(),
                kind: SessionKind::Ssh {
                    host: host.into(),
                    port,
                    user: user.into(),
                    auth: AuthMethod::Password,
                    jump: String::new(),
                },
                color: None,
                tags: vec![],
                use_count: 0,
                last_used: 0,
            }),
        };
        self.dirty_cfg = true;
        self.connect(ctx, id);
    }

    pub(crate) fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs[i].close();
        self.tabs.remove(i);
        self.panes.retain(|&p| p != i);
        for p in &mut self.panes {
            if *p > i {
                *p -= 1;
            }
        }
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a > i => Some(a - 1),
            Some(a) if a == i => Some(i.min(self.tabs.len() - 1)),
            other => other,
        };
        if self.layout != Layout::Single {
            self.fill_panes();
        }
        self.persist_tabs();
    }

    pub(crate) fn close_others(&mut self, keep: usize) {
        let mut i = self.tabs.len();
        while i > 0 {
            i -= 1;
            if i != keep {
                self.close_tab(i);
            }
        }
    }

    pub(crate) fn duplicate(&mut self, ctx: &egui::Context, i: usize) {
        let Some(t) = self.tabs.get(i) else { return };
        if let Some(id) = t.def_id {
            self.connect(ctx, id);
        } else if let terminal::TabKind::Local { shell } = &t.kind {
            let (title, shell) = (t.title.clone(), shell.clone());
            self.open_local(ctx, &title, shell);
        }
    }

    pub(crate) fn reconnect(&mut self, i: usize) {
        let Some(t) = self.tabs.get_mut(i) else { return };
        if t.is_ssh() {
            t.reconnect();
        } else if let Err(e) = terminal::local::respawn(t) {
            self.toast(format!("Не удалось перезапустить: {e}"));
        }
    }

    /// Запомнить открытые вкладки для восстановления (без секретов).
    pub(crate) fn persist_tabs(&mut self) {
        self.cfg.open_tabs = self
            .tabs
            .iter()
            .filter_map(|t| match (&t.kind, t.def_id) {
                (_, Some(id)) => Some(OpenTab::Session(id)),
                (terminal::TabKind::Local { shell }, None) => Some(OpenTab::Local { title: t.title.clone(), shell: shell.clone() }),
                _ => None,
            })
            .collect();
        self.dirty_cfg = true;
    }

    pub(crate) fn set_layout(&mut self, l: Layout) {
        self.layout = l;
        self.fill_panes();
    }

    fn fill_panes(&mut self) {
        let n = self.layout.count().min(self.tabs.len());
        let mut panes: Vec<usize> = self.panes.iter().copied().filter(|&p| p < self.tabs.len()).collect();
        panes.dedup();
        if let Some(a) = self.active {
            if !panes.contains(&a) {
                panes.insert(0, a);
            }
        }
        for i in 0..self.tabs.len() {
            if panes.len() >= n {
                break;
            }
            if !panes.contains(&i) {
                panes.push(i);
            }
        }
        panes.truncate(n);
        self.panes = panes;
    }

    // ── буфер обмена и вставка ───────────────────────

    pub(crate) fn clipboard_text(&mut self) -> Option<String> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()?.get_text().ok()
    }

    /// TRM-03: многострочная или большая вставка — с подтверждением,
    /// если программа не включила bracketed paste.
    pub(crate) fn request_paste(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        let Some(t) = self.active_tab() else { return };
        let risky = text.trim_end_matches('\n').contains('\n') || text.len() > 5 * 1024;
        if self.cfg.settings.confirm_multiline_paste && risky && !t.bracketed_paste() {
            self.paste_confirm = Some(text);
        } else {
            self.do_paste(&text);
        }
    }

    pub(crate) fn do_paste(&mut self, text: &str) {
        for i in self.input_targets() {
            self.tabs[i].paste(text);
        }
    }

    /// Куда идёт ввод: активная вкладка или все (рассылка, BRD-01).
    pub(crate) fn input_targets(&self) -> Vec<usize> {
        let Some(a) = self.active else { return vec![] };
        if !self.broadcast {
            return vec![a];
        }
        let mut v: Vec<usize> = (0..self.tabs.len())
            .filter(|&i| i == a || (!self.tabs[i].broadcast_excluded && self.tabs[i].is_alive()))
            .collect();
        v.dedup();
        v
    }

    fn send_bytes(&mut self, bytes: &[u8]) {
        for i in self.input_targets() {
            self.tabs[i].write_input(bytes);
        }
    }

    pub(crate) fn run_snippet(&mut self, n: usize) {
        let Some(s) = self.cfg.snippets.get(n).cloned() else { return };
        let mut text = s.text.replace("\r\n", "\n").replace('\n', "\r");
        if s.send_enter {
            text.push('\r');
        }
        self.send_bytes(text.as_bytes());
    }

    // ── импорт ~/.ssh/config ─────────────────────────

    pub(crate) fn import_ssh_config(&mut self) {
        let path = crate::sshconfig::default_path();
        if !path.exists() {
            self.toast(format!("Файл {} не найден", path.display()));
            return;
        }
        let rep = crate::sshconfig::import(&path);
        let mut added = 0;
        for h in &rep.hosts {
            let exists = self.cfg.find_ssh(&h.host, &h.user, h.port).is_some();
            self.cfg.upsert(SessionDef {
                id: 0,
                name: h.alias.clone(),
                group: "ssh_config".into(),
                kind: SessionKind::Ssh {
                    host: h.host.clone(),
                    port: h.port,
                    user: h.user.clone(),
                    auth: match &h.identity {
                        Some(p) => AuthMethod::Key { path: p.clone() },
                        None => AuthMethod::Password,
                    },
                    jump: h.jump.clone(),
                },
                color: None,
                tags: vec![],
                use_count: 0,
                last_used: 0,
            });
            if !exists {
                added += 1;
            }
        }
        self.dirty_cfg = true;
        let ignored = if rep.ignored.is_empty() {
            String::new()
        } else {
            format!(" · пропущены директивы: {}", rep.ignored.iter().cloned().collect::<Vec<_>>().join(", "))
        };
        self.toast(format!("Импорт ssh_config: {} хостов ({} новых){}", rep.hosts.len(), added, ignored));
    }

    // ── действия ────────────────────────────────────

    pub(crate) fn run_action(&mut self, ctx: &egui::Context, a: Action) {
        match a {
            Action::Palette => {
                self.palette = if self.palette.is_some() { None } else { Some(Palette { query: String::new(), selected: 0, focus: true }) }
            }
            Action::NewSsh => self.editor = Some(Editor::new("SSH")),
            Action::NewLocal(shell, title) => self.open_local(ctx, &title, shell),
            Action::CloseTab => {
                if let Some(i) = self.active {
                    self.close_tab(i);
                }
            }
            Action::NextTab | Action::PrevTab => {
                let n = self.tabs.len();
                if let (Some(i), true) = (self.active, n > 0) {
                    let j = if a == Action::NextTab { (i + 1) % n } else { (i + n - 1) % n };
                    self.set_active(j);
                }
            }
            Action::Copy => {
                if let Some(t) = self.active_tab_mut() {
                    if let Some(s) = t.selection_text() {
                        t.clear_selection();
                        ctx.copy_text(s);
                    }
                }
            }
            Action::Paste => {
                if let Some(t) = self.clipboard_text() {
                    self.request_paste(t);
                }
            }
            Action::Search => {
                if let Some(t) = self.active_tab_mut() {
                    t.search.open = !t.search.open;
                }
            }
            Action::Broadcast => {
                self.broadcast = !self.broadcast;
                self.toast(if self.broadcast { "Рассылка ввода ВКЛЮЧЕНА — ввод уходит во все вкладки" } else { "Рассылка ввода выключена" });
            }
            Action::ToggleSftp => self.show_sftp = !self.show_sftp,
            Action::Reconnect => {
                if let Some(i) = self.active {
                    self.reconnect(i);
                }
            }
            Action::Duplicate => {
                if let Some(i) = self.active {
                    self.duplicate(ctx, i);
                }
            }
            Action::ZoomIn => self.zoom(1.0),
            Action::ZoomOut => self.zoom(-1.0),
            Action::ZoomReset => {
                self.cfg.settings.font_size = 13.5;
                self.dirty_cfg = true;
            }
            Action::Layout(l) => self.set_layout(l),
            Action::Settings => {
                self.page = if self.page == Page::Settings { Page::Terminal } else { Page::Settings };
            }
            Action::ImportSshConfig => self.import_ssh_config(),
            Action::Snippet(n) => self.run_snippet(n),
            Action::ScrollPage(up) => {
                if let Some(t) = self.active_tab_mut() {
                    t.scroll_page(up);
                }
            }
            Action::ScrollEdge(top) => {
                if let Some(t) = self.active_tab_mut() {
                    if top { t.scroll_to_top() } else { t.set_scroll(0) }
                }
            }
            Action::Connect(id) => self.connect(ctx, id),
            Action::QuickConnect(u, h, p) => self.quick_connect(ctx, &u, &h, p),
        }
    }

    pub(crate) fn zoom(&mut self, d: f32) {
        let s = &mut self.cfg.settings.font_size;
        *s = (*s + d).clamp(8.0, 32.0);
        self.dirty_cfg = true;
    }

    /// Горячие клавиши (точное совпадение модификаторов). Не конфликтуют с shell:
    /// все приложенческие — на Ctrl+Shift, кроме Ctrl+Tab и Ctrl+F1…F12.
    fn shortcut(key: egui::Key, m: egui::Modifiers) -> Option<Action> {
        use egui::Key::*;
        let cs = m.ctrl && m.shift && !m.alt;
        let c = m.ctrl && !m.shift && !m.alt;
        let s = m.shift && !m.ctrl && !m.alt;
        Some(match key {
            P | K if cs => Action::Palette,
            N if cs => Action::NewSsh,
            T if cs => Action::NewLocal(None, "shell".into()),
            W if cs => Action::CloseTab,
            Tab if cs => Action::PrevTab,
            Tab if c => Action::NextTab,
            C if cs => Action::Copy,
            V if cs => Action::Paste,
            F if cs => Action::Search,
            B if cs => Action::Broadcast,
            E if cs => Action::ToggleSftp,
            R if cs => Action::Reconnect,
            D if cs => Action::Duplicate,
            Plus | Equals if cs || c => Action::ZoomIn,
            Minus if cs => Action::ZoomOut,
            Num0 if cs => Action::ZoomReset,
            Comma if cs => Action::Settings,
            PageUp if s => Action::ScrollPage(true),
            PageDown if s => Action::ScrollPage(false),
            Home if s => Action::ScrollEdge(true),
            End if s => Action::ScrollEdge(false),
            F1 if c => Action::Snippet(0),
            F2 if c => Action::Snippet(1),
            F3 if c => Action::Snippet(2),
            F4 if c => Action::Snippet(3),
            F5 if c => Action::Snippet(4),
            F6 if c => Action::Snippet(5),
            F7 if c => Action::Snippet(6),
            F8 if c => Action::Snippet(7),
            F9 if c => Action::Snippet(8),
            F10 if c => Action::Snippet(9),
            F11 if c => Action::Snippet(10),
            F12 if c => Action::Snippet(11),
            _ => return None,
        })
    }

    fn overlay_open(&self) -> bool {
        self.palette.is_some() || self.editor.is_some() || self.paste_confirm.is_some() || self.any_prompt()
    }

    pub(crate) fn any_prompt(&self) -> bool {
        #[cfg(feature = "ssh")]
        {
            self.tabs.iter().any(|t| t.ssh.as_ref().is_some_and(|s| s.has_prompt()))
        }
        #[cfg(not(feature = "ssh"))]
        false
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let mut actions = Vec::new();
        let terminal_mode = self.page == Page::Terminal && !self.overlay_open();
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                if let egui::Event::Key { key, pressed: true, modifiers, .. } = e {
                    if let Some(a) = Self::shortcut(*key, *modifiers) {
                        // Прокрутку и сниппеты перехватываем только в режиме терминала.
                        let term_only = matches!(a, Action::ScrollPage(_) | Action::ScrollEdge(_) | Action::Snippet(_) | Action::Copy);
                        if !term_only || terminal_mode {
                            actions.push(a);
                            return false;
                        }
                    }
                }
                true
            });
        });
        for a in actions {
            self.run_action(ctx, a);
        }
        // Ctrl + колесо — масштаб шрифта.
        let z = ctx.input(|i| i.zoom_delta());
        if z != 1.0 && self.page == Page::Terminal {
            self.zoom(if z > 1.0 { 0.5 } else { -0.5 });
        }
    }

    /// Ввод с клавиатуры → активная вкладка (или все при рассылке).
    fn pump_input(&mut self, ctx: &egui::Context) {
        if self.active.is_none() || self.page != Page::Terminal || self.overlay_open() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let app_cursor = self.active_tab().map(|t| t.app_cursor()).unwrap_or(false);
        let has_sel = self.active_tab().map(|t| t.has_selection()).unwrap_or(false);
        let mut out: Vec<u8> = Vec::new();
        let mut paste: Option<String> = None;
        let mut copy = false;
        ctx.input_mut(|input| {
            input.events.retain(|ev| {
                match ev {
                    egui::Event::Text(t) => out.extend_from_slice(t.as_bytes()),
                    egui::Event::Copy => {
                        // Ctrl+C: копировать, если есть выделение, иначе — прерывание (^C).
                        if has_sel { copy = true } else { out.push(0x03) }
                    }
                    egui::Event::Cut => out.push(0x18),
                    egui::Event::Paste(t) => paste = Some(t.clone()),
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        let m = *modifiers;
                        if m.ctrl && !m.alt && !m.shift {
                            if let Some(b) = terminal::ctrl_byte(*key) {
                                out.push(b);
                                return false;
                            }
                        }
                        if m.alt && !m.ctrl {
                            if let Some(b) = terminal::alt_letter(*key, m.shift) {
                                out.extend_from_slice(&b);
                                return false;
                            }
                        }
                        match terminal::encode_key(*key, m, app_cursor) {
                            Some(b) => out.extend_from_slice(&b),
                            None => return true,
                        }
                    }
                    _ => return true,
                }
                false
            });
        });
        if copy {
            self.run_action(ctx, Action::Copy);
        }
        if !out.is_empty() {
            self.send_bytes(&out);
        }
        if let Some(t) = paste {
            self.request_paste(t);
        }
    }

    /// Файлы, брошенные на окно, — загрузка в текущую папку SFTP.
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let files: Vec<std::path::PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if files.is_empty() {
            return;
        }
        #[cfg(feature = "ssh")]
        {
            let Some(t) = self.active_tab() else { return };
            let Some(ssh) = &t.ssh else {
                self.toast("Перетаскивание файлов работает для SSH-вкладок");
                return;
            };
            let cwd = ssh.sftp.lock().map(|v| v.cwd.clone()).unwrap_or_default();
            let dir = if cwd.is_empty() { ".".to_string() } else { cwd };
            for f in &files {
                t.sftp(terminal::ssh::SftpCmd::Upload { local: f.clone(), dir: dir.clone() });
            }
            self.show_sftp = true;
            self.toast(format!("Загрузка {} файл(ов) в {dir}", files.len()));
        }
    }
}

impl eframe::App for MobaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_shortcuts(&ctx);
        self.pump_input(&ctx);
        self.handle_drops(&ctx);

        crate::ui::draw(self, ui);

        // Заголовок окна по активной вкладке.
        let title = match self.active_tab() {
            Some(t) => format!("{} — MobaRust", t.title),
            None => "MobaRust".into(),
        };
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }

        if self.dirty_cfg {
            self.dirty_cfg = false;
            self.cfg.save();
        }

        // В простое — без перерисовок: потоки сами будят UI. Мигание курсора
        // и обратный отсчёт переподключения — редкие перерисовки.
        let focused = ctx.input(|i| i.focused);
        if focused && self.active.is_some() && self.cfg.settings.cursor_blink && self.page == Page::Terminal {
            ctx.request_repaint_after(std::time::Duration::from_millis(530));
        }
        if self.toast.as_ref().is_some_and(|(_, t)| t.elapsed().as_secs() < 6) {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        #[cfg(feature = "ssh")]
        if self.tabs.iter().any(|t| t.ssh.as_ref().is_some_and(|s| s.retry_at.lock().map(|r| r.is_some()).unwrap_or(false))) {
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    fn on_exit(&mut self) {
        self.persist_tabs();
        self.cfg.save();
        for t in &self.tabs {
            t.close();
        }
    }
}

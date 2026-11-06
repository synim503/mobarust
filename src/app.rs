//! UI приложения на egui: тулбар, менеджер сессий, вкладки терминалов,
//! статус-бар и диалог подключения.

use crate::config::{Config, SessionDef, SessionKind};
use crate::terminal::{self, TermSession};
use std::collections::BTreeMap;

/// Что показано в левой панели снизу.
#[derive(PartialEq)]
enum SideTab {
    Sessions,
    Tools,
    Macros,
}

/// Форма подключения по SSH.
struct SshForm {
    name: String,
    host: String,
    user: String,
    port: String,
    password: String,
    save: bool,
}

impl SshForm {
    fn from_def(name: &str, host: &str, user: &str, port: u16) -> Self {
        Self {
            name: name.to_string(),
            host: host.to_string(),
            user: user.to_string(),
            port: port.to_string(),
            password: String::new(),
            save: false,
        }
    }
}

pub struct MobaApp {
    cfg: Config,
    tabs: Vec<TermSession>,
    active: Option<usize>,
    omnibox: String,
    side_tab: SideTab,
    ssh_form: Option<SshForm>,
    status: String,
}

impl MobaApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_theme(&cc.egui_ctx);
        Self {
            cfg: Config::load(),
            tabs: Vec::new(),
            active: None,
            omnibox: String::new(),
            side_tab: SideTab::Sessions,
            ssh_form: None,
            status: "Готов".into(),
        }
    }

    // ── действия ──────────────────────────────────────────

    fn open_local(&mut self, title: &str, shell: Option<String>) {
        match terminal::local::spawn(title, shell, 24, 80) {
            Ok(sess) => {
                self.tabs.push(sess);
                self.active = Some(self.tabs.len() - 1);
                self.status = format!("Запущен {title}");
            }
            Err(e) => self.status = format!("Ошибка запуска: {e}"),
        }
    }

    #[cfg(feature = "ssh")]
    fn open_ssh(&mut self, form: &SshForm) {
        let port: u16 = form.port.trim().parse().unwrap_or(22);
        let params = terminal::ssh::SshParams {
            host: form.host.clone(),
            port,
            user: form.user.clone(),
            password: form.password.clone(),
        };
        let title = if form.name.is_empty() {
            format!("{}@{}", form.user, form.host)
        } else {
            form.name.clone()
        };
        let sess = terminal::ssh::spawn(&title, params, 24, 80);
        self.tabs.push(sess);
        self.active = Some(self.tabs.len() - 1);
        self.status = format!("SSH → {}:{}", form.host, port);

        if form.save && !self.cfg.sessions.iter().any(|s| matches!(&s.kind,
            SessionKind::Ssh { host, user, .. } if host == &form.host && user == &form.user))
        {
            let id = self.cfg.alloc_id();
            self.cfg.sessions.push(SessionDef {
                id,
                name: title,
                group: "SSH".into(),
                kind: SessionKind::Ssh { host: form.host.clone(), port, user: form.user.clone() },
            });
            self.cfg.save();
        }
    }

    #[cfg(not(feature = "ssh"))]
    fn open_ssh(&mut self, _form: &SshForm) {
        self.status = "SSH отключён при сборке".into();
    }

    /// Открыть сохранённую сессию из дерева.
    fn open_def(&mut self, def: &SessionDef) {
        match &def.kind {
            SessionKind::Local { shell } => self.open_local(&def.name, shell.clone()),
            SessionKind::Ssh { host, port, user } => {
                self.ssh_form = Some(SshForm::from_def(&def.name, host, user, *port));
            }
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.tabs.remove(idx);
            if self.tabs.is_empty() {
                self.active = None;
            } else {
                self.active = Some(idx.min(self.tabs.len() - 1));
            }
        }
    }

    /// Разобрать строку быстрого подключения: [ssh://]user@host[:port].
    fn parse_conn(s: &str) -> Option<(String, String, u16)> {
        let s = s.trim();
        let s = s.strip_prefix("ssh://").unwrap_or(s);
        let (user, rest) = s.split_once('@')?;
        let (host, port) = match rest.split_once(':') {
            Some((h, p)) => (h, p.parse().ok()?),
            None => (rest, 22u16),
        };
        if host.is_empty() || user.is_empty() {
            return None;
        }
        Some((user.to_string(), host.to_string(), port))
    }

    // ── панели UI ─────────────────────────────────────────

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("▸_ MobaRust").strong().size(15.0));
            ui.add_space(6.0);

            // быстрое подключение
            let hint = egui::RichText::new("user@host:port — Enter").weak();
            let edit = egui::TextEdit::singleline(&mut self.omnibox)
                .hint_text(hint)
                .desired_width(320.0)
                .font(egui::TextStyle::Monospace);
            let resp = ui.add(edit);
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Some((user, host, port)) = Self::parse_conn(&self.omnibox) {
                    self.ssh_form = Some(SshForm::from_def("", &host, &user, port));
                    self.omnibox.clear();
                } else {
                    self.status = "Неверный формат. Пример: deploy@10.0.0.1:22".into();
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("⚙").on_hover_text("Настройки").clicked() {}
                if ui.button("MultiExec").on_hover_text("Ввод во все вкладки (скоро)").clicked() {}
                if ui.button("＋ SSH").clicked() {
                    self.ssh_form = Some(SshForm::from_def("", "", "", 22));
                }
                if ui
                    .add(egui::Button::new("＋ Локальный шелл").fill(egui::Color32::from_rgb(0x4a, 0xa8, 0xff)))
                    .clicked()
                {
                    self.open_local("bash · local", None);
                }
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(egui::RichText::new("СЕССИИ").weak().small());
        ui.separator();

        // Группировка по полю group.
        let mut groups: BTreeMap<String, Vec<SessionDef>> = BTreeMap::new();
        for s in &self.cfg.sessions {
            groups.entry(s.group.clone()).or_default().push(s.clone());
        }

        let mut to_open: Option<SessionDef> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (group, items) in &groups {
                egui::CollapsingHeader::new(egui::RichText::new(format!("📁 {group}")).strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        for def in items {
                            let label = format!("  {}", def.name);
                            let resp = ui
                                .selectable_label(false, label)
                                .on_hover_text(def.host_label());
                            if resp.double_clicked() || resp.clicked() {
                                to_open = Some(def.clone());
                            }
                        }
                    });
            }
        });
        if let Some(def) = to_open {
            self.open_def(&def);
        }

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            ui.separator();
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.side_tab, SideTab::Sessions, "Сессии");
                ui.selectable_value(&mut self.side_tab, SideTab::Tools, "Инструменты");
                ui.selectable_value(&mut self.side_tab, SideTab::Macros, "Макросы");
            });
        });
    }

    fn tab_strip(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut close: Option<usize> = None;
            for i in 0..self.tabs.len() {
                let alive = self.tabs[i].is_alive();
                let dot = if alive { "🟢" } else { "⚫" };
                let title = self.tabs[i].title.clone();
                let selected = self.active == Some(i);
                if ui
                    .selectable_label(selected, format!("{dot} {title}"))
                    .clicked()
                {
                    self.active = Some(i);
                }
                if ui.small_button("✕").clicked() {
                    close = Some(i);
                }
                ui.separator();
            }
            if let Some(i) = close {
                self.close_tab(i);
            }
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (proto, info) = match self.active.and_then(|i| self.tabs.get(i)) {
                Some(s) if s.kind_label == "ssh" => ("● SSH", format!("{}×{}", s.cols, s.rows)),
                Some(s) => ("● LOCAL", format!("{}×{}", s.cols, s.rows)),
                None => ("○", "—".into()),
            };
            ui.colored_label(egui::Color32::from_rgb(0x4e, 0xc9, 0xa3), proto);
            ui.separator();
            ui.monospace(info);
            ui.separator();
            ui.monospace("UTF-8");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(&self.status);
            });
        });
    }

    fn ssh_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.ssh_form.is_some();
        let mut do_connect = false;
        let mut do_cancel = false;

        if let Some(form) = self.ssh_form.as_mut() {
            egui::Window::new("Подключение по SSH")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    egui::Grid::new("ssh_grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        ui.label("Имя");
                        ui.text_edit_singleline(&mut form.name);
                        ui.end_row();
                        ui.label("Хост");
                        ui.text_edit_singleline(&mut form.host);
                        ui.end_row();
                        ui.label("Пользователь");
                        ui.text_edit_singleline(&mut form.user);
                        ui.end_row();
                        ui.label("Порт");
                        ui.text_edit_singleline(&mut form.port);
                        ui.end_row();
                        ui.label("Пароль");
                        ui.add(egui::TextEdit::singleline(&mut form.password).password(true));
                        ui.end_row();
                    });
                    ui.checkbox(&mut form.save, "Сохранить сессию (без пароля)");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Подключиться").clicked() {
                            do_connect = true;
                        }
                        if ui.button("Отмена").clicked() {
                            do_cancel = true;
                        }
                    });
                });
        }

        if do_connect {
            if let Some(form) = self.ssh_form.take() {
                self.open_ssh(&form);
            }
        } else if do_cancel || !open {
            self.ssh_form = None;
        }
    }

    /// Собрать ввод с клавиатуры и отправить в активный терминал.
    fn pump_input(&mut self, ctx: &egui::Context) {
        let Some(i) = self.active else { return };
        if ctx.egui_wants_keyboard_input() {
            return; // фокус в поле ввода UI — не перехватываем
        }
        if self.ssh_form.is_some() {
            return;
        }
        let mut out: Vec<u8> = Vec::new();
        ctx.input(|input| {
            for ev in &input.events {
                match ev {
                    egui::Event::Text(t) if !t.is_empty() => out.extend_from_slice(t.as_bytes()),
                    egui::Event::Paste(t) => out.extend_from_slice(t.as_bytes()),
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        if modifiers.ctrl && !modifiers.alt {
                            if let Some(b) = terminal::ctrl_byte(*key) {
                                out.push(b);
                                continue;
                            }
                        }
                        if let Some(bytes) = terminal::encode_key(*key, *modifiers) {
                            out.extend_from_slice(&bytes);
                        }
                    }
                    _ => {}
                }
            }
        });
        if !out.is_empty() {
            if let Some(s) = self.tabs.get(i) {
                s.write_input(&out);
            }
        }
    }
}

impl eframe::App for MobaApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump_input(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(3.0);
            self.toolbar(ui);
            ui.add_space(3.0);
        });

        egui::Panel::bottom("status").show(ui, |ui| {
            self.status_bar(ui);
        });

        egui::Panel::left("sessions")
            .resizable(true)
            .default_size(250.0)
            .min_size(180.0)
            .max_size(400.0)
            .show(ui, |ui| {
                self.sidebar(ui);
            });

        egui::CentralPanel::default().show(ui, |ui| {
            self.tab_strip(ui);
            ui.separator();
            match self.active {
                Some(i) if i < self.tabs.len() => {
                    let font = self.cfg.font_size;
                    terminal::draw_terminal(ui, &mut self.tabs[i], font);
                }
                _ => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);
                        ui.heading("MobaRust");
                        ui.label("Откройте локальный шелл или подключитесь по SSH.");
                        ui.add_space(8.0);
                        if ui.button("＋ Локальный шелл").clicked() {
                            self.open_local("bash · local", None);
                        }
                    });
                }
            }
        });

        self.ssh_dialog(&ctx);

        // Перерисовка: если есть живой вывод — чаще, иначе реже (мигание курсора).
        let any_dirty = self.tabs.iter().any(|t| t.take_dirty());
        if any_dirty {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
    }
}

/// Тёмная тема «терминального IDE».
fn install_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(0x16, 0x1b, 0x22);
    visuals.window_fill = egui::Color32::from_rgb(0x16, 0x1b, 0x22);
    visuals.extreme_bg_color = egui::Color32::from_rgb(0x0a, 0x0e, 0x14);
    visuals.override_text_color = Some(egui::Color32::from_rgb(0xd7, 0xde, 0xe8));
    visuals.selection.bg_fill = egui::Color32::from_rgb(0x1e, 0x3a, 0x5f);
    ctx.set_visuals(visuals);
}

//! UI приложения на egui: тулбар, менеджер сессий с папками, вкладки
//! терминалов, статус-бар и диалог подключения (пароль / ключ).

use crate::config::{AuthMethod, Config, SessionDef, SessionKind};
use crate::terminal::{self, TermSession};
use std::collections::BTreeMap;

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
    folder: String,
    use_key: bool,
    key_path: String,
    password: String,
    passphrase: String,
}

impl SshForm {
    fn empty() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            user: String::new(),
            port: "22".into(),
            folder: "SSH".into(),
            use_key: false,
            key_path: default_key_path(),
            password: String::new(),
            passphrase: String::new(),
        }
    }

    fn quick(user: &str, host: &str, port: u16) -> Self {
        Self {
            user: user.into(),
            host: host.into(),
            port: port.to_string(),
            ..Self::empty()
        }
    }

    fn from_def(def: &SessionDef) -> Self {
        let mut f = Self::empty();
        f.name = def.name.clone();
        f.folder = def.group.clone();
        if let SessionKind::Ssh { host, port, user, auth } = &def.kind {
            f.host = host.clone();
            f.user = user.clone();
            f.port = port.to_string();
            if let AuthMethod::Key { path } = auth {
                f.use_key = true;
                f.key_path = path.clone();
            }
        }
        f
    }
}

fn default_key_path() -> String {
    directories::UserDirs::new()
        .and_then(|d| {
            let p = d.home_dir().join(".ssh").join("id_ed25519");
            p.to_str().map(|s| s.to_string())
        })
        .unwrap_or_default()
}

pub struct MobaApp {
    cfg: Config,
    tabs: Vec<TermSession>,
    active: Option<usize>,
    omnibox: String,
    side_tab: SideTab,
    ssh_form: Option<SshForm>,
    new_folder: Option<String>,
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
            new_folder: None,
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
        let auth = if form.use_key {
            terminal::ssh::SshAuth::Key {
                path: form.key_path.clone(),
                passphrase: form.passphrase.clone(),
            }
        } else {
            terminal::ssh::SshAuth::Password(form.password.clone())
        };
        let params = terminal::ssh::SshParams {
            host: form.host.clone(),
            port,
            user: form.user.clone(),
            auth,
        };
        let title = if form.name.trim().is_empty() {
            format!("{}@{}", form.user, form.host)
        } else {
            form.name.trim().to_string()
        };
        let sess = terminal::ssh::spawn(&title, params, 24, 80);
        self.tabs.push(sess);
        self.active = Some(self.tabs.len() - 1);
        self.status = format!("SSH → {}:{}", form.host, port);

        // Авто-сохранение сессии (без секретов).
        let folder = if form.folder.trim().is_empty() { "SSH" } else { form.folder.trim() };
        let auth_persist = if form.use_key {
            AuthMethod::Key { path: form.key_path.clone() }
        } else {
            AuthMethod::Password
        };
        self.cfg.upsert_ssh(&title, folder, &form.host, port, &form.user, auth_persist);
        self.cfg.save();
    }

    #[cfg(not(feature = "ssh"))]
    fn open_ssh(&mut self, _form: &SshForm) {
        self.status = "SSH отключён при сборке".into();
    }

    fn open_def(&mut self, def: &SessionDef) {
        match &def.kind {
            SessionKind::Local { shell } => self.open_local(&def.name, shell.clone()),
            SessionKind::Ssh { .. } => {
                self.ssh_form = Some(SshForm::from_def(def));
            }
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.tabs.remove(idx);
            self.active = if self.tabs.is_empty() {
                None
            } else {
                Some(idx.min(self.tabs.len() - 1))
            };
        }
    }

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

            let edit = egui::TextEdit::singleline(&mut self.omnibox)
                .hint_text(egui::RichText::new("user@host:port — Enter").weak())
                .desired_width(320.0)
                .font(egui::TextStyle::Monospace);
            let resp = ui.add(edit);
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if let Some((user, host, port)) = Self::parse_conn(&self.omnibox) {
                    self.ssh_form = Some(SshForm::quick(&user, &host, port));
                    self.omnibox.clear();
                } else {
                    self.status = "Неверный формат. Пример: deploy@10.0.0.1:22".into();
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("⚙").on_hover_text("Настройки").clicked() {}
                if ui.button("MultiExec").on_hover_text("Ввод во все вкладки (скоро)").clicked() {}
                if ui.button("＋ SSH").clicked() {
                    self.ssh_form = Some(SshForm::empty());
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
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("СЕССИИ").weak().small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("＋ Папка").on_hover_text("Новая папка").clicked() {
                    self.new_folder = Some(String::new());
                }
            });
        });

        // Инлайн-ввод имени новой папки.
        if let Some(name) = self.new_folder.as_mut() {
            let mut commit = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(name).hint_text("имя папки").desired_width(140.0),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit = true;
                }
                if ui.small_button("✓").clicked() {
                    commit = true;
                }
                if ui.small_button("✕").clicked() {
                    cancel = true;
                }
            });
            let folder_name = name.trim().to_string();
            if commit && !folder_name.is_empty() {
                self.cfg.ensure_folder(&folder_name);
                self.cfg.save();
                self.new_folder = None;
            } else if cancel {
                self.new_folder = None;
            }
        }
        ui.separator();

        // Группировка сессий по папкам (показываем и пустые папки).
        let mut groups: BTreeMap<String, Vec<SessionDef>> = BTreeMap::new();
        for f in self.cfg.all_folders() {
            groups.entry(f).or_default();
        }
        for s in &self.cfg.sessions {
            groups.entry(s.group.clone()).or_default().push(s.clone());
        }

        let mut to_open: Option<SessionDef> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (group, items) in &groups {
                egui::CollapsingHeader::new(egui::RichText::new(format!("📁 {group}")).strong())
                    .default_open(true)
                    .show(ui, |ui| {
                        if items.is_empty() {
                            ui.label(egui::RichText::new("  пусто").weak().small());
                        }
                        for def in items {
                            let resp = ui
                                .selectable_label(false, format!("  {}", def.name))
                                .on_hover_text(def.host_label());
                            if resp.clicked() || resp.double_clicked() {
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
                let dot = if self.tabs[i].is_alive() { "🟢" } else { "⚫" };
                let title = self.tabs[i].title.clone();
                let selected = self.active == Some(i);
                if ui.selectable_label(selected, format!("{dot} {title}")).clicked() {
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
        if self.ssh_form.is_none() {
            return;
        }
        let folders = self.cfg.all_folders();
        let mut open = true;
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
                        ui.label("Папка");
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt("folder_combo")
                                .selected_text(form.folder.clone())
                                .show_ui(ui, |ui| {
                                    for f in &folders {
                                        ui.selectable_value(&mut form.folder, f.clone(), f);
                                    }
                                });
                            ui.add(
                                egui::TextEdit::singleline(&mut form.folder)
                                    .desired_width(110.0)
                                    .hint_text("или новая"),
                            );
                        });
                        ui.end_row();
                    });

                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Аутентификация:");
                        ui.radio_value(&mut form.use_key, false, "Пароль");
                        ui.radio_value(&mut form.use_key, true, "Ключ");
                    });

                    egui::Grid::new("auth_grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        if form.use_key {
                            ui.label("Файл ключа");
                            ui.text_edit_singleline(&mut form.key_path);
                            ui.end_row();
                            ui.label("Passphrase");
                            ui.add(egui::TextEdit::singleline(&mut form.passphrase).password(true));
                            ui.end_row();
                        } else {
                            ui.label("Пароль");
                            ui.add(egui::TextEdit::singleline(&mut form.password).password(true));
                            ui.end_row();
                        }
                    });

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

    fn pump_input(&mut self, ctx: &egui::Context) {
        let Some(i) = self.active else { return };
        if ctx.egui_wants_keyboard_input() || self.ssh_form.is_some() || self.new_folder.is_some() {
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

        // Перерисовка: по готовности вывода — сразу; иначе редко (мигание курсора).
        let any_dirty = self.tabs.iter().any(|t| t.take_dirty());
        if any_dirty {
            ctx.request_repaint();
        } else if self.active.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }
}

fn install_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::from_rgb(0x16, 0x1b, 0x22);
    visuals.window_fill = egui::Color32::from_rgb(0x16, 0x1b, 0x22);
    visuals.extreme_bg_color = egui::Color32::from_rgb(0x0a, 0x0e, 0x14);
    visuals.override_text_color = Some(egui::Color32::from_rgb(0xd7, 0xde, 0xe8));
    visuals.selection.bg_fill = egui::Color32::from_rgb(0x1e, 0x3a, 0x5f);
    ctx.set_visuals(visuals);
}

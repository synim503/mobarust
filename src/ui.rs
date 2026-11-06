//! Отрисовка интерфейса. Принципы: всё видно и доступно с клавиатуры,
//! состояние подключения всегда на виду, опасные действия — с подтверждением,
//! пустые состояния подсказывают следующий шаг.

use crate::app::{Action, Editor, Layout, MobaApp, Page};
use crate::config::{SessionDef, Snippet};
use crate::terminal::{self, DrawOpts};
use egui::{Color32, RichText};

pub const BG: Color32 = Color32::from_rgb(0x0d, 0x11, 0x17);
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x1b, 0x22);
pub const PANEL2: Color32 = Color32::from_rgb(0x1c, 0x23, 0x30);
pub const FIELD: Color32 = Color32::from_rgb(0x0a, 0x0e, 0x14);
pub const FG: Color32 = Color32::from_rgb(0xd7, 0xde, 0xe8);
pub const MUTED: Color32 = Color32::from_rgb(0x8b, 0x97, 0xa7);
pub const ACCENT: Color32 = Color32::from_rgb(0x4a, 0xa8, 0xff);
pub const GREEN: Color32 = Color32::from_rgb(0x4e, 0xc9, 0xa3);
pub const YELLOW: Color32 = Color32::from_rgb(0xe5, 0xc0, 0x7b);
pub const RED: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);

const SWATCHES: [Option<[u8; 3]>; 7] = [
    None,
    Some([0xe0, 0x6c, 0x75]),
    Some([0xe5, 0x9a, 0x50]),
    Some([0xe5, 0xc0, 0x7b]),
    Some([0x4e, 0xc9, 0xa3]),
    Some([0x4a, 0xa8, 0xff]),
    Some([0xc6, 0x78, 0xdd]),
];

pub fn install_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = FIELD;
    v.faint_bg_color = PANEL2;
    v.override_text_color = Some(FG);
    v.selection.bg_fill = Color32::from_rgb(0x1e, 0x3a, 0x5f);
    v.hyperlink_color = ACCENT;
    v.window_corner_radius = 8.into();
    ctx.set_visuals(v);
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(6.0, 5.0);
        s.spacing.button_padding = egui::vec2(8.0, 4.0);
        s.interaction.tooltip_delay = 0.35;
    });
}

pub fn draw(app: &mut MobaApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();

    egui::Panel::top("toolbar").show(ui, |ui| {
        ui.add_space(4.0);
        toolbar(app, ui, &ctx);
        ui.add_space(4.0);
    });
    egui::Panel::bottom("status").show(ui, |ui| status_bar(app, ui));
    egui::Panel::left("sessions")
        .resizable(true)
        .default_size(250.0)
        .min_size(180.0)
        .max_size(440.0)
        .show(ui, |ui| sidebar(app, ui, &ctx));

    #[cfg(feature = "ssh")]
    if app.show_sftp && app.page == Page::Terminal && app.active_tab().is_some_and(|t| t.is_ssh()) {
        egui::Panel::right("sftp")
            .resizable(true)
            .default_size(300.0)
            .min_size(220.0)
            .max_size(640.0)
            .show(ui, |ui| sftp_panel(app, ui, &ctx));
    }

    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(BG))
        .show(ui, |ui| match app.page {
            Page::Settings => settings_page(app, ui),
            Page::Terminal => {
                tab_strip(app, ui, &ctx);
                terminal_area(app, ui, &ctx);
            }
        });

    palette(app, &ctx);
    editor(app, &ctx);
    paste_confirm(app, &ctx);
    confirmations(app, &ctx);
    #[cfg(feature = "ssh")]
    prompts(app, &ctx);
}

// ───────────────────────── тулбар ─────────────────────────

fn toolbar(app: &mut MobaApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("▸_ MobaRust").strong().size(15.0).color(ACCENT));
        ui.add_space(10.0);
        let quick = ui
            .add_sized(
                [320.0, 26.0],
                egui::Button::new(RichText::new("🔍  Подключиться или найти…        Ctrl+Shift+K").color(MUTED)).fill(FIELD),
            )
            .on_hover_text("Нечёткий поиск по сессиям и командам, или введите user@host:port");
        if quick.clicked() {
            app.run_action(ctx, Action::Palette);
        }
        ui.add_space(6.0);
        if ui.button("＋ SSH").on_hover_text("Новая SSH-сессия  Ctrl+Shift+N").clicked() {
            app.run_action(ctx, Action::NewSsh);
        }
        local_menu(app, ui, ctx, "＋ Локальный ▾");
        ui.separator();

        let lay = match app.layout {
            Layout::Single => "▣ Один",
            Layout::Cols2 => "◫ 2 колонки",
            Layout::Rows2 => "⊟ 2 строки",
            Layout::Grid4 => "⊞ 4 панели",
        };
        ui.menu_button(lay, |ui| {
            for (l, name) in [
                (Layout::Single, "▣ Один терминал"),
                (Layout::Cols2, "◫ Две колонки"),
                (Layout::Rows2, "⊟ Две строки"),
                (Layout::Grid4, "⊞ Четыре панели"),
            ] {
                if ui.radio(app.layout == l, name).clicked() {
                    app.set_layout(l);
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Разделить окно на несколько терминалов");

        let b = egui::Button::new(RichText::new("📢 Рассылка").color(if app.broadcast { Color32::WHITE } else { FG }))
            .fill(if app.broadcast { RED.gamma_multiply(0.7) } else { PANEL2 });
        if ui
            .add(b)
            .on_hover_text("Ввод сразу во все вкладки (MultiExec)  Ctrl+Shift+B.\nВкладку можно исключить в её меню.")
            .clicked()
        {
            app.run_action(ctx, Action::Broadcast);
        }
        if ui
            .add(egui::Button::new("📁 SFTP").selected(app.show_sftp))
            .on_hover_text("Панель файлов текущего сервера  Ctrl+Shift+E")
            .clicked()
        {
            app.show_sftp = !app.show_sftp;
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new("⚙").selected(app.page == Page::Settings))
                .on_hover_text("Настройки  Ctrl+Shift+,")
                .clicked()
            {
                app.run_action(ctx, Action::Settings);
            }
        });
    });
}

fn local_menu(app: &mut MobaApp, ui: &mut egui::Ui, ctx: &egui::Context, label: &str) {
    let shells = app.shells.clone();
    ui.menu_button(label, |ui| {
        for s in &shells {
            if ui.button(&s.label).clicked() {
                app.open_local(ctx, &s.label, Some(s.command.clone()));
                ui.close();
            }
        }
    })
    .response
    .on_hover_text("Локальный shell  Ctrl+Shift+T");
}

// ───────────────────────── менеджер сессий ─────────────────────────

fn sidebar(app: &mut MobaApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("СЕССИИ").small().color(MUTED).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("＋").on_hover_text("Новая SSH-сессия").clicked() {
                app.editor = Some(Editor::new("SSH"));
            }
            if ui.small_button("⤓").on_hover_text("Импорт хостов из ~/.ssh/config").clicked() {
                app.import_ssh_config();
            }
            if ui.small_button("📁+").on_hover_text("Новая папка").clicked() {
                app.new_folder = Some(String::new());
            }
        });
    });
    ui.add(
        egui::TextEdit::singleline(&mut app.filter)
            .hint_text("🔍 Фильтр: имя, хост, тег")
            .desired_width(f32::INFINITY),
    );

    if let Some(name) = app.new_folder.as_mut() {
        let mut commit = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(name).hint_text("имя папки").desired_width(140.0));
            r.request_focus();
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                commit = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
            commit |= ui.small_button("✓").clicked();
            cancel |= ui.small_button("✕").clicked();
        });
        let n = name.trim().to_string();
        if commit && !n.is_empty() {
            app.cfg.ensure_folder(&n);
            app.dirty_cfg = true;
            app.new_folder = None;
        } else if cancel {
            app.new_folder = None;
        }
    }
    ui.add_space(2.0);

    let filter = app.filter.trim().to_lowercase();
    let mut act: Option<SideAct> = None;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if app.cfg.sessions.is_empty() {
            ui.add_space(12.0);
            ui.label(RichText::new("Сохранённых сессий пока нет").color(MUTED));
            ui.add_space(6.0);
            if ui.button("＋ Новая SSH-сессия").clicked() {
                act = Some(SideAct::NewIn("SSH".into()));
            }
            if ui.button("⤓ Импорт ~/.ssh/config").clicked() {
                act = Some(SideAct::Import);
            }
            ui.label(RichText::new("Или Ctrl+Shift+K и введите user@host").small().color(MUTED));
        }

        if filter.is_empty() {
            let recent = app.cfg.recent(5);
            if !recent.is_empty() {
                egui::CollapsingHeader::new(RichText::new("🕘 Недавние").color(MUTED))
                    .id_salt("recent")
                    .default_open(true)
                    .show(ui, |ui| {
                        for d in &recent {
                            session_row(app, ui, d, &mut act);
                        }
                    });
            }
        }

        let folders = app.cfg.all_folders();
        for folder in folders {
            let items: Vec<SessionDef> = app
                .cfg
                .sessions
                .iter()
                .filter(|s| s.group == folder)
                .filter(|s| filter.is_empty() || fuzzy(&filter, &s.haystack()).is_some())
                .cloned()
                .collect();
            if !filter.is_empty() && items.is_empty() {
                continue;
            }
            let mut sorted = items;
            sorted.sort_by_key(|a| a.name.to_lowercase());
            let header = egui::CollapsingHeader::new(RichText::new(format!("📁 {folder}  ")).strong())
                .id_salt(("folder", &folder))
                .default_open(true)
                .open(if filter.is_empty() { None } else { Some(true) })
                .show(ui, |ui| {
                    if sorted.is_empty() {
                        ui.label(RichText::new("пусто").small().color(MUTED));
                    }
                    for d in &sorted {
                        session_row(app, ui, d, &mut act);
                    }
                });
            header.header_response.context_menu(|ui| {
                if ui.button("▶ Открыть все сессии").clicked() {
                    act = Some(SideAct::OpenAll(folder.clone()));
                    ui.close();
                }
                if ui.button("＋ Новая сессия здесь").clicked() {
                    act = Some(SideAct::NewIn(folder.clone()));
                    ui.close();
                }
                if ui.button("✏ Переименовать…").clicked() {
                    act = Some(SideAct::RenameFolder(folder.clone()));
                    ui.close();
                }
                if ui.button("🗑 Удалить папку…").clicked() {
                    act = Some(SideAct::DeleteFolder(folder.clone()));
                    ui.close();
                }
            });
        }
    });

    match act {
        Some(SideAct::Connect(id)) => app.connect(ctx, id),
        Some(SideAct::Edit(id)) => {
            if let Some(d) = app.cfg.find(id) {
                app.editor = Some(Editor::from_def(d));
            }
        }
        Some(SideAct::Clone(id)) => {
            if let Some(d) = app.cfg.find(id) {
                let mut e = Editor::from_def(d);
                e.id = 0;
                e.name = format!("{} (копия)", e.name);
                app.editor = Some(e);
            }
        }
        Some(SideAct::Move(id, f)) => {
            if let Some(d) = app.cfg.find_mut(id) {
                d.group = f;
                app.dirty_cfg = true;
            }
        }
        Some(SideAct::Delete(id)) => app.confirm_delete = Some(id),
        Some(SideAct::OpenAll(f)) => {
            let ids: Vec<u64> = app.cfg.sessions.iter().filter(|s| s.group == f).map(|s| s.id).collect();
            for id in ids {
                app.connect(ctx, id);
            }
        }
        Some(SideAct::NewIn(f)) => app.editor = Some(Editor::new(&f)),
        Some(SideAct::RenameFolder(f)) => app.rename_folder = Some((f.clone(), f)),
        Some(SideAct::DeleteFolder(f)) => app.confirm_folder_delete = Some(f),
        Some(SideAct::Import) => app.import_ssh_config(),
        Some(SideAct::Select(id)) => app.selected_session = Some(id),
        None => {}
    }
}

enum SideAct {
    Connect(u64),
    Edit(u64),
    Clone(u64),
    Move(u64, String),
    Delete(u64),
    OpenAll(String),
    NewIn(String),
    RenameFolder(String),
    DeleteFolder(String),
    Import,
    Select(u64),
}

fn session_row(app: &MobaApp, ui: &mut egui::Ui, d: &SessionDef, act: &mut Option<SideAct>) {
    let selected = app.selected_session == Some(d.id);
    let open = app.tabs.iter().any(|t| t.def_id == Some(d.id));
    let dot_color = d.color.map(|[r, g, b]| Color32::from_rgb(r, g, b)).unwrap_or(if open { GREEN } else { MUTED });
    let resp = ui
        .horizontal(|ui| {
            ui.label(RichText::new(if open { "●" } else { "○" }).color(dot_color));
            ui.selectable_label(selected, &d.name)
        })
        .inner;
    let resp = resp.on_hover_text(format!("{}\n\nДвойной щелчок — подключиться", d.host_label()));
    if resp.clicked() {
        *act = Some(SideAct::Select(d.id));
    }
    if resp.double_clicked() {
        *act = Some(SideAct::Connect(d.id));
    }
    let folders = app.cfg.all_folders();
    resp.context_menu(|ui| {
        if ui.button("▶ Подключиться").clicked() {
            *act = Some(SideAct::Connect(d.id));
            ui.close();
        }
        if ui.button("✏ Изменить…").clicked() {
            *act = Some(SideAct::Edit(d.id));
            ui.close();
        }
        if ui.button("⧉ Дублировать…").clicked() {
            *act = Some(SideAct::Clone(d.id));
            ui.close();
        }
        ui.menu_button("📁 Переместить в", |ui| {
            for f in &folders {
                if ui.add_enabled(f != &d.group, egui::Button::new(f)).clicked() {
                    *act = Some(SideAct::Move(d.id, f.clone()));
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button(RichText::new("🗑 Удалить…").color(RED)).clicked() {
            *act = Some(SideAct::Delete(d.id));
            ui.close();
        }
    });
}

// ───────────────────────── вкладки ─────────────────────────

fn tab_state_color(t: &terminal::TermSession) -> Color32 {
    #[cfg(feature = "ssh")]
    if let Some(s) = &t.ssh {
        use terminal::ssh::ConnState::*;
        return match s.state() {
            Connected => GREEN,
            Connecting(_) | Authenticating(_) => YELLOW,
            Disconnected(_) | Blocked(_) => RED,
        };
    }
    if t.is_alive() { GREEN } else { MUTED }
}

fn tab_strip(app: &mut MobaApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    if app.tabs.is_empty() {
        return;
    }
    enum TabAct {
        Activate(usize),
        Close(usize),
        CloseOthers(usize),
        Duplicate(usize),
        Reconnect(usize),
        Exclude(usize),
    }
    let mut act: Option<TabAct> = None;
    egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(4, 3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            egui::ScrollArea::horizontal().max_width(ui.available_width() - 40.0).show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, t) in app.tabs.iter().enumerate() {
                        let active = app.active == Some(i);
                        let mut title: String = t.title.chars().take(28).collect();
                        if t.has_activity() && !active {
                            title.push_str(" •");
                        }
                        #[cfg(feature = "ssh")]
                        if t.ssh.as_ref().is_some_and(|s| s.has_prompt()) {
                            title.push_str(" ⚠");
                        }
                        if app.broadcast && !t.broadcast_excluded {
                            title = format!("📢 {title}");
                        }
                        let color = t.color.map(|[r, g, b]| Color32::from_rgb(r, g, b));
                        ui.label(RichText::new("●").color(tab_state_color(t)));
                        let mut rt = RichText::new(title);
                        if let Some(c) = color {
                            rt = rt.color(c);
                        }
                        let r = ui.selectable_label(active, rt);
                        let r = r.on_hover_text(format!("{}\nСредняя кнопка — закрыть, правая — меню", t.title));
                        if r.clicked() {
                            act = Some(TabAct::Activate(i));
                        }
                        if r.middle_clicked() {
                            act = Some(TabAct::Close(i));
                        }
                        r.context_menu(|ui| {
                            if ui.button("⟳ Переподключить").clicked() {
                                act = Some(TabAct::Reconnect(i));
                                ui.close();
                            }
                            if ui.button("⧉ Дублировать").clicked() {
                                act = Some(TabAct::Duplicate(i));
                                ui.close();
                            }
                            if ui.checkbox(&mut t.broadcast_excluded.clone(), "Исключить из рассылки").clicked() {
                                act = Some(TabAct::Exclude(i));
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Закрыть другие").clicked() {
                                act = Some(TabAct::CloseOthers(i));
                                ui.close();
                            }
                            if ui.button("✕ Закрыть   Ctrl+Shift+W").clicked() {
                                act = Some(TabAct::Close(i));
                                ui.close();
                            }
                        });
                        if ui.small_button("✕").on_hover_text("Закрыть вкладку").clicked() {
                            act = Some(TabAct::Close(i));
                        }
                        ui.add_space(6.0);
                    }
                });
            });
            let shells = app.shells.clone();
            ui.menu_button("＋", |ui| {
                if ui.button("SSH-сессия…").clicked() {
                    app.run_action(ctx, Action::NewSsh);
                    ui.close();
                }
                if ui.button("Из сохранённых…  Ctrl+Shift+K").clicked() {
                    app.run_action(ctx, Action::Palette);
                    ui.close();
                }
                ui.separator();
                for s in &shells {
                    if ui.button(&s.label).clicked() {
                        app.open_local(ctx, &s.label, Some(s.command.clone()));
                        ui.close();
                    }
                }
            });
        });
    });
    match act {
        Some(TabAct::Activate(i)) => app.set_active(i),
        Some(TabAct::Close(i)) => app.close_tab(i),
        Some(TabAct::CloseOthers(i)) => app.close_others(i),
        Some(TabAct::Duplicate(i)) => app.duplicate(ctx, i),
        Some(TabAct::Reconnect(i)) => app.reconnect(i),
        Some(TabAct::Exclude(i)) => {
            let t = &mut app.tabs[i];
            t.broadcast_excluded = !t.broadcast_excluded;
        }
        None => {}
    }
}

// ───────────────────────── терминалы ─────────────────────────

fn terminal_area(app: &mut MobaApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    if app.tabs.is_empty() {
        welcome(app, ui, ctx);
        return;
    }
    if app.active.is_none() {
        app.set_active(0);
    }

    // Поиск по истории (TRM-04).
    if let Some(ai) = app.active {
        if app.tabs[ai].search.open {
            search_bar(app, ui, ai);
        }
    }

    let area = ui.available_rect_before_wrap();
    let panes: Vec<usize> = if app.layout == Layout::Single {
        vec![app.active.unwrap_or(0)]
    } else {
        if app.panes.is_empty() {
            app.set_layout(app.layout);
        }
        app.panes.clone()
    };
    let rects = split(area, app.layout, panes.len());
    for (k, ti) in panes.into_iter().enumerate() {
        if ti >= app.tabs.len() {
            continue;
        }
        let rect = rects[k];
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect).id_salt(("pane", ti)), |ui| {
            pane(app, ui, ctx, ti);
        });
    }
    ui.allocate_rect(area, egui::Sense::hover());
}

fn split(r: egui::Rect, l: Layout, n: usize) -> Vec<egui::Rect> {
    let g = 3.0;
    let (w, h) = (r.width(), r.height());
    let lt = r.left_top();
    let rect = |x: f32, y: f32, ww: f32, hh: f32| egui::Rect::from_min_size(lt + egui::vec2(x, y), egui::vec2(ww, hh));
    match (l, n) {
        (_, 0) | (_, 1) | (Layout::Single, _) => vec![r],
        (Layout::Cols2, _) => vec![rect(0.0, 0.0, w / 2.0 - g, h), rect(w / 2.0 + g, 0.0, w / 2.0 - g, h)],
        (Layout::Rows2, _) => vec![rect(0.0, 0.0, w, h / 2.0 - g), rect(0.0, h / 2.0 + g, w, h / 2.0 - g)],
        (Layout::Grid4, 2) => vec![rect(0.0, 0.0, w / 2.0 - g, h), rect(w / 2.0 + g, 0.0, w / 2.0 - g, h)],
        (Layout::Grid4, 3) => vec![
            rect(0.0, 0.0, w / 2.0 - g, h),
            rect(w / 2.0 + g, 0.0, w / 2.0 - g, h / 2.0 - g),
            rect(w / 2.0 + g, h / 2.0 + g, w / 2.0 - g, h / 2.0 - g),
        ],
        (Layout::Grid4, _) => vec![
            rect(0.0, 0.0, w / 2.0 - g, h / 2.0 - g),
            rect(w / 2.0 + g, 0.0, w / 2.0 - g, h / 2.0 - g),
            rect(0.0, h / 2.0 + g, w / 2.0 - g, h / 2.0 - g),
            rect(w / 2.0 + g, h / 2.0 + g, w / 2.0 - g, h / 2.0 - g),
        ],
    }
}

fn pane(app: &mut MobaApp, ui: &mut egui::Ui, ctx: &egui::Context, ti: usize) {
    let focused = app.active == Some(ti) && app.page == Page::Terminal;
    let opts = DrawOpts {
        font_size: app.cfg.settings.font_size,
        focused,
        blink: app.cfg.settings.cursor_blink,
        copy_on_select: app.cfg.settings.copy_on_select,
        right_click_paste: app.cfg.settings.right_click_paste,
        broadcast: app.broadcast,
    };
    let rect = ui.max_rect();
    let r = terminal::draw_terminal(ui, &mut app.tabs[ti], &opts);
    if r.clicked && app.active != Some(ti) {
        app.set_active(ti);
    }
    if let Some(s) = r.copied {
        ctx.copy_text(s);
        app.toast("Скопировано");
    }
    if r.paste {
        app.set_active(ti);
        if let Some(t) = app.clipboard_text() {
            app.request_paste(t);
        }
    }
    if r.open_search {
        app.tabs[ti].search.open = true;
    }
    if r.reconnect {
        app.reconnect(ti);
    }

    // Состояние подключения поверх терминала.
    #[cfg(feature = "ssh")]
    if let Some(ssh) = app.tabs[ti].ssh.clone() {
        use terminal::ssh::ConnState::*;
        match ssh.state() {
            Connecting(_) | Authenticating(_) => {}
            Disconnected(reason) => {
                let retry = ssh.retry_at.lock().ok().and_then(|r| *r);
                let mut txt = format!("⚠ Отключено: {reason}");
                if let Some(t) = retry {
                    let s = t.saturating_duration_since(std::time::Instant::now()).as_secs();
                    txt.push_str(&format!(" · повтор через {s} с"));
                }
                if bottom_bar(ui, rect, &txt, RED, "⟳ Переподключить (R)") {
                    app.reconnect(ti);
                }
            }
            Blocked(_) => blocked_panel(app, ui, rect, ti, &ssh),
            Connected => {}
        }
    }
    #[cfg(not(feature = "ssh"))]
    let _ = rect;
}

fn bottom_bar(ui: &mut egui::Ui, rect: egui::Rect, text: &str, color: Color32, button: &str) -> bool {
    let r = egui::Rect::from_min_max(egui::pos2(rect.left() + 8.0, rect.bottom() - 40.0), egui::pos2(rect.right() - 8.0, rect.bottom() - 6.0));
    let mut clicked = false;
    ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
        egui::Frame::new()
            .fill(PANEL2)
            .stroke(egui::Stroke::new(1.0, color))
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(10, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(text).color(color));
                    clicked = ui.button(button).clicked();
                });
            });
    });
    clicked
}

#[cfg(feature = "ssh")]
fn blocked_panel(app: &mut MobaApp, ui: &mut egui::Ui, rect: egui::Rect, ti: usize, ssh: &terminal::ssh::SshShared) {
    let info = ssh.blocked.lock().ok().and_then(|b| b.clone());
    let r = egui::Rect::from_min_size(rect.left_top() + egui::vec2(12.0, 12.0), egui::vec2((rect.width() - 24.0).min(620.0), 210.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
        egui::Frame::new()
            .fill(Color32::from_rgb(0x2a, 0x12, 0x16))
            .stroke(egui::Stroke::new(1.5, RED))
            .corner_radius(8)
            .inner_margin(12)
            .show(ui, |ui| {
                ui.label(RichText::new("⛔ Ключ хоста изменился — подключение заблокировано").strong().size(15.0).color(RED));
                ui.add_space(4.0);
                ui.label("Сервер предъявил другой ключ, чем записан в known_hosts. Это бывает после переустановки сервера, но может означать перехват соединения.");
                if let Some(b) = &info {
                    ui.add_space(4.0);
                    ui.monospace(format!("{}:{}   {}\n{}", b.host, b.port, b.algo, b.fingerprint));
                    ui.add_space(6.0);
                    if !app.forget_confirm {
                        if ui.button("Сервер переустанавливали — удалить старую запись…").clicked() {
                            app.forget_confirm = true;
                        }
                    } else {
                        ui.label(RichText::new("Сверьте новый отпечаток с администратором сервера.").color(YELLOW));
                        ui.horizontal(|ui| {
                            if ui.button(RichText::new("Удалить из known_hosts и переподключиться").color(RED)).clicked() {
                                match terminal::ssh::forget_host_key(b.line) {
                                    Ok(()) => {
                                        app.forget_confirm = false;
                                        app.reconnect(ti);
                                        app.toast("Старая запись удалена (копия: known_hosts.old)");
                                    }
                                    Err(e) => app.toast(format!("Не удалось изменить known_hosts: {e}")),
                                }
                            }
                            if ui.button("Отмена").clicked() {
                                app.forget_confirm = false;
                            }
                        });
                    }
                }
            });
    });
}

fn search_bar(app: &mut MobaApp, ui: &mut egui::Ui, ai: usize) {
    let t = &mut app.tabs[ai];
    let mut close = false;
    egui::Frame::new().fill(PANEL2).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("🔍");
            let r = ui.add(egui::TextEdit::singleline(&mut t.search.query).hint_text("Поиск в истории…").desired_width(280.0));
            if t.search.matches.is_empty() && t.search.query.is_empty() {
                r.request_focus();
            }
            if r.changed() {
                t.search.matches.clear();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if enter {
                let back = ui.input(|i| i.modifiers.shift);
                if t.search.matches.is_empty() {
                    t.run_search();
                } else {
                    t.next_match(!back);
                }
                r.request_focus();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            let n = t.search.matches.len();
            ui.label(if n == 0 {
                RichText::new(if t.search.query.is_empty() { "Enter — искать" } else { "нет совпадений / Enter" }).color(MUTED)
            } else {
                RichText::new(format!("{} из {n}", t.search.current + 1)).color(MUTED)
            });
            if ui.small_button("↑").on_hover_text("Предыдущее (Shift+Enter)").clicked() {
                t.next_match(false);
            }
            if ui.small_button("↓").on_hover_text("Следующее (Enter)").clicked() {
                t.next_match(true);
            }
            if ui.small_button("✕").on_hover_text("Закрыть (Esc)").clicked() {
                close = true;
            }
        });
    });
    if close {
        t.search.open = false;
        t.search.matches.clear();
        t.set_scroll(0);
    }
}

fn welcome(app: &mut MobaApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.add_space(60.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("▸_ MobaRust").size(28.0).strong().color(ACCENT));
        ui.label(RichText::new("Быстрый SSH-клиент. Без аккаунта, без телеметрии, без подписки.").color(MUTED));
        ui.add_space(18.0);
        if ui
            .add_sized([300.0, 34.0], egui::Button::new(RichText::new("🔍 Подключиться…   Ctrl+Shift+K").size(15.0)).fill(ACCENT.gamma_multiply(0.35)))
            .clicked()
        {
            app.run_action(ctx, Action::Palette);
        }
        ui.add_space(6.0);
        if ui.add_sized([300.0, 30.0], egui::Button::new("＋ Новая SSH-сессия   Ctrl+Shift+N")).clicked() {
            app.run_action(ctx, Action::NewSsh);
        }
        if ui.add_sized([300.0, 30.0], egui::Button::new("⤓ Импорт хостов из ~/.ssh/config")).clicked() {
            app.import_ssh_config();
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let shells = app.shells.clone();
            ui.add_space((ui.available_width() - 120.0 * shells.len().min(4) as f32).max(0.0) / 2.0);
            for s in shells.iter().take(4) {
                if ui.button(format!("⌨ {}", s.label)).clicked() {
                    app.open_local(ctx, &s.label, Some(s.command.clone()));
                }
            }
        });
        ui.add_space(24.0);
        ui.label(RichText::new("Горячие клавиши").strong());
        ui.horizontal(|ui| {
            ui.add_space(((ui.available_width() - 470.0) / 2.0).max(0.0));
            egui::Grid::new("hk").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                for (k, v) in HOTKEYS.iter().take(8) {
                    ui.monospace(*k);
                    ui.label(RichText::new(*v).color(MUTED));
                    ui.end_row();
                }
            });
        });
    });
}

const HOTKEYS: &[(&str, &str)] = &[
    ("Ctrl+Shift+K / Ctrl+Shift+P", "подключиться, палитра команд"),
    ("Ctrl+Shift+N", "новая SSH-сессия"),
    ("Ctrl+Shift+T", "локальный shell"),
    ("Ctrl+C", "копировать выделение, иначе ^C"),
    ("Ctrl+V / Ctrl+Shift+V", "вставить (с проверкой многострочной)"),
    ("Ctrl+Shift+F", "поиск в истории"),
    ("Ctrl+Shift+B", "рассылка ввода во все вкладки"),
    ("Ctrl+Shift+E", "панель SFTP"),
    ("Ctrl+Shift+R", "переподключить"),
    ("Ctrl+Shift+D", "дублировать вкладку"),
    ("Ctrl+Shift+W", "закрыть вкладку"),
    ("Ctrl+Tab / Ctrl+Shift+Tab", "следующая / предыдущая вкладка"),
    ("Shift+PgUp/PgDn, колесо", "прокрутка истории"),
    ("Ctrl+колесо, Ctrl+=/Ctrl+Shift+-", "размер шрифта"),
    ("Ctrl+F1…F12", "сниппеты"),
    ("Ctrl+Shift+,", "настройки"),
];

// ───────────────────────── SFTP ─────────────────────────

#[cfg(feature = "ssh")]
fn sftp_panel(app: &mut MobaApp, ui: &mut egui::Ui, _ctx: &egui::Context) {
    use terminal::ssh::{ConnState, SftpCmd};
    let Some(ai) = app.active else { return };
    let Some(ssh) = app.tabs[ai].ssh.clone() else { return };
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("SFTP").small().strong().color(MUTED));
        ui.label(RichText::new(&ssh.endpoint).small().color(MUTED));
    });
    if ssh.state() != ConnState::Connected {
        ui.add_space(10.0);
        ui.label(RichText::new("Файлы появятся после подключения").color(MUTED));
        return;
    }

    let mut cmds: Vec<SftpCmd> = Vec::new();
    let downloads = directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(|d| d.to_path_buf()))
        .unwrap_or_else(crate::sshconfig::home);

    let Ok(mut view) = ssh.sftp.lock() else { return };
    if !view.requested {
        view.requested = true;
        cmds.push(SftpCmd::List(".".into()));
    }
    if app.sftp_path_for != view.cwd {
        app.sftp_path_for = view.cwd.clone();
        app.sftp_path = view.cwd.clone();
    }
    let cwd = view.cwd.clone();

    ui.horizontal(|ui| {
        if ui.small_button("⬆").on_hover_text("Вверх").clicked() && !cwd.is_empty() {
            cmds.push(SftpCmd::List(format!("{cwd}/..")));
        }
        if ui.small_button("⟳").on_hover_text("Обновить").clicked() {
            cmds.push(SftpCmd::List(if cwd.is_empty() { ".".into() } else { cwd.clone() }));
        }
        if ui.small_button("📁+").on_hover_text("Новая папка").clicked() {
            app.sftp_mkdir = Some(String::new());
        }
        if ui.small_button("~").on_hover_text("Домашняя папка").clicked() {
            cmds.push(SftpCmd::List(".".into()));
        }
        if ui.small_button("⤓").on_hover_text("Открыть папку «Загрузки»").clicked() {
            reveal(&downloads.join("."));
        }
    });
    let r = ui.add(
        egui::TextEdit::singleline(&mut app.sftp_path)
            .font(egui::TextStyle::Monospace)
            .hint_text("путь — Enter")
            .desired_width(f32::INFINITY),
    );
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        cmds.push(SftpCmd::List(app.sftp_path.clone()));
    }
    if let Some(name) = app.sftp_mkdir.as_mut() {
        let (mut ok, mut cancel) = (false, false);
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(name).hint_text("имя папки").desired_width(150.0));
            r.request_focus();
            ok = ui.small_button("✓").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            cancel = ui.small_button("✕").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        let n = name.trim().to_string();
        if ok && !n.is_empty() {
            cmds.push(SftpCmd::Mkdir(join(&cwd, &n)));
            app.sftp_mkdir = None;
        } else if cancel {
            app.sftp_mkdir = None;
        }
    }
    if let Some(e) = &view.error {
        ui.label(RichText::new(format!("⚠ {e}")).color(RED).small());
    }
    if view.loading {
        ui.label(RichText::new("Загрузка…").small().color(MUTED));
    }
    ui.separator();

    let transfers_h = 40.0 + 30.0 * view.transfers.len().min(4) as f32 + if view.transfers.iter().any(|t| t.finished) { 26.0 } else { 0.0 };
    egui::ScrollArea::vertical()
        .max_height((ui.available_height() - transfers_h).max(80.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for e in &view.entries {
                let path = join(&cwd, &e.name);
                let icon = if e.is_dir { "📁" } else { "📄" };
                let sel = app.sftp_selected.as_deref() == Some(path.as_str());
                let resp = ui
                    .horizontal(|ui| {
                        let r = ui.selectable_label(sel, format!("{icon} {}", e.name));
                        if !e.is_dir {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.label(RichText::new(human(e.size)).small().color(MUTED));
                            });
                        }
                        r
                    })
                    .inner;
                if resp.clicked() {
                    app.sftp_selected = Some(path.clone());
                }
                if resp.double_clicked() {
                    if e.is_dir {
                        cmds.push(SftpCmd::List(path.clone()));
                    } else {
                        cmds.push(SftpCmd::Download { path: path.clone(), is_dir: false, dest: downloads.clone() });
                    }
                }
                resp.context_menu(|ui| {
                    if e.is_dir && ui.button("📂 Открыть").clicked() {
                        cmds.push(SftpCmd::List(path.clone()));
                        ui.close();
                    }
                    if ui.button("⤓ Скачать в «Загрузки»").clicked() {
                        cmds.push(SftpCmd::Download { path: path.clone(), is_dir: e.is_dir, dest: downloads.clone() });
                        ui.close();
                    }
                    if ui.button("✏ Переименовать…").clicked() {
                        app.sftp_rename = Some((path.clone(), e.name.clone()));
                        ui.close();
                    }
                    if ui.button("⧉ Копировать путь").clicked() {
                        ui.ctx().copy_text(path.clone());
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(RichText::new("🗑 Удалить…").color(RED)).clicked() {
                        app.sftp_delete = Some((path.clone(), e.is_dir));
                        ui.close();
                    }
                });
            }
            if view.entries.is_empty() && !view.loading {
                ui.label(RichText::new("Папка пуста").color(MUTED));
            }
        });

    ui.separator();
    ui.label(RichText::new("⬆ Перетащите файлы в окно, чтобы загрузить").small().color(MUTED));
    let mut clear = false;
    for t in view.transfers.iter().rev().take(4) {
        ui.horizontal(|ui| {
            ui.label(if t.upload { "⬆" } else { "⬇" });
            let frac = if t.total > 0 { t.done as f32 / t.total as f32 } else if t.finished { 1.0 } else { 0.0 };
            let text = match (&t.error, t.finished) {
                (Some(e), _) => format!("{} — ошибка: {e}", t.name),
                (None, true) => format!("{} — готово, {}", t.name, human(t.done)),
                (None, false) => format!("{} — {} из {}", t.name, human(t.done), human(t.total)),
            };
            ui.add(egui::ProgressBar::new(frac).text(RichText::new(text).small()).desired_width(ui.available_width() - 28.0));
            if t.finished && t.error.is_none() {
                if let Some(p) = &t.local {
                    if ui.small_button("📂").on_hover_text("Показать в папке").clicked() {
                        reveal(p);
                    }
                }
            }
        });
    }
    if view.transfers.iter().any(|t| t.finished) && ui.small_button("Очистить завершённые").clicked() {
        clear = true;
    }
    if clear {
        view.transfers.retain(|t| !t.finished);
    }
    drop(view);

    // Модальные мини-диалоги SFTP.
    if let Some((path, name)) = app.sftp_rename.as_mut() {
        let mut done = None;
        egui::Modal::new(egui::Id::unique("sftp_rename")).show(ui.ctx(), |ui| {
            ui.label(RichText::new("Переименовать").strong());
            let r = ui.text_edit_singleline(name);
            r.request_focus();
            ui.horizontal(|ui| {
                if ui.button("Переименовать").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    done = Some(true);
                }
                if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        if let Some(ok) = done {
            if ok && !name.trim().is_empty() {
                let parent = path.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
                cmds.push(SftpCmd::Rename { from: path.clone(), to: join(&parent, name.trim()) });
            }
            app.sftp_rename = None;
        }
    }
    if let Some((path, is_dir)) = app.sftp_delete.clone() {
        let mut done = None;
        egui::Modal::new(egui::Id::unique("sftp_delete")).show(ui.ctx(), |ui| {
            ui.label(RichText::new("Удалить на сервере?").strong());
            ui.monospace(&path);
            if is_dir {
                ui.label(RichText::new("Папка будет удалена вместе со всем содержимым.").color(RED));
            }
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Удалить").color(RED)).clicked() {
                    done = Some(true);
                }
                if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        if let Some(ok) = done {
            if ok {
                cmds.push(SftpCmd::Delete { path, is_dir });
            }
            app.sftp_delete = None;
        }
    }

    for c in cmds {
        app.tabs[ai].sftp(c);
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn human(n: u64) -> String {
    let f = n as f64;
    if f < 1024.0 {
        format!("{n} Б")
    } else if f < 1024.0 * 1024.0 {
        format!("{:.1} КБ", f / 1024.0)
    } else if f < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} МБ", f / 1024.0 / 1024.0)
    } else {
        format!("{:.2} ГБ", f / 1024.0 / 1024.0 / 1024.0)
    }
}

fn reveal(p: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(format!("/select,{}", p.display())).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = p.parent().unwrap_or(p);
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

// ───────────────────────── статус-бар ─────────────────────────

fn status_bar(app: &mut MobaApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        match app.active_tab() {
            Some(t) => {
                let (txt, col) = state_text(t);
                ui.label(RichText::new(txt).color(col).small());
                ui.separator();
                ui.label(RichText::new(format!("{}×{}", t.cols, t.rows)).small().monospace().color(MUTED));
                if t.scroll_offset() > 0 {
                    ui.separator();
                    ui.label(RichText::new(format!("↑ {}", t.scroll_offset())).small().color(YELLOW));
                }
            }
            None => {
                ui.label(RichText::new("Нет открытых вкладок").small().color(MUTED));
            }
        }
        if app.broadcast {
            ui.separator();
            ui.label(RichText::new("📢 РАССЫЛКА ВКЛ").small().strong().color(RED));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::config::is_portable() {
                ui.label(RichText::new("портативный").small().color(MUTED));
                ui.separator();
            }
            let msg = match &app.toast {
                Some((m, t)) if t.elapsed().as_secs() < 6 => RichText::new(m).small().color(FG),
                _ => RichText::new(&app.status).small().color(MUTED),
            };
            ui.label(msg);
        });
    });
}

fn state_text(t: &terminal::TermSession) -> (String, Color32) {
    #[cfg(feature = "ssh")]
    if let Some(s) = &t.ssh {
        use terminal::ssh::ConnState::*;
        return match s.state() {
            Connected => (format!("● SSH  {}", s.endpoint), GREEN),
            Connecting(e) => (format!("◌ Подключение к {e}…"), YELLOW),
            Authenticating(e) => (format!("◌ Вход на {e}…"), YELLOW),
            Disconnected(r) => (format!("● Отключено: {r} — R чтобы переподключить"), RED),
            Blocked(_) => ("⛔ Ключ хоста изменился — заблокировано".into(), RED),
        };
    }
    if t.is_alive() {
        (format!("● Локальный  {}", t.title), GREEN)
    } else {
        ("○ Процесс завершён — Ctrl+Shift+R".into(), MUTED)
    }
}

// ───────────────────────── палитра команд ─────────────────────────

/// Нечёткое сопоставление: все слова — подстроки (выше балл), иначе подпоследовательность.
pub fn fuzzy(q: &str, hay: &str) -> Option<i32> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return Some(0);
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    if words.iter().all(|w| hay.contains(w)) {
        let pos = hay.find(words[0]).unwrap_or(0) as i32;
        return Some(1000 - pos.min(500));
    }
    let mut it = hay.chars();
    for c in q.chars().filter(|c| !c.is_whitespace()) {
        it.find(|&h| h == c)?;
    }
    Some(100)
}

fn parse_conn(s: &str) -> Option<(String, String, u16)> {
    let s = s.trim();
    let s = s.strip_prefix("ssh ").unwrap_or(s).trim();
    let s = s.strip_prefix("ssh://").unwrap_or(s);
    let (user, rest) = s.split_once('@')?;
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (rest, 22),
    };
    if user.is_empty() || host.is_empty() || host.contains(' ') || user.contains(' ') {
        return None;
    }
    Some((user.into(), host.into(), port))
}

fn palette(app: &mut MobaApp, ctx: &egui::Context) {
    let Some(mut p) = app.palette.take() else { return };
    let q = p.query.to_lowercase();

    let mut items: Vec<(i32, String, String, Action)> = Vec::new();
    if let Some((u, h, port)) = parse_conn(&p.query) {
        items.push((10_000, format!("⚡ Подключиться к {u}@{h}:{port}"), "сохранится в «SSH»".into(), Action::QuickConnect(u, h, port)));
    }
    for s in &app.cfg.sessions {
        if let Some(score) = fuzzy(&q, &s.haystack()) {
            let hint = s.host_label().lines().next().unwrap_or_default().to_string();
            items.push((score + s.use_count.min(50) as i32 * 4 + 2000, format!("▶ {}", s.name), format!("{} · {}", hint, s.group), Action::Connect(s.id)));
        }
    }
    // (подпись, ключевые слова для поиска в латинице, горячая клавиша, действие)
    let mut actions: Vec<(String, &str, &str, Action)> = vec![
        ("Новая SSH-сессия".into(), "new ssh session host add", "Ctrl+Shift+N", Action::NewSsh),
        ("Импорт ~/.ssh/config".into(), "import ssh config", "", Action::ImportSshConfig),
        ("Рассылка ввода: вкл/выкл".into(), "broadcast multiexec all", "Ctrl+Shift+B", Action::Broadcast),
        ("Панель SFTP: показать/скрыть".into(), "sftp files panel", "Ctrl+Shift+E", Action::ToggleSftp),
        ("Найти в истории".into(), "search find scrollback", "Ctrl+Shift+F", Action::Search),
        ("Переподключить вкладку".into(), "reconnect", "Ctrl+Shift+R", Action::Reconnect),
        ("Дублировать вкладку".into(), "duplicate clone tab", "Ctrl+Shift+D", Action::Duplicate),
        ("Закрыть вкладку".into(), "close tab", "Ctrl+Shift+W", Action::CloseTab),
        ("Раскладка: один терминал".into(), "layout single one", "", Action::Layout(Layout::Single)),
        ("Раскладка: две колонки".into(), "layout split columns vertical 2", "", Action::Layout(Layout::Cols2)),
        ("Раскладка: две строки".into(), "layout split rows horizontal 2", "", Action::Layout(Layout::Rows2)),
        ("Раскладка: четыре панели".into(), "layout split grid 4", "", Action::Layout(Layout::Grid4)),
        ("Шрифт крупнее".into(), "font zoom in bigger", "Ctrl+=", Action::ZoomIn),
        ("Шрифт мельче".into(), "font zoom out smaller", "Ctrl+Shift+-", Action::ZoomOut),
        ("Настройки".into(), "settings preferences options", "Ctrl+Shift+,", Action::Settings),
    ];
    for s in &app.shells {
        actions.push((format!("Локальный: {}", s.label), "local shell terminal", "", Action::NewLocal(Some(s.command.clone()), s.label.clone())));
    }
    for (i, s) in app.cfg.snippets.iter().enumerate() {
        actions.push((format!("Сниппет: {}", s.name), "snippet command", if i < 12 { FKEYS[i] } else { "" }, Action::Snippet(i)));
    }
    for (label, kw, key, a) in actions {
        if let Some(score) = fuzzy(&q, &format!("{} {kw}", label.to_lowercase())) {
            items.push((score, format!("⌘ {label}"), key.to_string(), a));
        }
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items.truncate(12);
    if p.selected >= items.len() {
        p.selected = items.len().saturating_sub(1);
    }

    let mut run: Option<Action> = None;
    let mut close = false;
    egui::Modal::new(egui::Id::unique("palette")).show(ctx, |ui| {
        ui.set_width(580.0);
        let r = ui.add(
            egui::TextEdit::singleline(&mut p.query)
                .hint_text("Сессия, команда или user@host:port")
                .font(egui::TextStyle::Heading)
                .desired_width(f32::INFINITY),
        );
        // Поле палитры держит фокус всё время, пока палитра открыта.
        r.request_focus();
        p.focus = false;
        if r.changed() {
            p.selected = 0;
        }
        ui.input(|i| {
            if i.key_pressed(egui::Key::ArrowDown) {
                p.selected = (p.selected + 1).min(items.len().saturating_sub(1));
            }
            if i.key_pressed(egui::Key::ArrowUp) {
                p.selected = p.selected.saturating_sub(1);
            }
            if i.key_pressed(egui::Key::Escape) {
                close = true;
            }
            if i.key_pressed(egui::Key::Enter) {
                if let Some(it) = items.get(p.selected) {
                    run = Some(it.3.clone());
                }
            }
        });
        ui.add_space(4.0);
        for (k, (_, label, hint, a)) in items.iter().enumerate() {
            let sel = k == p.selected;
            let resp = ui
                .horizontal(|ui| {
                    let r = ui.selectable_label(sel, label);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(hint).small().color(MUTED));
                    });
                    r
                })
                .inner;
            if resp.clicked() {
                run = Some(a.clone());
            }
            if resp.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                p.selected = k;
            }
        }
        if items.is_empty() {
            ui.label(RichText::new("Ничего не найдено. Введите user@host, чтобы подключиться.").color(MUTED));
        }
        ui.add_space(2.0);
        ui.label(RichText::new("↑↓ выбор · Enter — выполнить · Esc — закрыть").small().color(MUTED));
    });

    if let Some(a) = run {
        app.run_action(ctx, a);
    } else if !close {
        app.palette = Some(p);
    }
}

const FKEYS: [&str; 12] = [
    "Ctrl+F1", "Ctrl+F2", "Ctrl+F3", "Ctrl+F4", "Ctrl+F5", "Ctrl+F6", "Ctrl+F7", "Ctrl+F8", "Ctrl+F9", "Ctrl+F10", "Ctrl+F11", "Ctrl+F12",
];

// ───────────────────────── редактор сессии ─────────────────────────

fn editor(app: &mut MobaApp, ctx: &egui::Context) {
    let Some(mut e) = app.editor.take() else { return };
    let folders = app.cfg.all_folders();
    let mut result: Option<(bool, bool)> = None; // (сохранить, подключиться)
    egui::Modal::new(egui::Id::unique("editor")).show(ctx, |ui| {
        ui.set_width(520.0);
        ui.label(RichText::new(if e.id == 0 { "Новая SSH-сессия" } else { "Изменить сессию" }).size(17.0).strong());
        ui.add_space(6.0);
        egui::Grid::new("ed").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Хост *");
            let r = ui.add(egui::TextEdit::singleline(&mut e.host).hint_text("server.example.com или 10.0.0.5").desired_width(320.0));
            if e.host.is_empty() && e.name.is_empty() {
                r.request_focus();
            }
            ui.end_row();
            ui.label("Пользователь *");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut e.user).desired_width(200.0));
                ui.label("Порт");
                ui.add(egui::TextEdit::singleline(&mut e.port).desired_width(60.0));
            });
            ui.end_row();
            ui.label("Имя");
            ui.add(egui::TextEdit::singleline(&mut e.name).hint_text("по умолчанию user@host").desired_width(320.0));
            ui.end_row();
            ui.label("Папка");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("ed_folder").selected_text(e.folder.clone()).width(140.0).show_ui(ui, |ui| {
                    for f in &folders {
                        ui.selectable_value(&mut e.folder, f.clone(), f);
                    }
                });
                ui.add(egui::TextEdit::singleline(&mut e.folder).hint_text("или новая").desired_width(160.0));
            });
            ui.end_row();
            ui.label("Ключ");
            ui.add(
                egui::TextEdit::singleline(&mut e.identity)
                    .hint_text("~/.ssh/id_ed25519 (пусто — агент, ~/.ssh/id_* или пароль)")
                    .desired_width(320.0),
            );
            ui.end_row();
            ui.label("Через jump-хосты");
            ui.add(
                egui::TextEdit::singleline(&mut e.jump)
                    .hint_text("bastion или user@host:port, через запятую")
                    .desired_width(320.0),
            );
            ui.end_row();
            ui.label("Теги");
            ui.add(egui::TextEdit::singleline(&mut e.tags).hint_text("prod, db").desired_width(320.0));
            ui.end_row();
            ui.label("Цвет вкладки");
            ui.horizontal(|ui| {
                for sw in SWATCHES {
                    let (fill, txt) = match sw {
                        Some([r, g, b]) => (Color32::from_rgb(r, g, b), " "),
                        None => (PANEL2, "—"),
                    };
                    let b = egui::Button::new(txt).fill(fill).min_size(egui::vec2(22.0, 22.0)).selected(e.color == sw);
                    if ui.add(b).clicked() {
                        e.color = sw;
                    }
                }
            });
            ui.end_row();
        });
        ui.add_space(4.0);
        ui.label(RichText::new("Пароли не сохраняются в профиле: их спросят при входе (можно запомнить в зашифрованном хранилище).").small().color(MUTED));
        if let Some(err) = &e.error {
            ui.label(RichText::new(format!("⚠ {err}")).color(RED));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("Сохранить и подключиться").strong()).fill(ACCENT.gamma_multiply(0.45))).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl)
            {
                result = Some((true, true));
            }
            if ui.button("Сохранить").clicked() {
                result = Some((true, false));
            }
            if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                result = Some((false, false));
            }
        });
    });

    match result {
        Some((true, connect)) => match e.build() {
            Ok(def) => {
                let id = app.cfg.upsert(def);
                app.dirty_cfg = true;
                app.selected_session = Some(id);
                if connect {
                    app.connect(ctx, id);
                }
            }
            Err(err) => {
                e.error = Some(err);
                app.editor = Some(e);
            }
        },
        Some((false, _)) => {}
        None => app.editor = Some(e),
    }
}

// ───────────────────────── вставка и подтверждения ─────────────────────────

fn paste_confirm(app: &mut MobaApp, ctx: &egui::Context) {
    let Some(mut text) = app.paste_confirm.take() else { return };
    let mut res: Option<u8> = None; // 1 вставить, 2 одной строкой, 0 отмена
    let mut never = false;
    egui::Modal::new(egui::Id::unique("paste")).show(ctx, |ui| {
        ui.set_width(560.0);
        let lines = text.lines().count();
        ui.label(RichText::new("Проверьте вставляемый текст").size(16.0).strong());
        ui.label(
            RichText::new(format!("{lines} строк, {} символов. Каждая строка выполнится как отдельная команда.", text.chars().count()))
                .color(YELLOW),
        );
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut text).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).desired_rows(8));
        });
        ui.checkbox(&mut never, "Больше не спрашивать (можно вернуть в настройках)");
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new("Вставить").fill(ACCENT.gamma_multiply(0.45))).clicked() {
                res = Some(1);
            }
            if ui.button("Вставить одной строкой").clicked() {
                res = Some(2);
            }
            if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                res = Some(0);
            }
        });
    });
    if never {
        app.cfg.settings.confirm_multiline_paste = false;
        app.dirty_cfg = true;
    }
    match res {
        Some(1) => app.do_paste(&text),
        Some(2) => {
            let one = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
            app.do_paste(&one);
        }
        Some(_) => {}
        None => app.paste_confirm = Some(text),
    }
}

fn confirmations(app: &mut MobaApp, ctx: &egui::Context) {
    if let Some(id) = app.confirm_delete {
        let name = app.cfg.find(id).map(|d| d.name.clone()).unwrap_or_default();
        let mut done = None;
        egui::Modal::new(egui::Id::unique("del_session")).show(ctx, |ui| {
            ui.label(RichText::new(format!("Удалить сессию «{name}»?")).strong());
            ui.label(RichText::new("Открытые вкладки останутся. Сохранённый пароль тоже будет удалён.").color(MUTED));
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Удалить").color(RED)).clicked() {
                    done = Some(true);
                }
                if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        if let Some(ok) = done {
            if ok {
                if let Some(crate::config::SessionKind::Ssh { host, port, user, .. }) = app.cfg.find(id).map(|d| d.kind.clone()) {
                    crate::secrets::remove(&format!("{user}@{host}:{port}"));
                }
                app.cfg.delete(id);
                for t in &mut app.tabs {
                    if t.def_id == Some(id) {
                        t.def_id = None;
                    }
                }
                app.dirty_cfg = true;
                app.toast(format!("Сессия «{name}» удалена"));
            }
            app.confirm_delete = None;
        }
    }

    if let Some((from, mut to)) = app.rename_folder.take() {
        let mut done = None;
        egui::Modal::new(egui::Id::unique("ren_folder")).show(ctx, |ui| {
            ui.label(RichText::new(format!("Переименовать папку «{from}»")).strong());
            let r = ui.text_edit_singleline(&mut to);
            r.request_focus();
            ui.horizontal(|ui| {
                if ui.button("Переименовать").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                    done = Some(true);
                }
                if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        match done {
            Some(true) => {
                app.cfg.rename_folder(&from, &to);
                app.dirty_cfg = true;
            }
            Some(false) => {}
            None => app.rename_folder = Some((from, to)),
        }
    }

    if let Some(folder) = app.confirm_folder_delete.clone() {
        let n = app.cfg.sessions.iter().filter(|s| s.group == folder).count();
        let target = app.cfg.all_folders().into_iter().find(|f| f != &folder).unwrap_or_else(|| "SSH".into());
        let mut done = None;
        egui::Modal::new(egui::Id::unique("del_folder")).show(ctx, |ui| {
            ui.label(RichText::new(format!("Удалить папку «{folder}»?")).strong());
            if n > 0 {
                ui.label(format!("{n} сессий будут перенесены в «{target}» — ничего не потеряется."));
            }
            ui.horizontal(|ui| {
                if ui.button(RichText::new("Удалить папку").color(RED)).clicked() {
                    done = Some(true);
                }
                if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
            });
        });
        if let Some(ok) = done {
            if ok {
                app.cfg.delete_folder(&folder, &target);
                app.dirty_cfg = true;
            }
            app.confirm_folder_delete = None;
        }
    }
}

// ───────────────────────── запросы SSH (ключ хоста, пароль, 2FA) ─────────────────────────

#[cfg(feature = "ssh")]
fn prompts(app: &mut MobaApp, ctx: &egui::Context) {
    use terminal::ssh::{Answer, PromptKind};
    let found = app.tabs.iter().enumerate().find_map(|(i, t)| t.ssh.as_ref().filter(|s| s.has_prompt()).map(|s| (i, s.clone())));
    let Some((ti, ssh)) = found else { return };
    let title = app.tabs[ti].title.clone();
    let mut answer: Option<Answer> = None;
    {
        let Ok(guard) = ssh.prompt.lock() else { return };
        let Some(pending) = guard.as_ref() else { return };
        let fields = match &pending.kind {
            PromptKind::Secret { .. } => 1,
            PromptKind::Kbd { prompts, .. } => prompts.len(),
            PromptKind::HostKey { .. } => 0,
        };
        if app.prompt_inputs.len() != fields {
            app.prompt_inputs = vec![String::new(); fields];
        }
        egui::Modal::new(egui::Id::unique("ssh_prompt")).show(ctx, |ui| {
            ui.set_width(520.0);
            ui.label(RichText::new(format!("Вкладка: {title}")).small().color(MUTED));
            match &pending.kind {
                PromptKind::HostKey { host, port, algo, fingerprint } => {
                    ui.label(RichText::new("🔑 Неизвестный хост").size(17.0).strong());
                    ui.add_space(4.0);
                    ui.label(format!(
                        "Первое подключение к {host}:{port}. Убедитесь, что отпечаток совпадает с ключом сервера — \
                         например, спросите администратора или выполните на сервере: ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub"
                    ));
                    ui.add_space(6.0);
                    egui::Frame::new().fill(FIELD).corner_radius(6).inner_margin(8).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(format!("{algo}\n{fingerprint}")).monospace()).selectable(true));
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(RichText::new("Доверять и запомнить").strong()).fill(ACCENT.gamma_multiply(0.45))).clicked() {
                            answer = Some(Answer::Trust { save: true });
                        }
                        if ui.button("Только в этот раз").clicked() {
                            answer = Some(Answer::Trust { save: false });
                        }
                        if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            answer = Some(Answer::Reject);
                        }
                    });
                }
                PromptKind::Secret { title, label, error, can_remember } => {
                    ui.label(RichText::new(format!("🔒 {title}")).size(16.0).strong());
                    ui.add_space(4.0);
                    ui.label(label);
                    let r = ui.add(egui::TextEdit::singleline(&mut app.prompt_inputs[0]).password(true).desired_width(f32::INFINITY));
                    if app.prompt_focus {
                        r.request_focus();
                        app.prompt_focus = false;
                    }
                    if let Some(e) = error {
                        ui.label(RichText::new(format!("⚠ {e}")).color(RED));
                    }
                    if *can_remember {
                        ui.checkbox(&mut app.prompt_remember, "Запомнить (шифруется Windows DPAPI для вашей учётной записи)");
                    }
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Войти").fill(ACCENT.gamma_multiply(0.45))).clicked() || enter {
                            answer = Some(Answer::Secret { value: app.prompt_inputs[0].clone(), remember: app.prompt_remember });
                        }
                        if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            answer = Some(Answer::Cancel);
                        }
                    });
                }
                PromptKind::Kbd { title, instructions, prompts } => {
                    ui.label(RichText::new(format!("🔐 {title}")).size(16.0).strong());
                    if !instructions.trim().is_empty() {
                        ui.label(instructions);
                    }
                    let mut enter = false;
                    for (k, (label, echo)) in prompts.iter().enumerate() {
                        ui.label(label.trim());
                        let r = ui.add(egui::TextEdit::singleline(&mut app.prompt_inputs[k]).password(!echo).desired_width(f32::INFINITY));
                        if k == 0 && app.prompt_focus {
                            r.request_focus();
                            app.prompt_focus = false;
                        }
                        enter |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && k + 1 == prompts.len();
                    }
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Отправить").fill(ACCENT.gamma_multiply(0.45))).clicked() || enter {
                            answer = Some(Answer::Responses(app.prompt_inputs.clone()));
                        }
                        if ui.button("Отмена").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            answer = Some(Answer::Cancel);
                        }
                    });
                }
            }
        });
    }
    if let Some(a) = answer {
        for s in &mut app.prompt_inputs {
            // затираем секрет в памяти UI
            unsafe { s.as_bytes_mut().fill(0) };
        }
        app.prompt_inputs.clear();
        app.prompt_remember = false;
        app.prompt_focus = true;
        ssh.answer(a);
    }
}

// ───────────────────────── настройки ─────────────────────────

fn settings_page(app: &mut MobaApp, ui: &mut egui::Ui) {
    let snapshot = |a: &MobaApp| serde_json::to_string(&(&a.cfg.settings, &a.cfg.snippets)).unwrap_or_default();
    let before = snapshot(app);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::new().inner_margin(egui::Margin::symmetric(24, 16)).show(ui, |ui| {
            ui.set_max_width(760.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Настройки").size(22.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("✕ Закрыть").clicked() {
                        app.page = Page::Terminal;
                    }
                });
            });
            ui.label(RichText::new("Изменения сохраняются сразу.").color(MUTED));
            let s = &mut app.cfg.settings;

            section(ui, "Терминал");
            ui.horizontal(|ui| {
                ui.label("Размер шрифта");
                ui.add(egui::Slider::new(&mut s.font_size, 8.0..=32.0).step_by(0.5));
            });
            ui.horizontal(|ui| {
                ui.label("История (строк)");
                ui.add(egui::DragValue::new(&mut s.scrollback).range(500..=100_000).speed(100));
                let mb = s.scrollback as f64 * 120.0 * 32.0 / 1024.0 / 1024.0;
                ui.label(RichText::new(format!("≈ до {mb:.0} МБ на вкладку при 120 колонках; применяется к новым вкладкам")).small().color(MUTED));
            });
            ui.checkbox(&mut s.cursor_blink, "Мигающий курсор");

            section(ui, "Мышь и буфер обмена");
            ui.checkbox(&mut s.copy_on_select, "Копировать при выделении (как PuTTY)");
            ui.checkbox(&mut s.right_click_paste, "Правая кнопка — вставка (как PuTTY; иначе контекстное меню)");
            ui.checkbox(&mut s.confirm_multiline_paste, "Подтверждать вставку нескольких строк или больше 5 КБ");

            section(ui, "Подключение");
            ui.horizontal(|ui| {
                ui.label("Keep-alive, сек");
                ui.add(egui::DragValue::new(&mut s.keepalive_secs).range(5..=300));
                ui.label(RichText::new("после 3 пропусков соединение считается потерянным").small().color(MUTED));
            });
            ui.checkbox(&mut s.auto_reconnect, "Автоматически переподключаться при обрыве (2, 4, 8 … 60 с)");
            ui.checkbox(&mut s.sftp_auto_open, "Показывать панель SFTP при подключении");
            ui.checkbox(&mut s.restore_tabs, "Восстанавливать вкладки после перезапуска (без паролей)");

            section(ui, "Сниппеты  ·  Ctrl+F1…F12 и палитра команд");
            let mut remove = None;
            for (i, sn) in app.cfg.snippets.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if i < 12 { FKEYS[i] } else { "      " }).monospace().small().color(MUTED));
                    ui.add(egui::TextEdit::singleline(&mut sn.name).desired_width(140.0));
                    ui.add(egui::TextEdit::singleline(&mut sn.text).font(egui::TextStyle::Monospace).desired_width(330.0));
                    ui.checkbox(&mut sn.send_enter, "⏎").on_hover_text("Нажать Enter после вставки");
                    if ui.small_button("🗑").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                app.cfg.snippets.remove(i);
                app.dirty_cfg = true;
            }
            if ui.button("＋ Добавить сниппет").clicked() {
                app.cfg.snippets.push(Snippet { name: "Новый".into(), text: String::new(), send_enter: true });
                app.dirty_cfg = true;
            }

            section(ui, "Горячие клавиши");
            egui::Grid::new("hk_all").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                for (k, v) in HOTKEYS {
                    ui.monospace(*k);
                    ui.label(RichText::new(*v).color(MUTED));
                    ui.end_row();
                }
            });

            section(ui, "Данные");
            let dir = crate::config::data_dir();
            ui.horizontal(|ui| {
                ui.label("Папка данных:");
                ui.monospace(dir.display().to_string());
                if ui.small_button("📂").clicked() {
                    reveal(&dir.join("config.json"));
                }
            });
            ui.label(RichText::new(if crate::config::is_portable() {
                "Портативный режим: данные хранятся рядом с программой."
            } else {
                "Портативный режим: положите пустой файл «portable» рядом с mobarust.exe."
            })
            .small()
            .color(MUTED));
            ui.label(
                RichText::new("Конфигурация пишется атомарно, хранятся 3 резервные копии (config.bak1–3.json). Пароли — только в зашифрованном secrets.json (DPAPI).")
                    .small()
                    .color(MUTED),
            );

            section(ui, "О программе");
            ui.label(format!("MobaRust {}", env!("CARGO_PKG_VERSION")));
            ui.label(RichText::new("Без аккаунта, без телеметрии, без автообновлений. Открытый код.").color(MUTED));
        });
    });
    if snapshot(app) != before {
        app.dirty_cfg = true;
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(title).size(15.0).strong().color(ACCENT));
    ui.separator();
}

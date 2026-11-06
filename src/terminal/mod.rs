//! Эмуляция терминала: единая абстракция `TermSession` поверх vt100,
//! общая для локального PTY и SSH-канала, плюс рендер сетки в egui.

pub mod local;
#[cfg(feature = "ssh")]
pub mod ssh;

use egui::Color32;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// u16 → NonZeroU16 (vt100 требует ненулевые размеры).
pub(crate) fn nz(v: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(v.max(1)).unwrap()
}

/// Сообщение в поток ввода-вывода сессии.
pub enum Msg {
    /// Байты, которые нужно отправить в PTY / SSH-канал.
    Data(Vec<u8>),
    /// Новый размер терминала (строки, столбцы).
    Resize(u16, u16),
}

/// Канал отправки во ввод-вывод сессии. Локальный шелл работает на
/// синхронных потоках (`std::mpsc`), SSH — на tokio (`tokio::mpsc`).
enum Writer {
    Std(std::sync::mpsc::Sender<Msg>),
    #[cfg(feature = "ssh")]
    Tokio(tokio::sync::mpsc::UnboundedSender<Msg>),
}

/// Живая сессия терминала: состояние экрана + канал ввода.
pub struct TermSession {
    pub title: String,
    pub kind_label: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    /// Флаг «появился новый вывод» → перерисовать.
    pub dirty: Arc<AtomicBool>,
    /// Флаг «сессия жива».
    pub alive: Arc<AtomicBool>,
    pub rows: u16,
    pub cols: u16,
    writer: Writer,
}

impl TermSession {
    fn send(&self, m: Msg) {
        match &self.writer {
            Writer::Std(tx) => {
                let _ = tx.send(m);
            }
            #[cfg(feature = "ssh")]
            Writer::Tokio(tx) => {
                let _ = tx.send(m);
            }
        }
    }

    /// Отправить ввод пользователя.
    pub fn write_input(&self, data: &[u8]) {
        self.send(Msg::Data(data.to_vec()));
    }

    /// Поменять размер терминала (строки/столбцы).
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(nz(rows), nz(cols));
        }
        self.send(Msg::Resize(rows, cols));
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Забрать и сбросить флаг «нужно перерисовать».
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::SeqCst)
    }
}

// ───────────────────────── цвета ─────────────────────────

const DEFAULT_FG: Color32 = Color32::from_rgb(0xc5, 0xcd, 0xd8);
pub const TERM_BG: Color32 = Color32::from_rgb(0x0a, 0x0e, 0x14);
const CURSOR: Color32 = Color32::from_rgb(0xd7, 0xde, 0xe8);

/// 16 базовых ANSI-цветов (нормальные + яркие).
const ANSI16: [(u8, u8, u8); 16] = [
    (0x1a, 0x1f, 0x29), // 0 black
    (0xe0, 0x6c, 0x75), // 1 red
    (0x4e, 0xc9, 0xa3), // 2 green
    (0xe5, 0xc0, 0x7b), // 3 yellow
    (0x4a, 0xa8, 0xff), // 4 blue
    (0xc6, 0x78, 0xdd), // 5 magenta
    (0x56, 0xb6, 0xc2), // 6 cyan
    (0xc5, 0xcd, 0xd8), // 7 white
    (0x5a, 0x66, 0x76), // 8 bright black
    (0xff, 0x8a, 0x92), // 9 bright red
    (0x6f, 0xe3, 0xc0), // 10 bright green
    (0xff, 0xdb, 0x9b), // 11 bright yellow
    (0x7a, 0xc0, 0xff), // 12 bright blue
    (0xd9, 0x9a, 0xef), // 13 bright magenta
    (0x7d, 0xd4, 0xe0), // 14 bright cyan
    (0xff, 0xff, 0xff), // 15 bright white
];

/// Перевод цвета xterm-256 → RGB.
fn idx_to_rgb(i: u8) -> (u8, u8, u8) {
    match i {
        0..=15 => ANSI16[i as usize],
        16..=231 => {
            let i = i - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let c = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (c(r), c(g), c(b))
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

fn to_color(c: vt100::Color, fallback: Color32) -> Color32 {
    match c {
        vt100::Color::Default => fallback,
        vt100::Color::Idx(i) => {
            let (r, g, b) = idx_to_rgb(i);
            Color32::from_rgb(r, g, b)
        }
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

// ───────────────────────── ввод с клавиатуры ─────────────────────────

/// Спец-клавиши → escape-последовательности (VT).
pub fn encode_key(key: egui::Key, mods: egui::Modifiers) -> Option<Vec<u8>> {
    use egui::Key::*;
    let bytes = match key {
        Enter => vec![b'\r'],
        Backspace => vec![0x7f],
        Tab => {
            if mods.shift {
                b"\x1b[Z".to_vec()
            } else {
                vec![b'\t']
            }
        }
        Escape => vec![0x1b],
        ArrowUp => b"\x1b[A".to_vec(),
        ArrowDown => b"\x1b[B".to_vec(),
        ArrowRight => b"\x1b[C".to_vec(),
        ArrowLeft => b"\x1b[D".to_vec(),
        Home => b"\x1b[H".to_vec(),
        End => b"\x1b[F".to_vec(),
        Delete => b"\x1b[3~".to_vec(),
        Insert => b"\x1b[2~".to_vec(),
        PageUp => b"\x1b[5~".to_vec(),
        PageDown => b"\x1b[6~".to_vec(),
        F1 => b"\x1bOP".to_vec(),
        F2 => b"\x1bOQ".to_vec(),
        F3 => b"\x1bOR".to_vec(),
        F4 => b"\x1bOS".to_vec(),
        _ => return None,
    };
    Some(bytes)
}

/// Ctrl + клавиша → управляющий байт (Ctrl+C = 0x03 и т.п.).
pub fn ctrl_byte(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
    let n = match key {
        A => 1, B => 2, C => 3, D => 4, E => 5, F => 6, G => 7, H => 8,
        I => 9, J => 10, K => 11, L => 12, M => 13, N => 14, O => 15, P => 16,
        Q => 17, R => 18, S => 19, T => 20, U => 21, V => 22, W => 23, X => 24,
        Y => 25, Z => 26,
        OpenBracket => 27,  // Ctrl+[ = ESC
        Backslash => 28,
        CloseBracket => 29,
        _ => return None,
    };
    Some(n)
}

// ───────────────────────── рендер ─────────────────────────

/// Нарисовать терминал, подогнав его сетку под доступное место.
/// Возвращает Response области (для фокуса/клика).
pub fn draw_terminal(ui: &mut egui::Ui, sess: &mut TermSession, font_size: f32) -> egui::Response {
    let ctx = ui.ctx().clone();
    let font_id = egui::FontId::monospace(font_size);
    let (ch_w, ch_h) = ctx.fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)));

    // Подгон размера под доступную область.
    let avail = ui.available_size();
    let cols = ((avail.x / ch_w).floor() as u16).max(8);
    let rows = ((avail.y / ch_h).floor() as u16).max(2);
    sess.resize(rows, cols);

    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, TERM_BG);

    let guard = sess.parser.lock().unwrap();
    let screen = guard.screen();
    let (srows, scols) = {
        let (r, c) = screen.size();
        (r.get(), c.get())
    };

    for r in 0..srows {
        let mut job = egui::text::LayoutJob::default();
        let y = rect.top() + r as f32 * ch_h;
        for c in 0..scols {
            let (text, mut fg, bg_opt, bold) = match screen.cell(r, c) {
                Some(cell) => {
                    let s = cell.contents();
                    let s = if s.is_empty() { " " } else { s };
                    let mut fg = to_color(cell.fgcolor(), DEFAULT_FG);
                    let mut bg = match cell.bgcolor() {
                        vt100::Color::Default => None,
                        other => Some(to_color(other, TERM_BG)),
                    };
                    if cell.inverse() {
                        let real_bg = bg.unwrap_or(TERM_BG);
                        bg = Some(fg);
                        fg = real_bg;
                    }
                    (s, fg, bg, cell.bold())
                }
                None => (" ", DEFAULT_FG, None, false),
            };
            if let Some(bg) = bg_opt {
                let x = rect.left() + c as f32 * ch_w;
                painter.rect_filled(
                    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(ch_w, ch_h)),
                    0.0,
                    bg,
                );
            }
            if bold {
                fg = fg.gamma_multiply(1.15);
            }
            job.append(
                text,
                0.0,
                egui::text::TextFormat {
                    font_id: font_id.clone(),
                    color: fg,
                    ..Default::default()
                },
            );
        }
        let galley = ctx.fonts_mut(|f| f.layout_job(job));
        painter.galley(egui::pos2(rect.left(), y), galley, DEFAULT_FG);
    }

    // Курсор (мигает).
    let (cr, cc) = screen.cursor_position();
    let blink = (ui.input(|i| i.time) * 1.5).sin() > -0.3;
    if blink && sess.is_alive() {
        let x = rect.left() + cc as f32 * ch_w;
        let y = rect.top() + cr as f32 * ch_h;
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(ch_w, ch_h)),
            0.0,
            CURSOR.gamma_multiply(0.7),
        );
    }

    resp
}

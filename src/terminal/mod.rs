//! Эмуляция терминала: единая абстракция `TermSession` поверх vt100,
//! общая для локального PTY и SSH-канала, плюс рендер сетки в egui.
//!
//! Оптимизация рендера: сетка ячеек раскладывается в галеи (`Galley`) только
//! когда реально изменился вывод (счётчик `gen`) или размер/шрифт. В простое
//! (мигание курсора) кадр лишь перерисовывает готовые галеи — без блокировки
//! мьютекса, без перелэйаута, с группировкой подряд идущих ячеек в раны.

pub mod local;
#[cfg(feature = "ssh")]
pub mod ssh;

use egui::Color32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// u16 → NonZeroU16 (vt100 требует ненулевые размеры).
pub(crate) fn nz(v: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(v.max(1)).unwrap()
}

/// Сообщение в поток ввода-вывода сессии.
pub enum Msg {
    Data(Vec<u8>),
    Resize(u16, u16),
}

/// Канал отправки во ввод-вывод сессии.
pub(crate) enum Writer {
    Std(std::sync::mpsc::Sender<Msg>),
    #[cfg(feature = "ssh")]
    Tokio(tokio::sync::mpsc::UnboundedSender<Msg>),
}

/// Кэш отрисовки (живёт в UI-потоке, трогается только в `draw_terminal`).
#[derive(Default)]
struct RenderCache {
    gen_id: u64,
    cols: u16,
    rows: u16,
    font: f32,
    ch_w: f32,
    ch_h: f32,
    galleys: Vec<Arc<egui::Galley>>,
    /// Фоновые раны: (строка, столбец, длина, цвет).
    bg: Vec<(u16, u16, u16, Color32)>,
    cursor: (u16, u16),
}

/// Живая сессия терминала.
pub struct TermSession {
    pub title: String,
    pub kind_label: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub dirty: Arc<AtomicBool>,
    pub alive: Arc<AtomicBool>,
    /// Монотонный счётчик поколений вывода — для валидации кэша рендера.
    pub generation: Arc<AtomicU64>,
    pub rows: u16,
    pub cols: u16,
    writer: Writer,
    cache: RenderCache,
}

impl TermSession {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        title: String,
        kind_label: &str,
        parser: Arc<Mutex<vt100::Parser>>,
        dirty: Arc<AtomicBool>,
        alive: Arc<AtomicBool>,
        generation: Arc<AtomicU64>,
        rows: u16,
        cols: u16,
        writer: Writer,
    ) -> Self {
        Self {
            title,
            kind_label: kind_label.to_string(),
            parser,
            dirty,
            alive,
            generation,
            rows,
            cols,
            writer,
            cache: RenderCache::default(),
        }
    }

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

    pub fn write_input(&self, data: &[u8]) {
        self.send(Msg::Data(data.to_vec()));
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(nz(rows), nz(cols));
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.send(Msg::Resize(rows, cols));
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::SeqCst)
    }

    /// Пересобрать кэш галей из текущего экрана (вызывается при смене `gen`).
    fn rebuild_cache(
        &mut self,
        ctx: &egui::Context,
        font_id: &egui::FontId,
        cols: u16,
        rows: u16,
        ch_w: f32,
        ch_h: f32,
        font: f32,
        gen_id: u64,
    ) {
        let guard = self.parser.lock().unwrap();
        let screen = guard.screen();
        let (srows, scols) = {
            let (r, c) = screen.size();
            (r.get(), c.get())
        };

        let mut galleys: Vec<Arc<egui::Galley>> = Vec::with_capacity(srows as usize);
        let mut bg: Vec<(u16, u16, u16, Color32)> = Vec::new();

        for r in 0..srows {
            let mut job = egui::text::LayoutJob::default();

            // Группировка подряд идущих ячеек: fg/bold — в текстовые раны,
            // bg — в прямоугольные раны (меньше секций и меньше примитивов).
            let mut run = String::new();
            let mut run_fg = DEFAULT_FG;
            let mut run_bold = false;
            let mut run_on = false;

            let mut bg_cur: Option<(u16, Color32)> = None; // (начальный столбец, цвет)
            let mut bg_len = 0u16;

            for c in 0..scols {
                let (ch, mut fg, bgc, bold) = match screen.cell(r, c) {
                    Some(cell) => {
                        let s = cell.contents();
                        let s = if s.is_empty() { " " } else { s };
                        let mut fg = to_color(cell.fgcolor(), DEFAULT_FG);
                        let mut bg = match cell.bgcolor() {
                            vt100::Color::Default => None,
                            other => Some(to_color(other, TERM_BG)),
                        };
                        if cell.inverse() {
                            let rb = bg.unwrap_or(TERM_BG);
                            bg = Some(fg);
                            fg = rb;
                        }
                        (s, fg, bg, cell.bold())
                    }
                    None => (" ", DEFAULT_FG, None, false),
                };
                if bold {
                    fg = fg.gamma_multiply(1.15);
                }

                // fg/bold-ран
                if run_on && fg == run_fg && bold == run_bold {
                    run.push_str(ch);
                } else {
                    if run_on {
                        job.append(&run, 0.0, fmt(font_id, run_fg));
                    }
                    run.clear();
                    run.push_str(ch);
                    run_fg = fg;
                    run_bold = bold;
                    run_on = true;
                }

                // bg-ран
                match bg_cur {
                    Some((start, col)) => {
                        if bgc == Some(col) {
                            bg_len += 1;
                        } else {
                            bg.push((r, start, bg_len, col));
                            match bgc {
                                Some(nc) => {
                                    bg_cur = Some((c, nc));
                                    bg_len = 1;
                                }
                                None => {
                                    bg_cur = None;
                                    bg_len = 0;
                                }
                            }
                        }
                    }
                    None => {
                        if let Some(nc) = bgc {
                            bg_cur = Some((c, nc));
                            bg_len = 1;
                        }
                    }
                }
            }
            if run_on {
                job.append(&run, 0.0, fmt(font_id, run_fg));
            }
            if let Some((start, col)) = bg_cur {
                bg.push((r, start, bg_len, col));
            }
            galleys.push(ctx.fonts_mut(|f| f.layout_job(job)));
        }

        let cursor = screen.cursor_position();
        drop(guard);

        self.cache = RenderCache {
            gen_id,
            cols,
            rows,
            font,
            ch_w,
            ch_h,
            galleys,
            bg,
            cursor,
        };
    }
}

fn fmt(font_id: &egui::FontId, color: Color32) -> egui::text::TextFormat {
    egui::text::TextFormat {
        font_id: font_id.clone(),
        color,
        ..Default::default()
    }
}

// ───────────────────────── цвета ─────────────────────────

const DEFAULT_FG: Color32 = Color32::from_rgb(0xc5, 0xcd, 0xd8);
pub const TERM_BG: Color32 = Color32::from_rgb(0x0a, 0x0e, 0x14);
const CURSOR: Color32 = Color32::from_rgb(0xd7, 0xde, 0xe8);

const ANSI16: [(u8, u8, u8); 16] = [
    (0x1a, 0x1f, 0x29),
    (0xe0, 0x6c, 0x75),
    (0x4e, 0xc9, 0xa3),
    (0xe5, 0xc0, 0x7b),
    (0x4a, 0xa8, 0xff),
    (0xc6, 0x78, 0xdd),
    (0x56, 0xb6, 0xc2),
    (0xc5, 0xcd, 0xd8),
    (0x5a, 0x66, 0x76),
    (0xff, 0x8a, 0x92),
    (0x6f, 0xe3, 0xc0),
    (0xff, 0xdb, 0x9b),
    (0x7a, 0xc0, 0xff),
    (0xd9, 0x9a, 0xef),
    (0x7d, 0xd4, 0xe0),
    (0xff, 0xff, 0xff),
];

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

pub fn ctrl_byte(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
    let n = match key {
        A => 1, B => 2, C => 3, D => 4, E => 5, F => 6, G => 7, H => 8,
        I => 9, J => 10, K => 11, L => 12, M => 13, N => 14, O => 15, P => 16,
        Q => 17, R => 18, S => 19, T => 20, U => 21, V => 22, W => 23, X => 24,
        Y => 25, Z => 26,
        OpenBracket => 27,
        Backslash => 28,
        CloseBracket => 29,
        _ => return None,
    };
    Some(n)
}

// ───────────────────────── рендер ─────────────────────────

pub fn draw_terminal(ui: &mut egui::Ui, sess: &mut TermSession, font_size: f32) -> egui::Response {
    let ctx = ui.ctx().clone();
    let font_id = egui::FontId::monospace(font_size);

    // Метрики шрифта — из кэша, пока шрифт тот же.
    let (ch_w, ch_h) = if sess.cache.font == font_size && sess.cache.ch_w > 0.0 {
        (sess.cache.ch_w, sess.cache.ch_h)
    } else {
        ctx.fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)))
    };

    let avail = ui.available_size();
    let cols = ((avail.x / ch_w).floor() as u16).max(8);
    let rows = ((avail.y / ch_h).floor() as u16).max(2);
    sess.resize(rows, cols);

    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, TERM_BG);

    // Перелэйаут только если изменился вывод / размер / шрифт.
    let g = sess.generation.load(Ordering::SeqCst);
    let stale = g != sess.cache.gen_id
        || cols != sess.cache.cols
        || rows != sess.cache.rows
        || font_size != sess.cache.font;
    if stale {
        sess.rebuild_cache(&ctx, &font_id, cols, rows, ch_w, ch_h, font_size, g);
    }

    // Фоновые прямоугольники.
    for &(r, c, len, color) in &sess.cache.bg {
        let x = rect.left() + c as f32 * ch_w;
        let y = rect.top() + r as f32 * ch_h;
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(len as f32 * ch_w, ch_h)),
            0.0,
            color,
        );
    }
    // Текст (готовые галеи).
    for (r, g) in sess.cache.galleys.iter().enumerate() {
        let y = rect.top() + r as f32 * ch_h;
        painter.galley(egui::pos2(rect.left(), y), g.clone(), DEFAULT_FG);
    }
    // Курсор (мигает — только это и считается в простое).
    let (cr, cc) = sess.cache.cursor;
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

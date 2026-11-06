//! Эмуляция терминала поверх vt100: общая для локального PTY и SSH сессия,
//! выделение и копирование, прокрутка истории, поиск, отчёты мыши,
//! кодирование клавиш и быстрый рендер в egui.
//!
//! Рендер: строки раскладываются в `Galley` только при изменении вывода
//! (счётчик `generation`), размера или шрифта. Фоновые потоки сами будят UI
//! (`ctx.request_repaint`), поэтому в простое приложение не тратит CPU.

pub mod local;
#[cfg(feature = "ssh")]
pub mod ssh;

use egui::Color32;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) fn nz(v: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(v.max(1)).unwrap()
}

/// Команды потоку ввода-вывода сессии.
pub enum Msg {
    Data(Vec<u8>),
    Resize(u16, u16),
    /// Переподключиться (SSH) / перезапустить shell (локальный).
    Reconnect,
    #[cfg(feature = "ssh")]
    Sftp(ssh::SftpCmd),
    Close,
}

pub(crate) enum Writer {
    Std(std::sync::mpsc::Sender<Msg>),
    #[cfg(feature = "ssh")]
    Tokio(tokio::sync::mpsc::UnboundedSender<Msg>),
}

/// Общее состояние, которое обновляют фоновые потоки.
#[derive(Clone)]
pub(crate) struct Shared {
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub dirty: Arc<AtomicBool>,
    pub alive: Arc<AtomicBool>,
    pub generation: Arc<AtomicU64>,
    pub ctx: egui::Context,
}

impl Shared {
    pub fn new(ctx: &egui::Context, rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: Arc::new(Mutex::new(vt100::Parser::new(nz(rows), nz(cols), scrollback))),
            dirty: Arc::new(AtomicBool::new(true)),
            alive: Arc::new(AtomicBool::new(true)),
            generation: Arc::new(AtomicU64::new(1)),
            ctx: ctx.clone(),
        }
    }

    /// Скормить байты парсеру и разбудить UI.
    pub fn feed(&self, bytes: &[u8]) {
        if let Ok(mut p) = self.parser.lock() {
            p.process(bytes);
        }
        self.bump();
    }

    pub fn bump(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.dirty.store(true, Ordering::SeqCst);
        self.ctx.request_repaint();
    }

    /// Служебная строка серым цветом.
    pub fn note(&self, text: &str) {
        self.feed(format!("\r\n\x1b[90m{}\x1b[0m\r\n", text.replace('\n', "\r\n")).as_bytes());
    }
}

#[derive(Default)]
struct RenderCache {
    gen_id: u64,
    cols: u16,
    rows: u16,
    font: f32,
    ch_w: f32,
    ch_h: f32,
    galleys: Vec<Arc<egui::Galley>>,
    bg: Vec<(u16, u16, u16, Color32)>,
    cursor: (u16, u16),
    cursor_hidden: bool,
    lines: Vec<String>,
}

/// Совпадение поиска: (абсолютная строка, начальный столбец, длина).
#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub query: String,
    pub matches: Vec<(usize, usize, usize)>,
    pub current: usize,
    max: usize,
}

pub enum TabKind {
    Local { shell: Option<String> },
    Ssh,
}

pub struct TermSession {
    pub title: String,
    pub kind: TabKind,
    /// Сохранённая сессия, из которой открыта вкладка.
    pub def_id: Option<u64>,
    pub color: Option<[u8; 3]>,
    pub(crate) shared: Shared,
    pub rows: u16,
    pub cols: u16,
    writer: Writer,
    #[cfg(feature = "ssh")]
    pub ssh: Option<Arc<ssh::SshShared>>,
    cache: RenderCache,
    /// Выделение в координатах видимого экрана (строка, столбец).
    sel: Option<((u16, u16), (u16, u16))>,
    selecting: bool,
    scroll: usize,
    scroll_accum: f32,
    pub search: Search,
    pub seen_gen: u64,
    pub broadcast_excluded: bool,
}

/// Что запросил пользователь мышью в области терминала.
#[derive(Default)]
pub struct TermUi {
    pub clicked: bool,
    pub copied: Option<String>,
    pub paste: bool,
    pub open_search: bool,
    pub reconnect: bool,
}

impl TermSession {
    pub(crate) fn new(title: String, kind: TabKind, shared: Shared, rows: u16, cols: u16, writer: Writer) -> Self {
        Self {
            title,
            kind,
            def_id: None,
            color: None,
            shared,
            rows,
            cols,
            writer,
            #[cfg(feature = "ssh")]
            ssh: None,
            cache: RenderCache::default(),
            sel: None,
            selecting: false,
            scroll: 0,
            scroll_accum: 0.0,
            search: Search::default(),
            seen_gen: 0,
            broadcast_excluded: false,
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

    pub fn is_ssh(&self) -> bool {
        matches!(self.kind, TabKind::Ssh)
    }

    /// Ввод пользователя (сбрасывает прокрутку истории к низу).
    pub fn write_input(&mut self, data: &[u8]) {
        if self.scroll != 0 {
            self.set_scroll(0);
        }
        self.send(Msg::Data(data.to_vec()));
    }

    /// Вставка текста: CR вместо LF, bracketed paste, если программа его включила.
    pub fn paste(&mut self, text: &str) {
        let body = text.replace("\r\n", "\n").replace('\n', "\r");
        let bracketed = self.bracketed_paste();
        let mut out = Vec::with_capacity(body.len() + 12);
        if bracketed {
            out.extend_from_slice(b"\x1b[200~");
        }
        out.extend_from_slice(body.as_bytes());
        if bracketed {
            out.extend_from_slice(b"\x1b[201~");
        }
        self.write_input(&out);
    }

    pub fn bracketed_paste(&self) -> bool {
        self.shared.parser.lock().map(|p| p.screen().bracketed_paste()).unwrap_or(false)
    }

    pub fn app_cursor(&self) -> bool {
        self.shared.parser.lock().map(|p| p.screen().application_cursor()).unwrap_or(false)
    }

    pub fn reconnect(&self) {
        self.send(Msg::Reconnect);
    }

    #[cfg(feature = "ssh")]
    pub fn sftp(&self, cmd: ssh::SftpCmd) {
        self.send(Msg::Sftp(cmd));
    }

    pub fn close(&self) {
        self.send(Msg::Close);
    }

    pub(crate) fn replace_writer(&mut self, w: Writer) {
        self.writer = w;
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        if let Ok(mut p) = self.shared.parser.lock() {
            p.screen_mut().set_size(nz(rows), nz(cols));
        }
        self.sel = None;
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        self.send(Msg::Resize(rows, cols));
    }

    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::SeqCst)
    }

    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::SeqCst)
    }

    pub fn has_activity(&self) -> bool {
        self.generation() != self.seen_gen && self.seen_gen != 0
    }

    // ── прокрутка истории ─────────────────────────────

    pub fn scroll_offset(&self) -> usize {
        self.scroll
    }

    pub fn set_scroll(&mut self, rows: usize) {
        if let Ok(mut p) = self.shared.parser.lock() {
            p.screen_mut().set_scrollback(rows);
            self.scroll = p.screen().scrollback();
        }
        self.sel = None;
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let target = (self.scroll as isize + delta).max(0) as usize;
        self.set_scroll(target);
    }

    pub fn scroll_page(&mut self, up: bool) {
        let page = self.rows.saturating_sub(1).max(1) as isize;
        self.scroll_by(if up { page } else { -page });
    }

    pub fn scroll_to_top(&mut self) {
        self.set_scroll(usize::MAX);
    }

    // ── выделение ───────────────────────────────────

    pub fn has_selection(&self) -> bool {
        matches!(self.sel, Some((a, b)) if a != b)
    }

    pub fn clear_selection(&mut self) {
        self.sel = None;
    }

    fn sel_norm(&self) -> Option<((u16, u16), (u16, u16))> {
        let (a, b) = self.sel?;
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    pub fn selection_text(&self) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.sel_norm()?;
        let p = self.shared.parser.lock().ok()?;
        let s = p.screen().contents_between(r0, c0, r1, c1.saturating_add(1));
        Some(s.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n"))
    }

    pub fn select_all_visible(&mut self) {
        self.sel = Some(((0, 0), (self.rows.saturating_sub(1), self.cols.saturating_sub(1))));
    }

    fn select_word(&mut self, row: u16, col: u16) {
        let line = self.cache.lines.get(row as usize).cloned().unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        let is_word = |c: char| c.is_alphanumeric() || "-_./~:@%+=".contains(c);
        let c = col as usize;
        if c >= chars.len() || !is_word(chars[c]) {
            return;
        }
        let mut s = c;
        while s > 0 && is_word(chars[s - 1]) {
            s -= 1;
        }
        let mut e = c;
        while e + 1 < chars.len() && is_word(chars[e + 1]) {
            e += 1;
        }
        self.sel = Some(((row, s as u16), (row, e as u16)));
    }

    // ── поиск по истории ────────────────────────────

    /// Найти все вхождения во всём scrollback и перейти к последнему (ближайшему к низу).
    pub fn run_search(&mut self) {
        let q = self.search.query.to_lowercase();
        self.search.matches.clear();
        self.search.current = 0;
        if q.is_empty() {
            return;
        }
        let rows = self.rows as usize;
        let mut lines: Vec<String> = Vec::new();
        let max;
        {
            let Ok(mut p) = self.shared.parser.lock() else { return };
            let orig = p.screen().scrollback();
            p.screen_mut().set_scrollback(usize::MAX);
            max = p.screen().scrollback();
            let cols = self.cols;
            let mut off = max;
            loop {
                p.screen_mut().set_scrollback(off);
                let top = max - off;
                for (i, line) in p.screen().rows(0, cols).enumerate() {
                    let abs = top + i;
                    if abs >= lines.len() {
                        lines.push(line);
                    }
                }
                if off == 0 {
                    break;
                }
                off = off.saturating_sub(rows.max(1));
            }
            p.screen_mut().set_scrollback(orig);
        }
        for (abs, line) in lines.iter().enumerate() {
            let low = line.to_lowercase();
            let mut start = 0;
            while let Some(pos) = low[start..].find(&q) {
                let byte = start + pos;
                let col = low[..byte].chars().count();
                self.search.matches.push((abs, col, q.chars().count()));
                start = byte + q.len().max(1);
                if start >= low.len() {
                    break;
                }
            }
        }
        self.search.max = max;
        if !self.search.matches.is_empty() {
            self.search.current = self.search.matches.len() - 1;
            self.goto_match();
        }
    }

    pub fn next_match(&mut self, forward: bool) {
        let n = self.search.matches.len();
        if n == 0 {
            return;
        }
        self.search.current = if forward { (self.search.current + 1) % n } else { (self.search.current + n - 1) % n };
        self.goto_match();
    }

    fn goto_match(&mut self) {
        let Some(&(abs, _, _)) = self.search.matches.get(self.search.current) else { return };
        let half = (self.rows / 2) as usize;
        let top = abs.saturating_sub(half);
        let off = self.search.max.saturating_sub(top);
        self.set_scroll(off);
    }

    // ── мышь для приложений (htop, mc, vim) ────────

    fn mouse_report(&mut self, button: u8, row: u16, col: u16, press: bool) -> bool {
        let (mode, enc) = match self.shared.parser.lock() {
            Ok(p) => (p.screen().mouse_protocol_mode(), p.screen().mouse_protocol_encoding()),
            Err(_) => return false,
        };
        if mode == vt100::MouseProtocolMode::None {
            return false;
        }
        if !press && mode == vt100::MouseProtocolMode::Press {
            return true;
        }
        let (x, y) = (col as u32 + 1, row as u32 + 1);
        let bytes = if enc == vt100::MouseProtocolEncoding::Sgr {
            format!("\x1b[<{};{};{}{}", button, x, y, if press { 'M' } else { 'm' }).into_bytes()
        } else {
            let b = if press { button } else { 3 };
            let mut v = b"\x1b[M".to_vec();
            v.push(32 + b);
            v.push((32 + x.min(223)) as u8);
            v.push((32 + y.min(223)) as u8);
            v
        };
        self.send(Msg::Data(bytes));
        true
    }

    fn mouse_mode_on(&self) -> bool {
        self.shared
            .parser
            .lock()
            .map(|p| p.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None)
            .unwrap_or(false)
    }

    fn alt_screen(&self) -> bool {
        self.shared.parser.lock().map(|p| p.screen().alternate_screen()).unwrap_or(false)
    }

    // ── кэш рендера ─────────────────────────────────

    #[allow(clippy::too_many_arguments)]
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
        let guard = self.shared.parser.lock().unwrap();
        let screen = guard.screen();
        let (srows, scols) = {
            let (r, c) = screen.size();
            (r.get(), c.get())
        };
        let mut galleys = Vec::with_capacity(srows as usize);
        let mut bg = Vec::new();
        let mut lines = Vec::with_capacity(srows as usize);

        for r in 0..srows {
            let mut job = egui::text::LayoutJob::default();
            let mut line = String::with_capacity(scols as usize);
            let mut run = String::new();
            let mut run_fg = DEFAULT_FG;
            let mut run_on = false;
            let mut bg_cur: Option<(u16, Color32)> = None;
            let mut bg_len = 0u16;

            for c in 0..scols {
                let cell = screen.cell(r, c);
                if let Some(cell) = cell {
                    if cell.is_wide_continuation() {
                        continue; // широкий символ уже занял две клетки
                    }
                }
                let (ch, fg, bgc) = match cell {
                    Some(cell) => {
                        let s = cell.contents();
                        let s = if s.is_empty() { " " } else { s };
                        let mut fg = to_color(cell.fgcolor(), DEFAULT_FG);
                        let mut bgc = match cell.bgcolor() {
                            vt100::Color::Default => None,
                            other => Some(to_color(other, TERM_BG)),
                        };
                        if cell.inverse() {
                            let rb = bgc.unwrap_or(TERM_BG);
                            bgc = Some(fg);
                            fg = rb;
                        }
                        if cell.bold() {
                            fg = brighten(fg);
                        }
                        if cell.dim() {
                            fg = fg.gamma_multiply(0.6);
                        }
                        (s, fg, bgc)
                    }
                    None => (" ", DEFAULT_FG, None),
                };
                line.push_str(ch);

                if run_on && fg == run_fg {
                    run.push_str(ch);
                } else {
                    if run_on {
                        job.append(&run, 0.0, fmt(font_id, run_fg));
                    }
                    run.clear();
                    run.push_str(ch);
                    run_fg = fg;
                    run_on = true;
                }

                let width = if cell.map(|c| c.is_wide()).unwrap_or(false) { 2 } else { 1 };
                match bg_cur {
                    Some((_, col)) if bgc == Some(col) => bg_len += width,
                    Some((start, col)) => {
                        bg.push((r, start, bg_len, col));
                        bg_cur = bgc.map(|nc| (c, nc));
                        bg_len = if bgc.is_some() { width } else { 0 };
                    }
                    None => {
                        if let Some(nc) = bgc {
                            bg_cur = Some((c, nc));
                            bg_len = width;
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
            job.wrap.max_rows = 1;
            galleys.push(ctx.fonts_mut(|f| f.layout_job(job)));
            lines.push(line);
        }

        let cursor = screen.cursor_position();
        let cursor_hidden = screen.hide_cursor();
        drop(guard);
        self.cache = RenderCache { gen_id, cols, rows, font, ch_w, ch_h, galleys, bg, cursor, cursor_hidden, lines };
    }
}

fn fmt(font_id: &egui::FontId, color: Color32) -> egui::text::TextFormat {
    egui::text::TextFormat { font_id: font_id.clone(), color, ..Default::default() }
}

fn brighten(c: Color32) -> Color32 {
    let f = |v: u8| v.saturating_add(((255 - v as u16) / 4) as u8);
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

// ───────────────────────── цвета ─────────────────────────

const DEFAULT_FG: Color32 = Color32::from_rgb(0xc5, 0xcd, 0xd8);
pub const TERM_BG: Color32 = Color32::from_rgb(0x0a, 0x0e, 0x14);
const CURSOR: Color32 = Color32::from_rgb(0xd7, 0xde, 0xe8);
const SELECTION: Color32 = Color32::from_rgba_premultiplied(0x2a, 0x55, 0x8c, 0xa0);
const MATCH: Color32 = Color32::from_rgba_premultiplied(0x80, 0x66, 0x10, 0x90);
const MATCH_CUR: Color32 = Color32::from_rgba_premultiplied(0xd0, 0x90, 0x10, 0xc0);

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
            let c = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (c(i / 36), c((i % 36) / 6), c(i % 6))
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

// ───────────────────────── клавиатура ─────────────────────────

fn xterm_mod(m: egui::Modifiers) -> u8 {
    1 + (m.shift as u8) + 2 * (m.alt as u8) + 4 * (m.ctrl as u8)
}

/// Спец-клавиши → escape-последовательности xterm (учёт режима курсора и модификаторов).
pub fn encode_key(key: egui::Key, m: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    use egui::Key::*;
    let md = xterm_mod(m);
    let csi_letter = |l: char| -> Vec<u8> {
        if md > 1 {
            format!("\x1b[1;{md}{l}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{l}").into_bytes()
        } else {
            format!("\x1b[{l}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if md > 1 { format!("\x1b[{n};{md}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };
    let ss3 = |l: char| -> Vec<u8> {
        if md > 1 { format!("\x1b[1;{md}{l}").into_bytes() } else { format!("\x1bO{l}").into_bytes() }
    };
    Some(match key {
        Enter => {
            if m.alt { b"\x1b\r".to_vec() } else { vec![b'\r'] }
        }
        Backspace => {
            if m.ctrl { vec![0x08] } else if m.alt { b"\x1b\x7f".to_vec() } else { vec![0x7f] }
        }
        Tab => {
            if m.shift { b"\x1b[Z".to_vec() } else { vec![b'\t'] }
        }
        Escape => vec![0x1b],
        ArrowUp => csi_letter('A'),
        ArrowDown => csi_letter('B'),
        ArrowRight => csi_letter('C'),
        ArrowLeft => csi_letter('D'),
        Home => csi_letter('H'),
        End => csi_letter('F'),
        Insert => tilde(2),
        Delete => tilde(3),
        PageUp => tilde(5),
        PageDown => tilde(6),
        F1 => ss3('P'),
        F2 => ss3('Q'),
        F3 => ss3('R'),
        F4 => ss3('S'),
        F5 => tilde(15),
        F6 => tilde(17),
        F7 => tilde(18),
        F8 => tilde(19),
        F9 => tilde(20),
        F10 => tilde(21),
        F11 => tilde(23),
        F12 => tilde(24),
        Space if m.ctrl => vec![0],
        _ => return None,
    })
}

/// Ctrl + клавиша → управляющий байт.
pub fn ctrl_byte(key: egui::Key) -> Option<u8> {
    use egui::Key::*;
    Some(match key {
        A => 1, B => 2, C => 3, D => 4, E => 5, F => 6, G => 7, H => 8,
        I => 9, J => 10, K => 11, L => 12, M => 13, N => 14, O => 15, P => 16,
        Q => 17, R => 18, S => 19, T => 20, U => 21, V => 22, W => 23, X => 24,
        Y => 25, Z => 26,
        OpenBracket => 27,
        Backslash => 28,
        CloseBracket => 29,
        Slash | Minus => 31,
        _ => return None,
    })
}

/// Alt + буква → ESC + буква (для readline: Alt+B/F/D и т.п.).
pub fn alt_letter(key: egui::Key, shift: bool) -> Option<Vec<u8>> {
    let b = ctrl_byte(key)?;
    if !(1..=26).contains(&b) {
        return None;
    }
    let ch = if shift { b'A' + b - 1 } else { b'a' + b - 1 };
    Some(vec![0x1b, ch])
}

// ───────────────────────── рендер ─────────────────────────

pub struct DrawOpts {
    pub font_size: f32,
    pub focused: bool,
    pub blink: bool,
    pub copy_on_select: bool,
    pub right_click_paste: bool,
    pub broadcast: bool,
}

pub fn draw_terminal(ui: &mut egui::Ui, sess: &mut TermSession, o: &DrawOpts) -> TermUi {
    let mut out = TermUi::default();
    let ctx = ui.ctx().clone();
    let font_id = egui::FontId::monospace(o.font_size);

    let (ch_w, ch_h) = if sess.cache.font == o.font_size && sess.cache.ch_w > 0.0 {
        (sess.cache.ch_w, sess.cache.ch_h)
    } else {
        ctx.fonts_mut(|f| (f.glyph_width(&font_id, 'M'), f.row_height(&font_id)))
    };

    let avail = ui.available_size();
    let cols = ((avail.x / ch_w).floor() as u16).max(8);
    let rows = ((avail.y / ch_h).floor() as u16).max(2);
    sess.resize(rows, cols);

    let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, TERM_BG);

    let g = sess.generation();
    if g != sess.cache.gen_id || cols != sess.cache.cols || rows != sess.cache.rows || o.font_size != sess.cache.font {
        if !sess.selecting && g != sess.cache.gen_id && sess.scroll == 0 {
            sess.sel = None;
        }
        sess.rebuild_cache(&ctx, &font_id, cols, rows, ch_w, ch_h, o.font_size, g);
    }
    sess.seen_gen = g;

    // ── мышь ──
    let cell_at = |p: egui::Pos2| -> (u16, u16) {
        let c = ((p.x - rect.left()) / ch_w).floor().clamp(0.0, (cols - 1) as f32) as u16;
        let r = ((p.y - rect.top()) / ch_h).floor().clamp(0.0, (rows - 1) as f32) as u16;
        (r, c)
    };
    let shift = ui.input(|i| i.modifiers.shift);
    let app_mouse = sess.mouse_mode_on() && !shift;

    if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
        out.clicked = true;
    }

    if app_mouse {
        if let Some(p) = resp.interact_pointer_pos() {
            let (r, c) = cell_at(p);
            if ui.input(|i| i.pointer.primary_pressed()) {
                sess.mouse_report(0, r, c, true);
            }
            if ui.input(|i| i.pointer.primary_released()) {
                sess.mouse_report(0, r, c, false);
            }
        }
    } else {
        if resp.drag_started_by(egui::PointerButton::Primary) {
            if let Some(p) = resp.interact_pointer_pos() {
                let rc = cell_at(p);
                sess.sel = Some((rc, rc));
                sess.selecting = true;
            }
        }
        if sess.selecting && resp.dragged_by(egui::PointerButton::Primary) {
            if let (Some(p), Some((a, _))) = (resp.interact_pointer_pos(), sess.sel) {
                sess.sel = Some((a, cell_at(p)));
            }
        }
        if sess.selecting && resp.drag_stopped() {
            sess.selecting = false;
            if o.copy_on_select {
                out.copied = sess.selection_text();
            }
        }
        if resp.double_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (r, c) = cell_at(p);
                sess.select_word(r, c);
                if o.copy_on_select {
                    out.copied = sess.selection_text();
                }
            }
        } else if resp.clicked() {
            sess.sel = None;
        }
    }

    // Колесо: история / стрелки в полноэкранных программах / отчёт мыши.
    if resp.hovered() {
        let (dy, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
        if dy != 0.0 && !ctrl {
            sess.scroll_accum += dy;
            let lines = (sess.scroll_accum / ch_h).trunc();
            if lines != 0.0 {
                sess.scroll_accum -= lines * ch_h;
                let n = lines.abs() as usize * 1;
                let up = lines > 0.0;
                if app_mouse {
                    let pos = ui.input(|i| i.pointer.hover_pos()).unwrap_or(rect.center());
                    let (r, c) = cell_at(pos);
                    for _ in 0..n {
                        sess.mouse_report(if up { 64 } else { 65 }, r, c, true);
                    }
                } else if sess.alt_screen() {
                    let key = if up { b"\x1b[A" } else { b"\x1b[B" };
                    for _ in 0..n * 3 {
                        sess.send(Msg::Data(key.to_vec()));
                    }
                } else {
                    sess.scroll_by(if up { (n * 3) as isize } else { -((n * 3) as isize) });
                }
            }
        }
    }

    // Правая кнопка: вставка (опция в стиле PuTTY) или контекстное меню.
    if resp.secondary_clicked() && o.right_click_paste {
        out.paste = true;
    } else if !o.right_click_paste {
        let has_sel = sess.has_selection();
        resp.context_menu(|ui| {
            if ui.add_enabled(has_sel, egui::Button::new("Копировать   Ctrl+Shift+C")).clicked() {
                out.copied = sess.selection_text();
                ui.close();
            }
            if ui.button("Вставить        Ctrl+Shift+V").clicked() {
                out.paste = true;
                ui.close();
            }
            if ui.button("Выделить экран").clicked() {
                sess.select_all_visible();
                ui.close();
            }
            ui.separator();
            if ui.button("Найти…          Ctrl+Shift+F").clicked() {
                out.open_search = true;
                ui.close();
            }
            if ui.button("Переподключить  Ctrl+Shift+R").clicked() {
                out.reconnect = true;
                ui.close();
            }
        });
    }

    // ── отрисовка ──
    let cache = &sess.cache;
    for &(r, c, len, color) in &cache.bg {
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(rect.left() + c as f32 * ch_w, rect.top() + r as f32 * ch_h),
                egui::vec2(len as f32 * ch_w, ch_h),
            ),
            0.0,
            color,
        );
    }

    // Совпадения поиска на видимом экране.
    if !sess.search.matches.is_empty() {
        let top_abs = sess.search.max.saturating_sub(sess.scroll);
        for (i, &(abs, col, len)) in sess.search.matches.iter().enumerate() {
            if abs < top_abs || abs >= top_abs + rows as usize {
                continue;
            }
            let r = (abs - top_abs) as f32;
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left() + col as f32 * ch_w, rect.top() + r * ch_h),
                    egui::vec2(len as f32 * ch_w, ch_h),
                ),
                2.0,
                if i == sess.search.current { MATCH_CUR } else { MATCH },
            );
        }
    }

    for (r, gal) in cache.galleys.iter().enumerate() {
        painter.galley(egui::pos2(rect.left(), rect.top() + r as f32 * ch_h), gal.clone(), DEFAULT_FG);
    }

    // Выделение поверх текста.
    if let Some(((r0, c0), (r1, c1))) = sess.sel_norm() {
        for r in r0..=r1 {
            let cs = if r == r0 { c0 } else { 0 };
            let ce = if r == r1 { c1 } else { cols - 1 };
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left() + cs as f32 * ch_w, rect.top() + r as f32 * ch_h),
                    egui::pos2(rect.left() + (ce + 1) as f32 * ch_w, rect.top() + (r + 1) as f32 * ch_h),
                ),
                0.0,
                SELECTION,
            );
        }
    }

    // Курсор: сплошной в фокусе, контурный без фокуса; не рисуем при прокрутке истории.
    if sess.is_alive() && !cache.cursor_hidden && sess.scroll == 0 {
        let (cr, cc) = cache.cursor;
        let cur = egui::Rect::from_min_size(
            egui::pos2(rect.left() + cc as f32 * ch_w, rect.top() + cr as f32 * ch_h),
            egui::vec2(ch_w, ch_h),
        );
        if o.focused {
            let on = !o.blink || (ui.input(|i| i.time) * 1000.0) as u64 / 530 % 2 == 0;
            if on {
                painter.rect_filled(cur, 0.0, CURSOR.gamma_multiply(0.75));
            }
        } else {
            painter.rect_stroke(cur, 0.0, egui::Stroke::new(1.0, CURSOR.gamma_multiply(0.6)), egui::StrokeKind::Inside);
        }
    }

    // Индикатор прокрутки истории.
    if sess.scroll > 0 {
        let txt = format!("↑ история: {} строк  ·  Shift+End — вниз", sess.scroll);
        let gal = ctx.fonts_mut(|f| f.layout_no_wrap(txt, egui::FontId::proportional(12.0), Color32::BLACK));
        let pad = egui::vec2(8.0, 3.0);
        let r = egui::Rect::from_min_size(
            egui::pos2(rect.right() - gal.size().x - pad.x * 2.0 - 8.0, rect.top() + 6.0),
            gal.size() + pad * 2.0,
        );
        painter.rect_filled(r, 4.0, Color32::from_rgb(0xe5, 0xc0, 0x7b));
        painter.galley(r.min + pad, gal, Color32::BLACK);
    }

    if o.broadcast && !sess.broadcast_excluded {
        painter.rect_stroke(rect.shrink(1.0), 0.0, egui::Stroke::new(2.0, Color32::from_rgb(0xe0, 0x6c, 0x75)), egui::StrokeKind::Inside);
    } else if o.focused {
        if let Some([r, g, b]) = sess.color {
            painter.rect_stroke(rect.shrink(1.0), 0.0, egui::Stroke::new(2.0, Color32::from_rgb(r, g, b)), egui::StrokeKind::Inside);
        }
    }

    if resp.hovered() {
        ctx.set_cursor_icon(if app_mouse { egui::CursorIcon::Default } else { egui::CursorIcon::Text });
    }
    out
}

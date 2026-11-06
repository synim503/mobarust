//! Модель данных и надёжное хранение конфигурации.
//!
//! * атомарная запись (временный файл + переименование) — CFG-01;
//! * 3 резервные копии, битый файл никогда не перезаписывается молча;
//! * версия схемы и миграции со старых форматов;
//! * портативный режим: данные рядом с exe, если рядом лежит файл `portable` — CFG-02.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

pub const SCHEMA: u32 = 3;

/// Способ входа, сохраняемый в профиле (без секретов).
/// `Password` исторически означает «авто»: агент → ключи по умолчанию → пароль / 2FA.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub enum AuthMethod {
    #[default]
    Password,
    /// Явный файл приватного ключа (IdentityFile).
    Key { path: String },
}

impl AuthMethod {
    pub fn identity(&self) -> Option<&str> {
        match self {
            AuthMethod::Key { path } if !path.trim().is_empty() => Some(path.as_str()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SessionKind {
    Local {
        shell: Option<String>,
    },
    Ssh {
        host: String,
        port: u16,
        user: String,
        #[serde(default)]
        auth: AuthMethod,
        /// Цепочка jump-хостов: `user@host:port, user2@host2` (как ProxyJump).
        #[serde(default)]
        jump: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionDef {
    pub id: u64,
    pub name: String,
    pub group: String,
    pub kind: SessionKind,
    /// Цвет метки вкладки (например, красный для prod).
    #[serde(default)]
    pub color: Option<[u8; 3]>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub use_count: u32,
    #[serde(default)]
    pub last_used: u64,
}

impl SessionDef {
    pub fn host_label(&self) -> String {
        match &self.kind {
            SessionKind::Local { shell } => shell.clone().unwrap_or_else(|| "локальный shell".into()),
            SessionKind::Ssh { host, user, port, auth, jump } => {
                let mut s = format!("{user}@{host}:{port}");
                if let Some(k) = auth.identity() {
                    s.push_str(&format!("\nключ: {k}"));
                }
                if !jump.trim().is_empty() {
                    s.push_str(&format!("\nчерез: {jump}"));
                }
                s
            }
        }
    }

    /// Строка для нечёткого поиска.
    pub fn haystack(&self) -> String {
        let mut s = format!("{} {} {}", self.name, self.group, self.tags.join(" "));
        if let SessionKind::Ssh { host, user, .. } = &self.kind {
            s.push_str(&format!(" {user}@{host} {host} {user}"));
        }
        s.to_lowercase()
    }

    pub fn is_ssh(&self) -> bool {
        matches!(self.kind, SessionKind::Ssh { .. })
    }
}

/// Пользовательские настройки.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub font_size: f32,
    pub scrollback: usize,
    pub copy_on_select: bool,
    pub right_click_paste: bool,
    pub confirm_multiline_paste: bool,
    pub keepalive_secs: u64,
    pub auto_reconnect: bool,
    pub cursor_blink: bool,
    pub sftp_auto_open: bool,
    pub restore_tabs: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 13.5,
            scrollback: 5000,
            copy_on_select: false,
            right_click_paste: false,
            confirm_multiline_paste: true,
            keepalive_secs: 15,
            auto_reconnect: true,
            cursor_blink: true,
            sftp_auto_open: true,
            restore_tabs: true,
        }
    }
}

/// Быстрая команда (сниппет); Ctrl+F1…F12 — первые 12.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snippet {
    pub name: String,
    pub text: String,
    #[serde(default = "yes")]
    pub send_enter: bool,
}

fn yes() -> bool {
    true
}

/// Вкладка для восстановления после перезапуска (без секретов).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OpenTab {
    Session(u64),
    Local { title: String, shell: Option<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub schema: u32,
    pub sessions: Vec<SessionDef>,
    #[serde(default)]
    pub folders: Vec<String>,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub snippets: Vec<Snippet>,
    #[serde(default)]
    pub open_tabs: Vec<OpenTab>,
    #[serde(default)]
    pub next_id: u64,
    /// Устаревшее поле (схема 1–2), переносится в settings.
    #[serde(default, skip_serializing)]
    font_size: Option<f32>,
}

static BACKED_UP: AtomicBool = AtomicBool::new(false);

/// Каталог данных: портативный (рядом с exe) или профиль пользователя.
pub fn data_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir.join("portable").exists() || dir.join("data").join("config.json").exists() {
                return dir.join("data");
            }
        }
    }
    directories::ProjectDirs::from("dev", "mobarust", "MobaRust")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn is_portable() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("portable").exists()))
        .unwrap_or(false)
}

/// Атомарная запись файла: tmp → fsync → rename.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Config {
    pub fn path() -> PathBuf {
        data_dir().join("config.json")
    }

    fn backup_path(n: usize) -> PathBuf {
        data_dir().join(format!("config.bak{n}.json"))
    }

    /// Загрузить конфиг. Если основной файл битый — берём резервную копию,
    /// а битый файл откладываем в сторону (никогда не теряем данные).
    pub fn load() -> Self {
        let main = Self::path();
        if let Ok(bytes) = std::fs::read(&main) {
            match serde_json::from_slice::<Config>(&bytes) {
                Ok(mut cfg) => {
                    cfg.migrate();
                    return cfg;
                }
                Err(_) => {
                    let broken = data_dir().join(format!("config.broken-{}.json", now_secs()));
                    let _ = std::fs::copy(&main, broken);
                    for n in 1..=3 {
                        if let Ok(b) = std::fs::read(Self::backup_path(n)) {
                            if let Ok(mut cfg) = serde_json::from_slice::<Config>(&b) {
                                cfg.migrate();
                                return cfg;
                            }
                        }
                    }
                }
            }
        }
        Self::seeded()
    }

    pub fn save(&self) {
        let main = Self::path();
        // Ротация резервных копий — один раз за запуск, до первой записи.
        if !BACKED_UP.swap(true, Ordering::SeqCst) && main.exists() {
            for n in (1..3).rev() {
                let _ = std::fs::rename(Self::backup_path(n), Self::backup_path(n + 1));
            }
            let _ = std::fs::copy(&main, Self::backup_path(1));
        }
        let mut out = self.clone();
        out.schema = SCHEMA;
        if let Ok(json) = serde_json::to_vec_pretty(&out) {
            let _ = write_atomic(&main, &json);
        }
    }

    fn migrate(&mut self) {
        if let Some(fs) = self.font_size.take() {
            if fs > 0.0 {
                self.settings.font_size = fs;
            }
        }
        let groups: Vec<String> = self.sessions.iter().map(|s| s.group.clone()).collect();
        for g in groups {
            self.ensure_folder(&g);
        }
        if self.settings.font_size <= 0.0 {
            self.settings.font_size = Settings::default().font_size;
        }
        if self.settings.scrollback == 0 {
            self.settings.scrollback = Settings::default().scrollback;
        }
        let max_id = self.sessions.iter().map(|s| s.id).max().unwrap_or(0);
        if self.next_id < max_id {
            self.next_id = max_id;
        }
        self.schema = SCHEMA;
    }

    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn ensure_folder(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && !self.folders.iter().any(|f| f == name) {
            self.folders.push(name.to_string());
        }
    }

    pub fn all_folders(&self) -> Vec<String> {
        let mut set = self.folders.clone();
        for s in &self.sessions {
            if !set.iter().any(|f| f == &s.group) {
                set.push(s.group.clone());
            }
        }
        set.sort_by_key(|a| a.to_lowercase());
        set
    }

    pub fn find(&self, id: u64) -> Option<&SessionDef> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut SessionDef> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Найти SSH-сессию по адресу.
    pub fn find_ssh(&self, host: &str, user: &str, port: u16) -> Option<u64> {
        self.sessions.iter().find_map(|s| match &s.kind {
            SessionKind::Ssh { host: h, user: u, port: p, .. } if h == host && u == user && *p == port => {
                Some(s.id)
            }
            _ => None,
        })
    }

    /// Вставить новую сессию или обновить существующую (дедуп по host+user+port).
    pub fn upsert(&mut self, mut def: SessionDef) -> u64 {
        self.ensure_folder(&def.group);
        if def.id != 0 {
            if let Some(cur) = self.find_mut(def.id) {
                def.use_count = cur.use_count;
                def.last_used = cur.last_used;
                *cur = def.clone();
                return def.id;
            }
        }
        if let SessionKind::Ssh { host, user, port, .. } = &def.kind {
            if let Some(id) = self.find_ssh(host, user, *port) {
                let cur = self.find_mut(id).unwrap();
                def.id = id;
                def.use_count = cur.use_count;
                def.last_used = cur.last_used;
                *cur = def;
                return id;
            }
        }
        def.id = self.alloc_id();
        let id = def.id;
        self.sessions.push(def);
        id
    }

    pub fn touch(&mut self, id: u64) {
        if let Some(s) = self.find_mut(id) {
            s.use_count = s.use_count.saturating_add(1);
            s.last_used = now_secs();
        }
    }

    pub fn delete(&mut self, id: u64) {
        self.sessions.retain(|s| s.id != id);
    }

    pub fn rename_folder(&mut self, from: &str, to: &str) {
        let to = to.trim();
        if to.is_empty() || from == to {
            return;
        }
        for s in &mut self.sessions {
            if s.group == from {
                s.group = to.to_string();
            }
        }
        for f in &mut self.folders {
            if f == from {
                *f = to.to_string();
            }
        }
        self.folders.dedup();
    }

    /// Удалить папку; её сессии переезжают в `move_to`.
    pub fn delete_folder(&mut self, name: &str, move_to: &str) {
        for s in &mut self.sessions {
            if s.group == name {
                s.group = move_to.to_string();
            }
        }
        self.folders.retain(|f| f != name);
        self.ensure_folder(move_to);
    }

    /// Недавние сессии (по времени последнего использования).
    pub fn recent(&self, n: usize) -> Vec<SessionDef> {
        let mut v: Vec<SessionDef> = self.sessions.iter().filter(|s| s.last_used > 0).cloned().collect();
        v.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        v.truncate(n);
        v
    }

    fn seeded() -> Self {
        let mut cfg = Config {
            schema: SCHEMA,
            folders: vec!["SSH".into()],
            snippets: vec![
                Snippet { name: "Нагрузка".into(), text: "uptime && free -h && df -h /".into(), send_enter: true },
                Snippet { name: "Топ процессов".into(), text: "ps aux --sort=-%cpu | head -15".into(), send_enter: true },
                Snippet { name: "Логи (journal)".into(), text: "journalctl -n 100 --no-pager".into(), send_enter: true },
            ],
            ..Default::default()
        };
        cfg.settings = Settings::default();
        cfg
    }
}

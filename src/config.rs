//! Модель сессий и их хранение на диске (JSON в конфиг-директории ОС).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Тип подключения.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SessionKind {
    /// Локальный шелл (bash / pwsh / cmd). `shell = None` → шелл по умолчанию.
    Local { shell: Option<String> },
    /// SSH-подключение. Пароль не хранится — запрашивается при подключении.
    Ssh { host: String, port: u16, user: String },
}

/// Описание сохранённой сессии в менеджере.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionDef {
    pub id: u64,
    pub name: String,
    pub group: String,
    pub kind: SessionKind,
}

impl SessionDef {
    pub fn host_label(&self) -> String {
        match &self.kind {
            SessionKind::Local { .. } => "local shell".into(),
            SessionKind::Ssh { host, user, port } => format!("{user}@{host}:{port}"),
        }
    }
}

/// Корневой конфиг приложения.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub sessions: Vec<SessionDef>,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default)]
    pub next_id: u64,
}

fn default_font_size() -> f32 {
    13.5
}

impl Config {
    fn path() -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("dev", "mobarust", "MobaRust")?;
        let dir = dirs.config_dir().to_path_buf();
        Some(dir.join("config.json"))
    }

    /// Загрузить конфиг или вернуть набор по умолчанию с примерами сессий.
    pub fn load() -> Self {
        if let Some(p) = Self::path() {
            if let Ok(bytes) = std::fs::read(&p) {
                if let Ok(cfg) = serde_json::from_slice::<Config>(&bytes) {
                    return cfg;
                }
            }
        }
        Self::seeded()
    }

    pub fn save(&self) {
        if let Some(p) = Self::path() {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_vec_pretty(self) {
                let _ = std::fs::write(&p, json);
            }
        }
    }

    /// Выдать новый уникальный id.
    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Стартовый набор: локальный шелл + пример SSH.
    fn seeded() -> Self {
        let mut cfg = Config {
            sessions: vec![],
            font_size: default_font_size(),
            next_id: 0,
        };
        let id1 = cfg.alloc_id();
        cfg.sessions.push(SessionDef {
            id: id1,
            name: "Локальный шелл".into(),
            group: "Локальные".into(),
            kind: SessionKind::Local { shell: None },
        });
        let id2 = cfg.alloc_id();
        cfg.sessions.push(SessionDef {
            id: id2,
            name: "example".into(),
            group: "SSH".into(),
            kind: SessionKind::Ssh {
                host: "example.com".into(),
                port: 22,
                user: "user".into(),
            },
        });
        cfg
    }
}

//! Модель сессий и их хранение на диске (JSON в конфиг-директории ОС).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Способ аутентификации SSH (без секретов — для хранения).
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub enum AuthMethod {
    #[default]
    Password,
    /// Приватный ключ по пути. Passphrase не хранится.
    Key {
        path: String,
    },
}

/// Тип подключения.
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
    },
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
            SessionKind::Ssh { host, user, port, auth } => {
                let m = match auth {
                    AuthMethod::Password => "pass",
                    AuthMethod::Key { .. } => "key",
                };
                format!("{user}@{host}:{port} · {m}")
            }
        }
    }
}

/// Корневой конфиг приложения.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub sessions: Vec<SessionDef>,
    #[serde(default)]
    pub folders: Vec<String>,
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
        Some(dirs.config_dir().join("config.json"))
    }

    pub fn load() -> Self {
        if let Some(p) = Self::path() {
            if let Ok(bytes) = std::fs::read(&p) {
                if let Ok(mut cfg) = serde_json::from_slice::<Config>(&bytes) {
                    cfg.normalize();
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

    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Гарантировать наличие папки в списке (для пустых папок тоже).
    pub fn ensure_folder(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && !self.folders.iter().any(|f| f == name) {
            self.folders.push(name.to_string());
        }
    }

    /// Добавить / обновить SSH-сессию (дедупликация по host+user+port).
    /// Возвращает true, если список изменился.
    pub fn upsert_ssh(
        &mut self,
        name: &str,
        folder: &str,
        host: &str,
        port: u16,
        user: &str,
        auth: AuthMethod,
    ) -> bool {
        self.ensure_folder(folder);
        if let Some(def) = self.sessions.iter_mut().find(|s| {
            matches!(&s.kind, SessionKind::Ssh { host: h, user: u, port: p, .. }
                if h == host && u == user && *p == port)
        }) {
            // Обновляем метаданные существующей.
            def.name = name.to_string();
            def.group = folder.to_string();
            def.kind = SessionKind::Ssh {
                host: host.to_string(),
                port,
                user: user.to_string(),
                auth,
            };
            return true;
        }
        let id = self.alloc_id();
        self.sessions.push(SessionDef {
            id,
            name: name.to_string(),
            group: folder.to_string(),
            kind: SessionKind::Ssh {
                host: host.to_string(),
                port,
                user: user.to_string(),
                auth,
            },
        });
        true
    }

    /// Все папки: из списка ∪ использованные сессиями (отсортированы).
    pub fn all_folders(&self) -> Vec<String> {
        let mut set: Vec<String> = self.folders.clone();
        for s in &self.sessions {
            if !set.iter().any(|f| f == &s.group) {
                set.push(s.group.clone());
            }
        }
        set.sort();
        set
    }

    /// Привести к согласованному виду (папки из сессий попадают в список).
    fn normalize(&mut self) {
        let groups: Vec<String> = self.sessions.iter().map(|s| s.group.clone()).collect();
        for g in groups {
            self.ensure_folder(&g);
        }
        if self.font_size <= 0.0 {
            self.font_size = default_font_size();
        }
    }

    fn seeded() -> Self {
        let mut cfg = Config {
            sessions: vec![],
            folders: vec!["SSH".into(), "Локальные".into()],
            font_size: default_font_size(),
            next_id: 0,
        };
        let id = cfg.alloc_id();
        cfg.sessions.push(SessionDef {
            id,
            name: "example".into(),
            group: "SSH".into(),
            kind: SessionKind::Ssh {
                host: "example.com".into(),
                port: 22,
                user: "user".into(),
                auth: AuthMethod::Password,
            },
        });
        cfg
    }
}

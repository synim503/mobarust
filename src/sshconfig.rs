//! Импорт `~/.ssh/config` (SES-03): Host, HostName, User, Port, IdentityFile,
//! ProxyJump, Include. Шаблоны (`*`, `?`, `!`) как отдельные хосты не создаются,
//! но `Host *` даёт значения по умолчанию. Неизвестные директивы считаются в отчёте.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct Imported {
    pub alias: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity: Option<String>,
    pub jump: String,
}

#[derive(Default)]
pub struct Report {
    pub hosts: Vec<Imported>,
    pub ignored: BTreeSet<String>,
    pub files: usize,
}

#[derive(Default, Clone)]
struct Block {
    patterns: Vec<String>,
    host: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<String>,
    jump: Option<String>,
}

pub fn home() -> PathBuf {
    directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn expand_tilde(p: &str) -> String {
    let p = p.trim().trim_matches('"');
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        return home().join(rest).to_string_lossy().into_owned();
    }
    p.to_string()
}

pub fn default_path() -> PathBuf {
    home().join(".ssh").join("config")
}

pub fn import(path: &Path) -> Report {
    let mut rep = Report::default();
    let mut blocks: Vec<Block> = Vec::new();
    parse_file(path, &mut blocks, &mut rep, 0);

    // Значения по умолчанию из `Host *`.
    let mut defaults = Block::default();
    for b in &blocks {
        if b.patterns.iter().any(|p| p == "*") {
            if defaults.user.is_none() {
                defaults.user = b.user.clone();
            }
            if defaults.identity.is_none() {
                defaults.identity = b.identity.clone();
            }
            if defaults.port.is_none() {
                defaults.port = b.port;
            }
        }
    }

    let fallback_user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "root".into());

    for b in &blocks {
        for pat in &b.patterns {
            if pat.contains(['*', '?', '!']) {
                continue;
            }
            if rep.hosts.iter().any(|h| &h.alias == pat) {
                continue; // первое совпадение выигрывает, как в OpenSSH
            }
            rep.hosts.push(Imported {
                alias: pat.clone(),
                host: b.host.clone().unwrap_or_else(|| pat.clone()),
                user: b
                    .user
                    .clone()
                    .or_else(|| defaults.user.clone())
                    .unwrap_or_else(|| fallback_user.clone()),
                port: b.port.or(defaults.port).unwrap_or(22),
                identity: b.identity.clone().or_else(|| defaults.identity.clone()),
                jump: b.jump.clone().unwrap_or_default(),
            });
        }
    }
    rep
}

fn parse_file(path: &Path, blocks: &mut Vec<Block>, rep: &mut Report, depth: u8) {
    if depth > 8 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    rep.files += 1;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, val) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.trim().to_lowercase(), v.trim().trim_start_matches('=').trim().to_string()),
            None => continue,
        };
        match key.as_str() {
            "host" => blocks.push(Block {
                patterns: val.split_whitespace().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            "match" => blocks.push(Block::default()), // Match не поддерживается — блок изолируем
            "include" => {
                for pat in val.split_whitespace() {
                    let p = expand_tilde(pat);
                    let p = if Path::new(&p).is_absolute() {
                        PathBuf::from(p)
                    } else {
                        home().join(".ssh").join(p)
                    };
                    for f in glob(&p) {
                        parse_file(&f, blocks, rep, depth + 1);
                    }
                }
            }
            _ => {
                let Some(b) = blocks.last_mut() else {
                    rep.ignored.insert(key);
                    continue;
                };
                match key.as_str() {
                    "hostname" => b.host = Some(val),
                    "user" => b.user = Some(val),
                    "port" => b.port = val.parse().ok(),
                    "identityfile" => {
                        if b.identity.is_none() {
                            b.identity = Some(expand_tilde(&val));
                        }
                    }
                    "proxyjump" => {
                        if !val.eq_ignore_ascii_case("none") {
                            b.jump = Some(val)
                        }
                    }
                    _ => {
                        rep.ignored.insert(key);
                    }
                }
            }
        }
    }
}

/// Простейший glob: `*` только в имени файла.
fn glob(p: &Path) -> Vec<PathBuf> {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !name.contains('*') {
        return vec![p.to_path_buf()];
    }
    let (pre, suf) = name.split_once('*').unwrap_or((&name, ""));
    let dir = p.parent().unwrap_or(Path::new("."));
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|f| {
                    let n = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    n.starts_with(pre) && n.ends_with(suf) && f.is_file()
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Разобрать строку цепочки jump-хостов: `user@host:port, host2`.
pub fn parse_jump(s: &str, default_user: &str) -> Vec<(String, String, u16)> {
    s.split(',')
        .map(|x| x.trim())
        .filter(|x| !x.is_empty())
        .filter_map(|x| {
            let x = x.strip_prefix("ssh://").unwrap_or(x);
            let (user, rest) = match x.split_once('@') {
                Some((u, r)) => (u.to_string(), r),
                None => (default_user.to_string(), x),
            };
            let (host, port) = match rest.rsplit_once(':') {
                Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h.to_string(), p.parse().ok()?),
                _ => (rest.to_string(), 22),
            };
            if host.is_empty() {
                None
            } else {
                Some((user, host, port))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hosts_include_and_jump() {
        let dir = std::env::temp_dir().join(format!("mobarust-sshcfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let inc = dir.join("extra.conf");
        std::fs::write(&inc, "Host db\n  HostName 10.0.0.5\n  ProxyJump bastion\n").unwrap();
        let main = dir.join("config");
        std::fs::write(
            &main,
            format!(
                "Include {}\nHost *\n  User admin\n  ServerAliveInterval 30\nHost bastion web-*\n  HostName bastion.example.com\n  Port 2222\n",
                inc.display()
            ),
        )
        .unwrap();
        let rep = import(&main);
        assert_eq!(rep.files, 2);
        let db = rep.hosts.iter().find(|h| h.alias == "db").unwrap();
        assert_eq!(db.host, "10.0.0.5");
        assert_eq!(db.user, "admin");
        assert_eq!(db.jump, "bastion");
        let b = rep.hosts.iter().find(|h| h.alias == "bastion").unwrap();
        assert_eq!(b.port, 2222);
        assert!(!rep.hosts.iter().any(|h| h.alias.contains('*')));
        assert!(rep.ignored.contains("serveraliveinterval"));
    }

    #[test]
    fn parses_jump_chain() {
        let v = parse_jump("ops@a.example:2200, b.example", "me");
        assert_eq!(v, vec![("ops".into(), "a.example".into(), 2200), ("me".into(), "b.example".into(), 22)]);
    }
}

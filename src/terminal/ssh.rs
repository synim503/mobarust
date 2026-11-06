//! SSH-движок на russh.
//!
//! * SEC-01 — ключ хоста проверяется по `~/.ssh/known_hosts` до любой
//!   аутентификации; новый ключ — диалог с отпечатком SHA256, изменившийся —
//!   жёсткая блокировка без кнопки «продолжить всё равно».
//! * SEC-02 — агенты: `SSH_AUTH_SOCK` → `\\.\pipe\openssh-ssh-agent` → Pageant.
//! * SEC-03 — ключи OpenSSH/PEM, passphrase спрашивается и кэшируется на сессию.
//! * Пароль и keyboard-interactive (2FA/OTP) спрашиваются в окне, по желанию
//!   запоминаются через DPAPI (SEC-04).
//! * SES-04 — цепочки jump-хостов через `direct-tcpip`.
//! * REL-01/02 — keep-alive 15 с × 3, переподключение по R/Enter или автоматически.
//! * SFTP-01 — SFTP в отдельном канале: его ошибки не закрывают shell.

use super::{Msg, Shared, TabKind, TermSession, Writer};
use crate::secrets;
use russh::client::{self, Handle};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, MethodKind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

// ───────────────────────── публичные типы ─────────────────────────

#[derive(Clone, Debug)]
pub struct Hop {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity: Option<String>,
}

impl Hop {
    pub fn id(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

/// Цель подключения: jump-хосты по порядку, последний — целевой сервер.
pub struct Target {
    pub hops: Vec<Hop>,
    pub keepalive: u64,
    pub auto_reconnect: bool,
}

#[derive(Clone, PartialEq, Debug)]
pub enum ConnState {
    Connecting(String),
    Authenticating(String),
    Connected,
    Disconnected(String),
    Blocked(String),
}

pub enum PromptKind {
    HostKey { host: String, port: u16, algo: String, fingerprint: String },
    Secret { title: String, label: String, error: Option<String>, can_remember: bool },
    Kbd { title: String, instructions: String, prompts: Vec<(String, bool)> },
}

pub enum Answer {
    Trust { save: bool },
    Reject,
    Secret { value: String, remember: bool },
    Responses(Vec<String>),
    Cancel,
}

pub struct Pending {
    pub kind: PromptKind,
    tx: Option<tokio::sync::oneshot::Sender<Answer>>,
}

#[derive(Clone)]
pub struct BlockInfo {
    pub host: String,
    pub port: u16,
    pub line: usize,
    pub algo: String,
    pub fingerprint: String,
}

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone)]
pub struct Transfer {
    pub id: u64,
    pub name: String,
    pub upload: bool,
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
    pub finished: bool,
    pub local: Option<PathBuf>,
}

#[derive(Default)]
pub struct SftpView {
    pub cwd: String,
    pub entries: Vec<Entry>,
    pub loading: bool,
    pub error: Option<String>,
    pub transfers: Vec<Transfer>,
    pub requested: bool,
    next_id: u64,
}

pub enum SftpCmd {
    List(String),
    Download { path: String, is_dir: bool, dest: PathBuf },
    Upload { local: PathBuf, dir: String },
    Mkdir(String),
    Delete { path: String, is_dir: bool },
    Rename { from: String, to: String },
}

/// Состояние SSH-вкладки, видимое UI.
pub struct SshShared {
    pub state: Mutex<ConnState>,
    pub prompt: Mutex<Option<Pending>>,
    pub blocked: Mutex<Option<BlockInfo>>,
    pub sftp: Mutex<SftpView>,
    pub retry_at: Mutex<Option<Instant>>,
    pub endpoint: String,
    rejected: Mutex<bool>,
    ctx: egui::Context,
}

impl SshShared {
    fn new(ctx: &egui::Context, endpoint: String) -> Self {
        Self {
            state: Mutex::new(ConnState::Connecting(endpoint.clone())),
            prompt: Mutex::new(None),
            blocked: Mutex::new(None),
            sftp: Mutex::new(SftpView::default()),
            retry_at: Mutex::new(None),
            endpoint,
            rejected: Mutex::new(false),
            ctx: ctx.clone(),
        }
    }

    pub fn state(&self) -> ConnState {
        self.state.lock().map(|s| s.clone()).unwrap_or(ConnState::Connected)
    }

    fn set_state(&self, s: ConnState) {
        if let Ok(mut g) = self.state.lock() {
            *g = s;
        }
        self.ctx.request_repaint();
    }

    pub fn has_prompt(&self) -> bool {
        self.prompt.lock().map(|p| p.is_some()).unwrap_or(false)
    }

    /// Ответ UI на текущий запрос.
    pub fn answer(&self, a: Answer) {
        if let Ok(mut g) = self.prompt.lock() {
            if let Some(mut p) = g.take() {
                if let Some(tx) = p.tx.take() {
                    let _ = tx.send(a);
                }
            }
        }
    }

    async fn ask(&self, kind: PromptKind) -> Answer {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Ok(mut g) = self.prompt.lock() {
            *g = Some(Pending { kind, tx: Some(tx) });
        }
        self.ctx.request_repaint();
        rx.await.unwrap_or(Answer::Cancel)
    }

    fn sftp_update(&self, f: impl FnOnce(&mut SftpView)) {
        if let Ok(mut g) = self.sftp.lock() {
            f(&mut g);
        }
        self.ctx.request_repaint();
    }
}

// ───────────────────────── known_hosts ─────────────────────────

pub fn known_hosts_file() -> PathBuf {
    crate::sshconfig::home().join(".ssh").join("known_hosts")
}

/// Удалить устаревшую запись (явное действие пользователя после блокировки).
pub fn forget_host_key(line: usize) -> std::io::Result<()> {
    let path = known_hosts_file();
    let text = std::fs::read_to_string(&path)?;
    let backup = path.with_extension("old");
    std::fs::write(&backup, &text)?;
    let kept: Vec<&str> = text.lines().enumerate().filter(|(i, _)| i + 1 != line).map(|(_, l)| l).collect();
    crate::config::write_atomic(&path, (kept.join("\n") + "\n").as_bytes())
}

struct Client {
    ssh: Arc<SshShared>,
    host: String,
    port: u16,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let pk = key.public_key();
        let fingerprint = pk.fingerprint(HashAlg::Sha256).to_string();
        let algo = pk.algorithm().to_string();
        let path = known_hosts_file();
        match russh::keys::check_known_hosts_path(&self.host, self.port, &pk, &path) {
            Ok(true) => Ok(true),
            Err(russh::keys::Error::KeyChanged { line }) => {
                if let Ok(mut b) = self.ssh.blocked.lock() {
                    *b = Some(BlockInfo { host: self.host.clone(), port: self.port, line, algo, fingerprint });
                }
                Ok(false)
            }
            _ => {
                let kind = PromptKind::HostKey { host: self.host.clone(), port: self.port, algo, fingerprint };
                match self.ssh.ask(kind).await {
                    Answer::Trust { save } => {
                        if save {
                            if let Some(dir) = path.parent() {
                                let _ = std::fs::create_dir_all(dir);
                            }
                            let _ = russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, &pk, &path);
                        }
                        Ok(true)
                    }
                    _ => {
                        if let Ok(mut r) = self.ssh.rejected.lock() {
                            *r = true;
                        }
                        Ok(false)
                    }
                }
            }
        }
    }
}

// ───────────────────────── запуск ─────────────────────────

pub fn spawn(ctx: &egui::Context, title: &str, target: Target, rows: u16, cols: u16, scrollback: usize) -> TermSession {
    let shared = Shared::new(ctx, rows, cols, scrollback);
    let endpoint = target.hops.last().map(|h| h.id()).unwrap_or_default();
    let ssh = Arc::new(SshShared::new(ctx, endpoint));
    let (tx, rx) = unbounded_channel::<Msg>();
    {
        let shared = shared.clone();
        let ssh = ssh.clone();
        std::thread::Builder::new()
            .name("ssh".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        shared.note(&format!("Ошибка запуска: {e}"));
                        return;
                    }
                };
                rt.block_on(run(shared, ssh, target, rx, rows, cols));
            })
            .ok();
    }
    let mut t = TermSession::new(title.to_string(), TabKind::Ssh, shared, rows, cols, Writer::Tokio(tx));
    t.ssh = Some(ssh);
    t
}

enum End {
    Closed,
    Exited,
    Restart,
}

/// Ошибка, после которой не надо переподключаться автоматически.
#[derive(Debug)]
struct NoRetry(String);
impl std::fmt::Display for NoRetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for NoRetry {}

#[derive(Default)]
struct AuthCache {
    passwords: HashMap<String, String>,
    passphrases: HashMap<String, String>,
    agent_hint_shown: bool,
}

async fn run(sh: Shared, ssh: Arc<SshShared>, target: Target, mut rx: UnboundedReceiver<Msg>, rows: u16, cols: u16) {
    let mut size = (rows, cols);
    let mut cache = AuthCache::default();
    let mut attempt: u32 = 0;
    let mut ever_connected = false;

    loop {
        if let Ok(mut b) = ssh.blocked.lock() {
            *b = None;
        }
        if let Ok(mut r) = ssh.rejected.lock() {
            *r = false;
        }
        *ssh.retry_at.lock().unwrap() = None;
        ssh.set_state(ConnState::Connecting(ssh.endpoint.clone()));
        let via = if target.hops.len() > 1 {
            format!(" через {}", target.hops[..target.hops.len() - 1].iter().map(|h| h.host.as_str()).collect::<Vec<_>>().join(" → "))
        } else {
            String::new()
        };
        sh.note(&format!("# Подключение к {}{via} …", ssh.endpoint));

        let res = session(&sh, &ssh, &target, &mut rx, &mut size, &mut cache, &mut ever_connected, &mut attempt).await;
        sh.alive.store(false, Ordering::SeqCst);

        let (reason, retry) = match res {
            Ok(End::Closed) => return,
            Ok(End::Restart) => continue,
            Ok(End::Exited) => ("сеанс завершён".to_string(), false),
            Err(e) => {
                let blocked = ssh.blocked.lock().ok().and_then(|b| b.clone());
                if let Some(b) = blocked {
                    let msg = format!(
                        "КЛЮЧ ХОСТА ИЗМЕНИЛСЯ для {}:{} ({} {}). Подключение заблокировано: возможна атака «человек посередине».",
                        b.host, b.port, b.algo, b.fingerprint
                    );
                    sh.feed(format!("\r\n\x1b[1;31m# {msg}\x1b[0m\r\n").as_bytes());
                    ssh.set_state(ConnState::Blocked(msg));
                    if wait_reconnect(&mut rx, &mut size, None, &ssh).await {
                        continue;
                    }
                    return;
                }
                if *ssh.rejected.lock().unwrap() {
                    ("ключ хоста не принят".to_string(), false)
                } else if let Some(nr) = e.downcast_ref::<NoRetry>() {
                    (nr.0.clone(), false)
                } else {
                    (e.to_string(), ever_connected && target.auto_reconnect)
                }
            }
        };

        let delay = if retry {
            let secs = [2u64, 4, 8, 15, 30, 60][attempt.min(5) as usize];
            attempt += 1;
            Some(Duration::from_secs(secs))
        } else {
            None
        };
        let hint = if let Some(d) = delay {
            format!("Переподключение через {} с · R или Enter — сейчас", d.as_secs())
        } else {
            "R или Enter — переподключиться".to_string()
        };
        sh.feed(format!("\r\n\x1b[33m# Соединение закрыто: {reason}\x1b[0m\r\n\x1b[90m# {hint}\x1b[0m\r\n").as_bytes());
        ssh.set_state(ConnState::Disconnected(reason));
        if !wait_reconnect(&mut rx, &mut size, delay, &ssh).await {
            return;
        }
    }
}

/// Ждать команды переподключения (или таймера). false — вкладка закрыта.
async fn wait_reconnect(
    rx: &mut UnboundedReceiver<Msg>,
    size: &mut (u16, u16),
    delay: Option<Duration>,
    ssh: &SshShared,
) -> bool {
    let deadline = delay.map(|d| Instant::now() + d);
    *ssh.retry_at.lock().unwrap() = deadline;
    loop {
        let sleep = async {
            match deadline {
                Some(t) => tokio::time::sleep_until(t.into()).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            m = rx.recv() => match m {
                None | Some(Msg::Close) => return false,
                Some(Msg::Reconnect) => return true,
                Some(Msg::Data(d)) if d.iter().any(|b| matches!(b, b'r' | b'R' | b'\r')) => return true,
                Some(Msg::Resize(r, c)) => *size = (r, c),
                _ => {}
            },
            _ = sleep => return true,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn session(
    sh: &Shared,
    ssh: &Arc<SshShared>,
    target: &Target,
    rx: &mut UnboundedReceiver<Msg>,
    size: &mut (u16, u16),
    cache: &mut AuthCache,
    ever_connected: &mut bool,
    attempt: &mut u32,
) -> anyhow::Result<End> {
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(target.keepalive.max(5))),
        keepalive_max: 3,
        inactivity_timeout: None,
        ..Default::default()
    });
    let (handle, _jumps) = connect_chain(sh, ssh, target, config, cache).await?;

    let channel = handle.channel_open_session().await?;
    channel
        .request_pty(false, "xterm-256color", size.1 as u32, size.0 as u32, 0, 0, &[])
        .await?;
    channel.request_shell(true).await?;

    ssh.set_state(ConnState::Connected);
    *ever_connected = true;
    *attempt = 0;
    sh.alive.store(true, Ordering::SeqCst);
    sh.bump();

    let mut channel = channel;
    let mut sftp_tx: Option<UnboundedSender<SftpCmd>> = None;
    let mut exited = false;
    loop {
        tokio::select! {
            m = channel.wait() => match m {
                Some(ChannelMsg::Data { ref data }) => sh.feed(data),
                Some(ChannelMsg::ExtendedData { ref data, .. }) => sh.feed(data),
                Some(ChannelMsg::ExitStatus { .. }) | Some(ChannelMsg::ExitSignal { .. }) => exited = true,
                Some(ChannelMsg::Close) | None => {
                    if exited || !handle.is_closed() {
                        return Ok(End::Exited);
                    }
                    anyhow::bail!("соединение потеряно (сервер не отвечает)");
                }
                _ => {}
            },
            incoming = rx.recv() => match incoming {
                Some(Msg::Data(d)) => { channel.data(&d[..]).await?; }
                Some(Msg::Resize(r, c)) => {
                    *size = (r, c);
                    let _ = channel.window_change(c as u32, r as u32, 0, 0).await;
                }
                Some(Msg::Sftp(cmd)) => {
                    if sftp_tx.is_none() {
                        sftp_tx = open_sftp(&handle, ssh.clone()).await;
                    }
                    if let Some(tx) = &sftp_tx {
                        let _ = tx.send(cmd);
                    }
                }
                Some(Msg::Reconnect) => {
                    let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
                    return Ok(End::Restart);
                }
                Some(Msg::Close) | None => {
                    let _ = handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
                    return Ok(End::Closed);
                }
            },
        }
    }
}

/// Подключиться через цепочку jump-хостов. Возвращает целевой Handle и
/// промежуточные (их нужно держать живыми).
async fn connect_chain(
    sh: &Shared,
    ssh: &Arc<SshShared>,
    target: &Target,
    config: Arc<client::Config>,
    cache: &mut AuthCache,
) -> anyhow::Result<(Handle<Client>, Vec<Handle<Client>>)> {
    let first = target.hops.first().ok_or_else(|| NoRetry("не задан хост".into()))?;
    let tcp = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::net::TcpStream::connect((first.host.as_str(), first.port)),
    )
    .await
    .map_err(|_| anyhow::anyhow!("таймаут подключения к {}:{}", first.host, first.port))??;
    let _ = tcp.set_nodelay(true);

    let mut handle = client::connect_stream(
        config.clone(),
        tcp,
        Client { ssh: ssh.clone(), host: first.host.clone(), port: first.port },
    )
    .await?;
    authenticate(&mut handle, first, sh, ssh, cache).await?;

    let mut chain = Vec::new();
    for hop in &target.hops[1..] {
        ssh.set_state(ConnState::Connecting(hop.id()));
        let ch = handle.channel_open_direct_tcpip(hop.host.clone(), hop.port as u32, "127.0.0.1", 0).await?;
        let next = client::connect_stream(
            config.clone(),
            ch.into_stream(),
            Client { ssh: ssh.clone(), host: hop.host.clone(), port: hop.port },
        )
        .await?;
        chain.push(handle);
        handle = next;
        authenticate(&mut handle, hop, sh, ssh, cache).await?;
    }
    Ok((handle, chain))
}

// ───────────────────────── аутентификация ─────────────────────────

async fn connect_agent() -> Option<AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>> {
    #[cfg(unix)]
    {
        if let Ok(a) = AgentClient::connect_env().await {
            return Some(a.dynamic());
        }
    }
    #[cfg(windows)]
    {
        if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
            if let Ok(a) = AgentClient::connect_named_pipe(&sock).await {
                return Some(a.dynamic());
            }
        }
        if let Ok(a) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
            return Some(a.dynamic());
        }
        if let Ok(a) = AgentClient::connect_pageant().await {
            return Some(a.dynamic());
        }
    }
    None
}

fn key_candidates(hop: &Hop) -> Vec<(PathBuf, bool)> {
    if let Some(p) = &hop.identity {
        return vec![(PathBuf::from(crate::sshconfig::expand_tilde(p)), true)];
    }
    let dir = crate::sshconfig::home().join(".ssh");
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.is_file())
        .map(|p| (p, false))
        .collect()
}

async fn load_key(path: &Path, sh: &Shared, ssh: &SshShared, cache: &mut AuthCache) -> Option<PrivateKey> {
    match russh::keys::load_secret_key(path, None) {
        Ok(k) => Some(k),
        Err(russh::keys::Error::KeyIsEncrypted) => {
            let pkey = path.display().to_string();
            if let Some(pp) = cache.passphrases.get(&pkey) {
                if let Ok(k) = russh::keys::load_secret_key(path, Some(pp)) {
                    return Some(k);
                }
            }
            let store = format!("passphrase:{pkey}");
            if let Some(pp) = secrets::get(&store) {
                if let Ok(k) = russh::keys::load_secret_key(path, Some(&pp)) {
                    cache.passphrases.insert(pkey, pp);
                    return Some(k);
                }
                secrets::remove(&store);
            }
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(pkey.clone());
            let mut err = None;
            for _ in 0..3 {
                let kind = PromptKind::Secret {
                    title: format!("Ключ {name} защищён паролем"),
                    label: "Passphrase".into(),
                    error: err.take(),
                    can_remember: secrets::available(),
                };
                match ssh.ask(kind).await {
                    Answer::Secret { value, remember } => match russh::keys::load_secret_key(path, Some(&value)) {
                        Ok(k) => {
                            if remember {
                                secrets::set(&store, &value);
                            }
                            cache.passphrases.insert(pkey, value);
                            return Some(k);
                        }
                        Err(_) => err = Some("Неверная passphrase".into()),
                    },
                    _ => return None,
                }
            }
            None
        }
        Err(e) => {
            let head = std::fs::read_to_string(path).ok().and_then(|t| t.lines().next().map(|l| l.to_string()));
            if head.as_deref().is_some_and(|h| h.starts_with("PuTTY-User-Key-File")) {
                sh.note(&format!(
                    "# {} — ключ в формате PuTTY (PPK). Сконвертируйте в OpenSSH: puttygen key.ppk -O private-openssh -o key",
                    path.display()
                ));
            } else {
                sh.note(&format!("# Не удалось прочитать ключ {}: {e}", path.display()));
            }
            None
        }
    }
}

fn remaining(r: &russh::client::AuthResult) -> Option<Vec<MethodKind>> {
    match r {
        russh::client::AuthResult::Success => None,
        russh::client::AuthResult::Failure { remaining_methods, .. } => Some(remaining_methods.to_vec()),
    }
}

async fn authenticate(
    h: &mut Handle<Client>,
    hop: &Hop,
    sh: &Shared,
    ssh: &SshShared,
    cache: &mut AuthCache,
) -> anyhow::Result<()> {
    let id = hop.id();
    let user = hop.user.clone();
    ssh.set_state(ConnState::Authenticating(id.clone()));
    let mut methods: Option<Vec<MethodKind>> = None;
    let allows = |m: &Option<Vec<MethodKind>>, k: MethodKind| m.as_ref().map(|v| v.contains(&k)).unwrap_or(true);
    let hash = h.best_supported_rsa_hash().await.ok().flatten().flatten();

    // 1. Явный ключ (IdentityFile) — первым.
    let candidates = key_candidates(hop);
    for (path, explicit) in candidates.iter().filter(|(_, e)| *e) {
        let _ = explicit;
        if let Some(key) = load_key(path, sh, ssh, cache).await {
            let r = h.authenticate_publickey(&user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await?;
            if r.success() {
                return Ok(());
            }
            methods = remaining(&r);
        }
    }

    // 2. SSH-агент.
    if allows(&methods, MethodKind::PublicKey) {
        match connect_agent().await {
            Some(mut agent) => {
                if let Ok(ids) = agent.request_identities().await {
                    for ident in ids {
                        let pk = ident.public_key().into_owned();
                        if let Ok(r) = h.authenticate_publickey_with(&user, pk, hash, &mut agent).await {
                            if r.success() {
                                return Ok(());
                            }
                            methods = remaining(&r);
                        }
                    }
                }
            }
            None => {
                if cfg!(windows) && !cache.agent_hint_shown {
                    cache.agent_hint_shown = true;
                    sh.note("# SSH-агент не найден. Включить: Set-Service ssh-agent -StartupType Automatic; Start-Service ssh-agent");
                }
            }
        }
    }

    // 3. Ключи по умолчанию (~/.ssh/id_*), если сервер принимает ключи.
    if allows(&methods, MethodKind::PublicKey) {
        for (path, _) in candidates.iter().filter(|(_, e)| !*e) {
            if let Some(key) = load_key(path, sh, ssh, cache).await {
                let r = h.authenticate_publickey(&user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await?;
                if r.success() {
                    return Ok(());
                }
                methods = remaining(&r);
            }
        }
    }

    // 4. Пароль.
    if allows(&methods, MethodKind::Password) {
        let mut err: Option<String> = None;
        for attempt in 0..3 {
            let (pw, remember, from_store) = if attempt == 0 && secrets::has(&id) && secrets::get(&id).is_some() {
                (secrets::get(&id).unwrap_or_default(), false, true)
            } else if attempt == 0 && cache.passwords.contains_key(&id) {
                (cache.passwords[&id].clone(), false, false)
            } else {
                let kind = PromptKind::Secret {
                    title: format!("Вход на {id}"),
                    label: "Пароль".into(),
                    error: err.take(),
                    can_remember: secrets::available(),
                };
                match ssh.ask(kind).await {
                    Answer::Secret { value, remember } => (value, remember, false),
                    _ => return Err(NoRetry("вход отменён".into()).into()),
                }
            };
            let r = h.authenticate_password(&user, &pw).await?;
            if r.success() {
                if remember {
                    secrets::set(&id, &pw);
                }
                cache.passwords.insert(id.clone(), pw);
                return Ok(());
            }
            if from_store {
                secrets::remove(&id);
            }
            cache.passwords.remove(&id);
            methods = remaining(&r);
            err = Some("Неверный пароль — попробуйте ещё раз".into());
            if !allows(&methods, MethodKind::Password) {
                break;
            }
        }
    }

    // 5. keyboard-interactive (одноразовые коды, 2FA).
    if allows(&methods, MethodKind::KeyboardInteractive) {
        let mut resp = h.authenticate_keyboard_interactive_start(&user, None::<String>).await?;
        for _ in 0..8 {
            match resp {
                client::KeyboardInteractiveAuthResponse::Success => return Ok(()),
                client::KeyboardInteractiveAuthResponse::Failure { .. } => break,
                client::KeyboardInteractiveAuthResponse::InfoRequest { name, instructions, prompts } => {
                    let answers = if prompts.is_empty() {
                        vec![]
                    } else {
                        let kind = PromptKind::Kbd {
                            title: if name.trim().is_empty() { format!("Вход на {id}") } else { name },
                            instructions,
                            prompts: prompts.iter().map(|p| (p.prompt.clone(), p.echo)).collect(),
                        };
                        match ssh.ask(kind).await {
                            Answer::Responses(v) => v,
                            _ => return Err(NoRetry("вход отменён".into()).into()),
                        }
                    };
                    resp = h.authenticate_keyboard_interactive_respond(answers).await?;
                }
            }
        }
    }

    Err(NoRetry(format!("не удалось войти на {id}: сервер отклонил ключи и пароль")).into())
}

// ───────────────────────── SFTP ─────────────────────────

async fn open_sftp(handle: &Handle<Client>, ssh: Arc<SshShared>) -> Option<UnboundedSender<SftpCmd>> {
    ssh.sftp_update(|v| {
        v.loading = true;
        v.error = None;
    });
    let res: anyhow::Result<russh_sftp::client::SftpSession> = async {
        let ch = handle.channel_open_session().await?;
        ch.request_subsystem(true, "sftp").await?;
        Ok(russh_sftp::client::SftpSession::new(ch.into_stream()).await?)
    }
    .await;
    match res {
        Ok(s) => {
            let (tx, rx) = unbounded_channel();
            tokio::spawn(sftp_task(s, rx, ssh));
            Some(tx)
        }
        Err(e) => {
            ssh.sftp_update(|v| {
                v.loading = false;
                v.error = Some(format!("SFTP недоступен: {e}"));
            });
            None
        }
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

async fn sftp_task(s: russh_sftp::client::SftpSession, mut rx: UnboundedReceiver<SftpCmd>, ssh: Arc<SshShared>) {
    while let Some(cmd) = rx.recv().await {
        let cwd = ssh.sftp.lock().map(|v| v.cwd.clone()).unwrap_or_default();
        let res: anyhow::Result<bool> = match cmd {
            SftpCmd::List(p) => list(&s, &ssh, &p).await.map(|_| false),
            SftpCmd::Mkdir(p) => s.create_dir(p).await.map(|_| true).map_err(Into::into),
            SftpCmd::Delete { path, is_dir } => delete(&s, &path, is_dir).await.map(|_| true),
            SftpCmd::Rename { from, to } => s.rename(from, to).await.map(|_| true).map_err(Into::into),
            SftpCmd::Download { path, is_dir, dest } => download(&s, &ssh, &path, is_dir, &dest).await.map(|_| false),
            SftpCmd::Upload { local, dir } => upload(&s, &ssh, &local, &dir).await.map(|_| true),
        };
        match res {
            Ok(true) => {
                let _ = list(&s, &ssh, &cwd).await;
            }
            Ok(false) => {}
            Err(e) => ssh.sftp_update(|v| {
                v.loading = false;
                v.error = Some(e.to_string());
            }),
        }
    }
}

async fn list(s: &russh_sftp::client::SftpSession, ssh: &SshShared, path: &str) -> anyhow::Result<()> {
    ssh.sftp_update(|v| v.loading = true);
    let path = if path.is_empty() { "." } else { path };
    let cwd = s.canonicalize(path).await?;
    let mut entries = Vec::new();
    for e in s.read_dir(cwd.clone()).await? {
        let name = e.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let md = e.metadata();
        let mut is_dir = e.file_type().is_dir();
        if e.file_type().is_symlink() {
            if let Ok(m) = s.metadata(join(&cwd, &name)).await {
                is_dir = m.is_dir();
            }
        }
        entries.push(Entry { name, is_dir, size: md.size.unwrap_or(0) });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    ssh.sftp_update(|v| {
        v.cwd = cwd;
        v.entries = entries;
        v.loading = false;
        v.error = None;
    });
    Ok(())
}

async fn delete(s: &russh_sftp::client::SftpSession, path: &str, is_dir: bool) -> anyhow::Result<()> {
    if !is_dir {
        s.remove_file(path.to_string()).await?;
        return Ok(());
    }
    let mut stack = vec![(path.to_string(), false)];
    while let Some((p, visited)) = stack.pop() {
        if visited {
            s.remove_dir(p).await?;
            continue;
        }
        stack.push((p.clone(), true));
        for e in s.read_dir(p.clone()).await? {
            let n = e.file_name();
            if n == "." || n == ".." {
                continue;
            }
            let child = join(&p, &n);
            if e.file_type().is_dir() {
                stack.push((child, false));
            } else {
                s.remove_file(child).await?;
            }
        }
    }
    Ok(())
}

fn new_transfer(ssh: &SshShared, name: &str, upload: bool, total: u64, local: Option<PathBuf>) -> u64 {
    let mut id = 0;
    ssh.sftp_update(|v| {
        v.next_id += 1;
        id = v.next_id;
        v.transfers.push(Transfer { id, name: name.into(), upload, done: 0, total, error: None, finished: false, local });
    });
    id
}

fn transfer_update(ssh: &SshShared, id: u64, f: impl FnOnce(&mut Transfer)) {
    ssh.sftp_update(|v| {
        if let Some(t) = v.transfers.iter_mut().find(|t| t.id == id) {
            f(t);
        }
    });
}

fn unique_path(p: PathBuf) -> PathBuf {
    if !p.exists() {
        return p;
    }
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|c| !c.exists()).unwrap()
}

const CHUNK: usize = 256 * 1024;

async fn download_file(
    s: &russh_sftp::client::SftpSession,
    ssh: &SshShared,
    remote: &str,
    local: &Path,
    tid: u64,
) -> anyhow::Result<u64> {
    let mut f = s.open(remote.to_string()).await?;
    let mut out = std::fs::File::create(local)?;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    let mut last = Instant::now();
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut out, &buf[..n])?;
        done += n as u64;
        if last.elapsed() > Duration::from_millis(100) {
            last = Instant::now();
            transfer_update(ssh, tid, |t| t.done = t.done.max(done));
        }
    }
    Ok(done)
}

async fn download(
    s: &russh_sftp::client::SftpSession,
    ssh: &SshShared,
    path: &str,
    is_dir: bool,
    dest: &Path,
) -> anyhow::Result<()> {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    std::fs::create_dir_all(dest)?;
    let root = unique_path(dest.join(&name));
    if !is_dir {
        let total = s.metadata(path.to_string()).await.map(|m| m.len()).unwrap_or(0);
        let tid = new_transfer(ssh, &name, false, total, Some(root.clone()));
        let r = download_file(s, ssh, path, &root, tid).await;
        transfer_update(ssh, tid, |t| match &r {
            Ok(n) => {
                t.done = *n;
                t.finished = true;
            }
            Err(e) => {
                t.error = Some(e.to_string());
                t.finished = true;
            }
        });
        return r.map(|_| ());
    }
    let tid = new_transfer(ssh, &format!("{name}/"), false, 0, Some(root.clone()));
    let mut stack = vec![(path.to_string(), root.clone())];
    let mut total_done = 0u64;
    let r: anyhow::Result<()> = async {
        while let Some((rdir, ldir)) = stack.pop() {
            std::fs::create_dir_all(&ldir)?;
            for e in s.read_dir(rdir.clone()).await? {
                let n = e.file_name();
                if n == "." || n == ".." {
                    continue;
                }
                let rp = join(&rdir, &n);
                let lp = ldir.join(&n);
                if e.file_type().is_dir() {
                    stack.push((rp, lp));
                } else {
                    let size = e.metadata().size.unwrap_or(0);
                    transfer_update(ssh, tid, |t| t.total += size);
                    total_done += download_file(s, ssh, &rp, &lp, tid).await?;
                    transfer_update(ssh, tid, |t| t.done = total_done);
                }
            }
        }
        Ok(())
    }
    .await;
    transfer_update(ssh, tid, |t| {
        t.finished = true;
        if let Err(e) = &r {
            t.error = Some(e.to_string());
        }
    });
    r
}

async fn upload_file(
    s: &russh_sftp::client::SftpSession,
    ssh: &SshShared,
    local: &Path,
    remote: &str,
    tid: u64,
    base: u64,
) -> anyhow::Result<u64> {
    let mut input = std::fs::File::open(local)?;
    let mut f = s.create(remote.to_string()).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    let mut last = Instant::now();
    loop {
        let n = std::io::Read::read(&mut input, &mut buf)?;
        if n == 0 {
            break;
        }
        f.write_all(&buf[..n]).await?;
        done += n as u64;
        if last.elapsed() > Duration::from_millis(100) {
            last = Instant::now();
            let d = base + done;
            transfer_update(ssh, tid, |t| t.done = d);
        }
    }
    f.flush().await?;
    f.shutdown().await?;
    Ok(done)
}

async fn upload(s: &russh_sftp::client::SftpSession, ssh: &SshShared, local: &Path, dir: &str) -> anyhow::Result<()> {
    let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let remote_root = join(dir, &name);
    if local.is_file() {
        let total = std::fs::metadata(local).map(|m| m.len()).unwrap_or(0);
        let tid = new_transfer(ssh, &name, true, total, None);
        let r = upload_file(s, ssh, local, &remote_root, tid, 0).await;
        transfer_update(ssh, tid, |t| {
            t.finished = true;
            match &r {
                Ok(n) => t.done = *n,
                Err(e) => t.error = Some(e.to_string()),
            }
        });
        return r.map(|_| ());
    }
    // Папка: сначала считаем объём, затем рекурсивно заливаем.
    let mut files = Vec::new();
    let mut dirs = vec![(local.to_path_buf(), remote_root.clone())];
    let mut i = 0;
    while i < dirs.len() {
        let (ld, rd) = dirs[i].clone();
        for e in std::fs::read_dir(&ld)?.flatten() {
            let p = e.path();
            let n = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                dirs.push((p, join(&rd, &n)));
            } else {
                files.push((p, join(&rd, &n)));
            }
        }
        i += 1;
    }
    let total: u64 = files.iter().map(|(p, _)| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum();
    let tid = new_transfer(ssh, &format!("{name}/"), true, total, None);
    let r: anyhow::Result<()> = async {
        for (_, rd) in &dirs {
            let _ = s.create_dir(rd.clone()).await; // уже существует — не ошибка
        }
        let mut done = 0;
        for (lp, rp) in &files {
            done += upload_file(s, ssh, lp, rp, tid, done).await?;
            transfer_update(ssh, tid, |t| t.done = done);
        }
        Ok(())
    }
    .await;
    transfer_update(ssh, tid, |t| {
        t.finished = true;
        if let Err(e) = &r {
            t.error = Some(e.to_string());
        }
    });
    r
}

// ───────────────────────── интеграционный тест ─────────────────────────
// Запуск: MOBARUST_SSH_TEST=<каталог с client_key> cargo test ssh_engine -- --nocapture
// Нужен sshd на 127.0.0.1:2222 с пользователем tester / S3cret-pass и ключом client_key (passphrase kpass).
#[cfg(test)]
mod it {
    use super::*;

    fn wait(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed().as_secs() < secs, "таймаут: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn screen(t: &TermSession) -> String {
        t.shared.parser.lock().unwrap().screen().contents()
    }

    fn prompt_kind(s: &SshShared) -> Option<&'static str> {
        s.prompt.lock().unwrap().as_ref().map(|p| match p.kind {
            PromptKind::HostKey { .. } => "hostkey",
            PromptKind::Secret { ref label, .. } if label == "Пароль" => "password",
            PromptKind::Secret { .. } => "passphrase",
            PromptKind::Kbd { .. } => "kbd",
        })
    }

    fn target(hops: Vec<Hop>) -> Target {
        Target { hops, keepalive: 15, auto_reconnect: true }
    }

    fn hop(identity: Option<String>) -> Hop {
        Hop { host: "127.0.0.1".into(), port: 2222, user: "tester".into(), identity }
    }

    #[test]
    fn ssh_engine() {
        let Ok(dir) = std::env::var("MOBARUST_SSH_TEST") else {
            eprintln!("MOBARUST_SSH_TEST не задан — пропуск");
            return;
        };
        let home = std::env::temp_dir().join(format!("mobarust-home-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        unsafe { std::env::set_var("HOME", &home) };
        let ctx = egui::Context::default();

        // 1. Первый вход: сначала ключ хоста (TOFU), потом пароль.
        let mut t = spawn(&ctx, "t1", target(vec![hop(None)]), 24, 80, 1000);
        let s = t.ssh.clone().unwrap();
        wait("запрос ключа хоста", 10, || prompt_kind(&s).is_some());
        assert_eq!(prompt_kind(&s), Some("hostkey"), "ключ хоста должен проверяться ДО пароля");
        s.answer(Answer::Trust { save: true });
        wait("запрос пароля", 10, || prompt_kind(&s) == Some("password"));
        s.answer(Answer::Secret { value: "wrong".into(), remember: false });
        wait("повторный запрос пароля", 10, || prompt_kind(&s) == Some("password"));
        s.answer(Answer::Secret { value: "S3cret-pass".into(), remember: false });
        wait("подключено", 10, || s.state() == ConnState::Connected);
        t.write_input(b"echo MOBA_$((40+2))\r");
        wait("вывод команды", 10, || screen(&t).contains("MOBA_42"));
        assert!(known_hosts_file().exists(), "known_hosts записан");
        println!("✔ TOFU → пароль (с повтором) → shell");

        // 2. SFTP: список, загрузка, скачивание.
        let local = home.join("up.txt");
        std::fs::write(&local, "hello sftp 123").unwrap();
        t.sftp(SftpCmd::List(".".into()));
        wait("список SFTP", 10, || !s.sftp.lock().unwrap().cwd.is_empty());
        let cwd = s.sftp.lock().unwrap().cwd.clone();
        assert_eq!(cwd, "/home/tester");
        t.sftp(SftpCmd::Upload { local: local.clone(), dir: cwd.clone() });
        wait("upload", 10, || s.sftp.lock().unwrap().entries.iter().any(|e| e.name == "up.txt"));
        let dl = home.join("dl");
        t.sftp(SftpCmd::Download { path: format!("{cwd}/up.txt"), is_dir: false, dest: dl.clone() });
        wait("download", 10, || s.sftp.lock().unwrap().transfers.iter().filter(|x| x.finished).count() == 2);
        assert_eq!(std::fs::read_to_string(dl.join("up.txt")).unwrap(), "hello sftp 123");
        t.sftp(SftpCmd::Delete { path: format!("{cwd}/up.txt"), is_dir: false });
        wait("delete", 10, || !s.sftp.lock().unwrap().entries.iter().any(|e| e.name == "up.txt"));
        assert!(s.sftp.lock().unwrap().error.is_none());
        println!("✔ SFTP list/upload/download/delete");

        // 3. Переподключение: пароль из кэша сессии, без запроса.
        t.reconnect();
        wait("переподключение начато", 10, || s.state() != ConnState::Connected);
        wait("переподключено", 15, || s.state() == ConnState::Connected);
        assert!(prompt_kind(&s).is_none());
        t.write_input(b"echo AGAIN_$((1+1))\r");
        wait("вывод после переподключения", 10, || screen(&t).contains("AGAIN_2"));
        println!("✔ переподключение без повторного пароля, история сохранена: {}", screen(&t).contains("MOBA_42"));

        // 4. Выход из shell → «сеанс завершён», без автопереподключения.
        t.write_input(b"exit\r");
        wait("отключено", 10, || matches!(s.state(), ConnState::Disconnected(_)));
        assert!(s.retry_at.lock().unwrap().is_none(), "после exit не переподключаемся автоматически");
        t.close();
        println!("✔ exit → без автоповтора");

        // 5. Вход по ключу с passphrase; второй вход — без запроса ключа хоста.
        let key = format!("{dir}/client_key");
        let t2 = spawn(&ctx, "t2", target(vec![hop(Some(key.clone()))]), 24, 80, 1000);
        let s2 = t2.ssh.clone().unwrap();
        wait("запрос passphrase", 10, || prompt_kind(&s2).is_some());
        assert_eq!(prompt_kind(&s2), Some("passphrase"), "хост уже известен — сразу ключ");
        s2.answer(Answer::Secret { value: "kpass".into(), remember: false });
        wait("подключено по ключу", 10, || s2.state() == ConnState::Connected);
        t2.close();
        println!("✔ ключ ed25519 с passphrase");

        // 6. Цепочка через jump-хост (direct-tcpip).
        let t3 = spawn(&ctx, "t3", target(vec![hop(Some(key.clone())), hop(Some(key.clone()))]), 24, 80, 1000);
        let s3 = t3.ssh.clone().unwrap();
        let mut answered = 0;
        wait("подключено через jump", 20, || {
            if prompt_kind(&s3) == Some("passphrase") {
                s3.answer(Answer::Secret { value: "kpass".into(), remember: false });
                answered += 1;
            }
            s3.state() == ConnState::Connected
        });
        assert_eq!(answered, 1, "passphrase спрашивается один раз на цепочку");
        t3.close();
        println!("✔ ProxyJump-цепочка, passphrase закэширована");

        // 7. Подмена ключа хоста → блокировка, пароль НЕ запрашивается.
        let other = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(home.join("fake"))
            .status()
            .unwrap();
        assert!(other.success());
        let pubkey = std::fs::read_to_string(home.join("fake.pub")).unwrap();
        let mut parts = pubkey.split_whitespace();
        let line = format!("[127.0.0.1]:2222 {} {}\n", parts.next().unwrap(), parts.next().unwrap());
        std::fs::write(known_hosts_file(), line).unwrap();
        let t4 = spawn(&ctx, "t4", target(vec![hop(None)]), 24, 80, 1000);
        let s4 = t4.ssh.clone().unwrap();
        wait("блокировка", 10, || matches!(s4.state(), ConnState::Blocked(_)));
        assert!(prompt_kind(&s4).is_none(), "при подмене ключа пароль не спрашивается");
        let line_no = s4.blocked.lock().unwrap().as_ref().unwrap().line;
        forget_host_key(line_no).unwrap();
        t4.reconnect();
        wait("снова TOFU после удаления записи", 10, || prompt_kind(&s4) == Some("hostkey"));
        s4.answer(Answer::Reject);
        wait("отклонено", 10, || matches!(s4.state(), ConnState::Disconnected(_)));
        t4.close();
        println!("✔ подмена ключа хоста блокирует вход; удаление записи → снова TOFU");

        // 8. Обрыв соединения (убит серверный процесс) → автопереподключение.
        let t5 = spawn(&ctx, "t5", target(vec![hop(None)]), 24, 80, 1000);
        let s5 = t5.ssh.clone().unwrap();
        wait("TOFU", 10, || prompt_kind(&s5) == Some("hostkey"));
        s5.answer(Answer::Trust { save: true });
        wait("пароль", 10, || prompt_kind(&s5) == Some("password"));
        s5.answer(Answer::Secret { value: "S3cret-pass".into(), remember: false });
        wait("подключено", 10, || s5.state() == ConnState::Connected);
        std::process::Command::new("pkill").args(["-KILL", "-f", "sshd-session: tester|sshd: tester"]).status().ok();
        wait("обрыв замечен", 20, || matches!(s5.state(), ConnState::Disconnected(_)));
        assert!(s5.retry_at.lock().unwrap().is_some(), "после обрыва запланирован автоповтор");
        wait("автопереподключение", 20, || s5.state() == ConnState::Connected);
        assert!(prompt_kind(&s5).is_none());
        t5.close();
        println!("✔ обрыв → автопереподключение без запроса пароля");

        // 9. Вход через ssh-agent (SSH_AUTH_SOCK), без запросов.
        if let Ok(sock) = std::env::var("MOBARUST_AGENT_SOCK") {
            unsafe { std::env::set_var("SSH_AUTH_SOCK", sock) };
            let t6 = spawn(&ctx, "t6", target(vec![hop(None)]), 24, 80, 1000);
            let s6 = t6.ssh.clone().unwrap();
            wait("подключено через агент", 10, || {
                assert!(prompt_kind(&s6).is_none(), "агент — без запросов");
                s6.state() == ConnState::Connected
            });
            t6.close();
            println!("✔ ssh-agent");
        }
    }
}

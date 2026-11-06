//! SSH-бэкенд на russh. Каждая сессия крутит свой tokio-рантайм в отдельном
//! потоке: подключается, открывает канал, запрашивает PTY + shell и гоняет
//! ввод-вывод. Проверка ключа хоста упрощена (accept-all) — для MVP.

use super::{Msg, TermSession, Writer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Параметры подключения.
pub struct SshParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

/// Обработчик клиента russh.
struct Client;

impl russh::client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // TODO: сверять с known_hosts. Пока принимаем любой ключ.
        Ok(true)
    }
}

/// Записать строку прямо в экран терминала (для статусов/ошибок).
fn banner(parser: &Arc<Mutex<vt100::Parser>>, dirty: &Arc<AtomicBool>, text: &str) {
    if let Ok(mut p) = parser.lock() {
        p.process(text.replace('\n', "\r\n").as_bytes());
    }
    dirty.store(true, Ordering::SeqCst);
}

/// Открыть SSH-сессию. Возвращает `TermSession` сразу; подключение идёт в фоне.
pub fn spawn(title: &str, params: SshParams, rows: u16, cols: u16) -> TermSession {
    let parser = Arc::new(Mutex::new(vt100::Parser::new(
        super::nz(rows),
        super::nz(cols),
        5000,
    )));
    let dirty = Arc::new(AtomicBool::new(true));
    let alive = Arc::new(AtomicBool::new(true));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();

    banner(
        &parser,
        &dirty,
        &format!("\x1b[90m# Подключение к {}@{}:{} …\x1b[0m\n", params.user, params.host, params.port),
    );

    {
        let parser = parser.clone();
        let dirty = dirty.clone();
        let alive = alive.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    banner(&parser, &dirty, &format!("\x1b[31mНе удалось создать рантайм: {e}\x1b[0m\n"));
                    alive.store(false, Ordering::SeqCst);
                    return;
                }
            };

            let result = rt.block_on(async {
                run_session(&params, rows, cols, &parser, &dirty, &mut rx).await
            });

            if let Err(e) = result {
                banner(&parser, &dirty, &format!("\r\n\x1b[31m# Соединение закрыто: {e}\x1b[0m\r\n"));
            } else {
                banner(&parser, &dirty, "\r\n\x1b[90m# Соединение закрыто.\x1b[0m\r\n");
            }
            alive.store(false, Ordering::SeqCst);
            dirty.store(true, Ordering::SeqCst);
        });
    }

    TermSession {
        title: title.to_string(),
        kind_label: "ssh".into(),
        parser,
        dirty,
        alive,
        rows,
        cols,
        writer: Writer::Tokio(tx),
    }
}

async fn run_session(
    params: &SshParams,
    rows: u16,
    cols: u16,
    parser: &Arc<Mutex<vt100::Parser>>,
    dirty: &Arc<AtomicBool>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
) -> Result<(), anyhow::Error> {
    let config = Arc::new(russh::client::Config::default());
    let mut session =
        russh::client::connect(config, (params.host.as_str(), params.port), Client).await?;

    let auth = session
        .authenticate_password(&params.user, &params.password)
        .await?;
    if !auth.success() {
        return Err(anyhow::anyhow!("аутентификация отклонена"));
    }

    let channel = session.channel_open_session().await?;
    channel
        .request_pty(false, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await?;
    channel.request_shell(true).await?;

    banner(parser, dirty, "\x1b[90m# Подключено.\x1b[0m\r\n");

    let mut channel = channel;
    loop {
        tokio::select! {
            msg = channel.wait() => {
                match msg {
                    Some(russh::ChannelMsg::Data { ref data }) => {
                        if let Ok(mut p) = parser.lock() { p.process(data); }
                        dirty.store(true, Ordering::SeqCst);
                    }
                    Some(russh::ChannelMsg::ExtendedData { ref data, .. }) => {
                        if let Ok(mut p) = parser.lock() { p.process(data); }
                        dirty.store(true, Ordering::SeqCst);
                    }
                    Some(russh::ChannelMsg::Eof) | Some(russh::ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
            incoming = rx.recv() => {
                match incoming {
                    Some(Msg::Data(d)) => { channel.data(&d[..]).await?; }
                    Some(Msg::Resize(r, c)) => {
                        channel.window_change(c as u32, r as u32, 0, 0).await?;
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

//! Локальные shell через псевдотерминал (ConPTY на Windows) — TRM-02.

use super::{Msg, Shared, TabKind, TermSession, Writer};
use anyhow::Result;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::atomic::Ordering;

/// Доступный локальный shell: (подпись, команда).
#[derive(Clone)]
pub struct ShellProfile {
    pub label: String,
    pub command: String,
}

fn in_path(exe: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
        .unwrap_or(false)
}

/// Найти shell'ы системы (один раз при старте).
pub fn detect_shells() -> Vec<ShellProfile> {
    let mut v = Vec::new();
    if cfg!(windows) {
        if in_path("pwsh.exe") {
            v.push(ShellProfile { label: "PowerShell 7".into(), command: "pwsh.exe -NoLogo".into() });
        }
        v.push(ShellProfile { label: "Windows PowerShell".into(), command: "powershell.exe -NoLogo".into() });
        v.push(ShellProfile { label: "Командная строка".into(), command: "cmd.exe".into() });
        for d in wsl_distros() {
            v.push(ShellProfile { label: format!("WSL · {d}"), command: format!("wsl.exe -d {d}") });
        }
    } else {
        if let Ok(sh) = std::env::var("SHELL") {
            let name = sh.rsplit('/').next().unwrap_or(&sh).to_string();
            v.push(ShellProfile { label: name, command: sh });
        }
        for (label, path) in [("bash", "/bin/bash"), ("zsh", "/bin/zsh"), ("fish", "/usr/bin/fish")] {
            if std::path::Path::new(path).exists() && !v.iter().any(|s| s.command == path) {
                v.push(ShellProfile { label: label.into(), command: path.into() });
            }
        }
        if v.is_empty() {
            v.push(ShellProfile { label: "sh".into(), command: "/bin/sh".into() });
        }
    }
    v
}

/// Список дистрибутивов WSL (`wsl -l -q` выводит UTF-16LE). Без мелькания консоли.
#[cfg(windows)]
fn wsl_distros() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(out) = std::process::Command::new("wsl.exe")
        .args(["-l", "-q"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return vec![];
    };
    let u16s: Vec<u16> = out.stdout.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&u16s)
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string())
        .filter(|l| !l.is_empty() && !l.contains(' '))
        .collect()
}

#[cfg(not(windows))]
fn wsl_distros() -> Vec<String> {
    vec![]
}

fn default_shell() -> String {
    detect_shells().first().map(|s| s.command.clone()).unwrap_or_else(|| "/bin/sh".into())
}

/// Запустить shell и вернуть канал ввода; вывод пишется в `shared`.
fn start(shared: &Shared, command: &str, rows: u16, cols: u16) -> Result<Writer> {
    let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
    let mut parts = command.split_whitespace();
    let prog = parts.next().unwrap_or("sh");
    let mut cmd = CommandBuilder::new(prog);
    for a in parts {
        cmd.arg(a);
    }
    if let Some(home) = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()) {
        cmd.cwd(home);
    }
    if !cfg!(windows) {
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut pty_writer = pair.master.take_writer()?;
    let master = pair.master;
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    shared.alive.store(true, Ordering::SeqCst);

    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => shared.feed(&buf[..n]),
                }
            }
            shared.alive.store(false, Ordering::SeqCst);
            shared.note("# Процесс завершён. Ctrl+Shift+R — запустить заново.");
        });
    }

    std::thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            match msg {
                Msg::Data(d) => {
                    if pty_writer.write_all(&d).is_err() {
                        break;
                    }
                    let _ = pty_writer.flush();
                }
                Msg::Resize(r, c) => {
                    let _ = master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
                }
                Msg::Close => {
                    let _ = child.kill();
                    break;
                }
                _ => {}
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    });

    Ok(Writer::Std(tx))
}

pub fn spawn(
    ctx: &egui::Context,
    title: &str,
    shell: Option<String>,
    rows: u16,
    cols: u16,
    scrollback: usize,
) -> Result<TermSession> {
    let shared = Shared::new(ctx, rows, cols, scrollback);
    let command = shell.clone().unwrap_or_else(default_shell);
    let writer = start(&shared, &command, rows, cols)?;
    Ok(TermSession::new(title.to_string(), TabKind::Local { shell }, shared, rows, cols, writer))
}

/// Перезапустить shell в той же вкладке (история остаётся).
pub fn respawn(sess: &mut TermSession) -> Result<()> {
    let TabKind::Local { shell } = &sess.kind else { return Ok(()) };
    if sess.is_alive() {
        return Ok(());
    }
    let command = shell.clone().unwrap_or_else(default_shell);
    let writer = start(&sess.shared, &command, sess.rows, sess.cols)?;
    sess.replace_writer(writer);
    Ok(())
}

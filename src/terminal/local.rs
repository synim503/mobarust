//! Локальный шелл через псевдотерминал (portable-pty).

use super::{Msg, TermSession, Writer};
use anyhow::Result;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
    }
}

/// Запустить локальный шелл и вернуть живую сессию.
pub fn spawn(title: &str, shell: Option<String>, rows: u16, cols: u16) -> Result<TermSession> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let cmd = CommandBuilder::new(shell.unwrap_or_else(default_shell));
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut pty_writer = pair.master.take_writer()?;
    let master = pair.master;

    let parser = Arc::new(Mutex::new(vt100::Parser::new(
        super::nz(rows),
        super::nz(cols),
        5000,
    )));
    let dirty = Arc::new(AtomicBool::new(true));
    let alive = Arc::new(AtomicBool::new(true));
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();

    // Поток чтения вывода PTY → парсер.
    {
        let parser = parser.clone();
        let dirty = dirty.clone();
        let alive = alive.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut p) = parser.lock() {
                            p.process(&buf[..n]);
                        }
                        dirty.store(true, Ordering::SeqCst);
                    }
                }
            }
            alive.store(false, Ordering::SeqCst);
            dirty.store(true, Ordering::SeqCst);
        });
    }

    // Поток ввода / ресайза (владеет master ради resize, держит child живым).
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
                    let _ = master.resize(PtySize {
                        rows: r,
                        cols: c,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
            }
        }
        let _ = child.wait();
    });

    Ok(TermSession {
        title: title.to_string(),
        kind_label: "local".into(),
        parser,
        dirty,
        alive,
        rows,
        cols,
        writer: Writer::Std(tx),
    })
}

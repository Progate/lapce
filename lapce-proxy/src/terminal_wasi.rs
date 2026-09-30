//! 端末（WASI・BrowserOS）。**Linux と同じく pty の上でシェルを動かす。**
//!
//! ネイティブ版は alacritty_terminal の pty と event_loop（epoll / kqueue）を使うが、
//! WASI にはどちらも無い。BrowserOS は pty を `browser_os_pty` のホスト関数として持ち
//! （`openpty` / `set_winsize`）、子プロセスをその端末の上で起動する
//! `posix_spawn_tty` を `browser_os_process` に持っている。
//!
//! 画面側（lapce-app）はネイティブ版と同じで、ここが送るのは端末へ書かれたバイト列
//! （`update_terminal`）と、子の pid と終了コードだけである。
//!
//! ## 止まらずに待つ
//!
//! 親の側の fd は `O_NONBLOCK` にし、子の終わりは `WNOHANG` で聞き、何も無ければ
//! futex で眠る（→ wasi_process.rs と同じ理由）。

use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use lapce_rpc::{
    core::CoreRpcHandler,
    terminal::{TermId, TerminalProfile},
};

use crate::wasi_process;

#[link(wasm_import_module = "browser_os_pty")]
unsafe extern "C" {
    #[link_name = "openpty"]
    fn host_openpty(
        master_fd: *mut u32,
        name: *mut u8,
        name_cap: u32,
        name_len: *mut u32,
        rows: u32,
        cols: u32,
    ) -> i32;
    #[link_name = "set_winsize"]
    fn host_set_winsize(master_fd: u32, rows: u32, cols: u32) -> i32;
}

/// 画面側から端末へ
pub enum TerminalMessage {
    Input(Vec<u8>),
    Resize { rows: u16, cols: u16 },
    Shutdown,
}

#[derive(Clone)]
pub struct TerminalSender(Sender<TerminalMessage>);

impl TerminalSender {
    pub fn send(&self, message: TerminalMessage) {
        if let Err(err) = self.0.send(message) {
            tracing::error!("{:?}", err);
        }
    }
}

/// 何も起きていないとき、次に見に行くまでの間隔
const IDLE: Duration = Duration::from_millis(8);

fn futex_sleep(duration: Duration) {
    let (_sender, receiver) = std::sync::mpsc::channel::<()>();
    let _ = receiver.recv_timeout(duration);
}

/// シェルの決め方はネイティブ版（terminal.rs の `program`）と同じ順にする
fn program(profile: &TerminalProfile) -> (String, Vec<String>) {
    if let Some(command) = profile.command.clone() {
        return (command, profile.arguments.clone().unwrap_or_default());
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    (shell, Vec::new())
}

fn workdir(profile: &TerminalProfile) -> PathBuf {
    profile
        .workdir
        .as_ref()
        .and_then(|url| url.to_file_path().ok())
        .filter(|path| path.is_dir())
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// pty を開いてシェルを起動し、読み書きするスレッドを立てる
pub fn start(
    term_id: TermId,
    profile: TerminalProfile,
    cols: u16,
    rows: u16,
    core_rpc: CoreRpcHandler,
) -> Result<(TerminalSender, u32)> {
    let mut master = 0u32;
    let mut name = [0u8; 64];
    let mut name_len = 0u32;
    let errno = unsafe {
        host_openpty(
            &mut master,
            name.as_mut_ptr(),
            name.len() as u32,
            &mut name_len,
            rows as u32,
            cols as u32,
        )
    };
    if errno != 0 {
        return Err(anyhow!("openpty failed (errno {errno})"));
    }
    let tty = String::from_utf8_lossy(&name[..name_len as usize]).into_owned();
    let mut master = unsafe { File::from_raw_fd(master as i32) };
    set_nonblocking(&master)?;

    let (shell, arguments) = program(&profile);
    let mut argv = vec![shell.as_str()];
    argv.extend(arguments.iter().map(String::as_str));
    // 画面側は alacritty の格子で、xterm と同じ並びを解釈する
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    env.retain(|(key, _)| key != "TERM" && key != "COLORTERM");
    env.push(("TERM".to_owned(), "xterm-256color".to_owned()));
    env.push(("COLORTERM".to_owned(), "truecolor".to_owned()));
    env.extend(profile.environment.clone().unwrap_or_default());
    let pid = wasi_process::spawn_on_tty(&argv, &env, &workdir(&profile), &tty)?;

    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name(format!("terminal-{}", term_id.0))
        .spawn(move || run(term_id, &mut master, pid, rx, core_rpc))?;
    Ok((TerminalSender(tx), pid))
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 読めるだけ読んで画面側へ送る。何か読めたら true、閉じたら `Err`
fn pump(term_id: TermId, master: &mut File, core_rpc: &CoreRpcHandler) -> io::Result<bool> {
    let mut buffer = [0u8; 64 * 1024];
    let mut any = false;
    loop {
        match master.read(&mut buffer) {
            Ok(0) => {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "pty closed"));
            }
            Ok(count) => {
                core_rpc.update_terminal(term_id, buffer[..count].to_vec());
                any = true;
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(any),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

fn write_all(master: &mut File, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        match master.write(bytes) {
            Ok(0) => return,
            Ok(count) => bytes = &bytes[count..],
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => futex_sleep(Duration::from_millis(1)),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => {
                tracing::error!("{:?}", err);
                return;
            }
        }
    }
}

fn run(
    term_id: TermId,
    master: &mut File,
    pid: u32,
    rx: Receiver<TerminalMessage>,
    core_rpc: CoreRpcHandler,
) {
    let exit_code = loop {
        let mut busy = false;
        loop {
            match rx.try_recv() {
                Ok(TerminalMessage::Input(bytes)) => {
                    write_all(master, &bytes);
                    busy = true;
                }
                Ok(TerminalMessage::Resize { rows, cols }) => {
                    unsafe { host_set_winsize(master.as_raw_fd() as u32, rows as u32, cols as u32) };
                }
                // 親の側を閉じれば、端末の上のプロセスは OS が畳む（実機の SIGHUP と同じ）
                Ok(TerminalMessage::Shutdown) | Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => break,
            }
        }
        match pump(term_id, master, &core_rpc) {
            Ok(read) => busy |= read,
            Err(_) => break wasi_process::try_wait(pid).ok().flatten(),
        }
        if let Ok(Some(code)) = wasi_process::try_wait(pid) {
            // 終わったあとに残っている出力も配る
            let _ = pump(term_id, master, &core_rpc);
            break Some(code);
        }
        if !busy {
            futex_sleep(IDLE);
        }
    };
    core_rpc.terminal_process_stopped(term_id, exit_code);
}

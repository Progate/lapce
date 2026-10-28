//! 端末（WASI・BrowserOS）。**Linux と同じく pty の上でシェルを動かす。**
//!
//! ネイティブ版は alacritty_terminal の pty と event_loop（epoll / kqueue）を使うが、
//! WASI にはどちらも無い。BrowserOS は Linux の syscall を受ける（→ linux_wasi.rs）ので、pty は
//! Linux の openpty(3) と同じく `/dev/ptmx` を開き、`ioctl(TIOCSPTLCK)` で錠を外し、`ioctl(TIOCGPTN)` で
//! 従の番号を引いて `/dev/pts/<N>` を得る。窓の大きさは `ioctl(TIOCSWINSZ)`。子は Linux と同じく
//! `setsid(2)` で新しいセッションを作り、端末を 0 / 1 / 2 に開くと、それが子の制御端末になる
//! （→ wasi_process.rs の `spawn_on_tty`）。
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

use crate::{
    linux_wasi::{
        self, AT_FDCWD, O_CLOEXEC, O_NOCTTY, O_RDWR, SYS_IOCTL, SYS_OPENAT, address,
    },
    wasi_process,
};

/// ioctl(2) の要求（Linux の値）
const TIOCSWINSZ: i64 = 0x5414;
const TIOCGPTN: i64 = 0x8004_5430;
const TIOCSPTLCK: i64 = 0x4004_5431;

/// `struct winsize`（行・列・画素の幅・画素の高さ）
#[repr(C)]
struct Winsize {
    rows: u16,
    cols: u16,
    xpixel: u16,
    ypixel: u16,
}

/// 窓の大きさを変える（主からの `ioctl(TIOCSWINSZ)`。端末の上の前面のグループに SIGWINCH が届く）
fn set_winsize(master: &File, rows: u16, cols: u16) -> io::Result<()> {
    let size = Winsize {
        rows,
        cols,
        xpixel: 0,
        ypixel: 0,
    };
    linux_wasi::call(
        SYS_IOCTL,
        [
            master.as_raw_fd() as i64,
            TIOCSWINSZ,
            address(&size),
            0,
            0,
            0,
        ],
    )?;
    Ok(())
}

/// openpty(3)。主の fd と、従のパス（`/dev/pts/<N>`）
fn openpty(rows: u16, cols: u16) -> io::Result<(File, String)> {
    let ptmx = linux_wasi::c_path("/dev/ptmx");
    let fd = linux_wasi::call(
        SYS_OPENAT,
        [
            AT_FDCWD,
            address(ptmx.as_ptr()),
            O_RDWR | O_NOCTTY | O_CLOEXEC,
            0,
            0,
            0,
        ],
    )?;
    let master = unsafe { File::from_raw_fd(fd as i32) };
    let unlock = 0i32;
    linux_wasi::call(SYS_IOCTL, [fd, TIOCSPTLCK, address(&unlock), 0, 0, 0])?;
    let mut number = 0u32;
    linux_wasi::call(SYS_IOCTL, [fd, TIOCGPTN, address(&raw mut number), 0, 0, 0])?;
    set_winsize(&master, rows, cols)?;
    Ok((master, format!("/dev/pts/{number}")))
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
    let (mut master, tty) =
        openpty(rows, cols).map_err(|error| anyhow!("openpty failed: {error}"))?;
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
    if flags < 0
        || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 読めるだけ読んで画面側へ送る。何か読めたら true、閉じたら `Err`
fn pump(
    term_id: TermId,
    master: &mut File,
    core_rpc: &CoreRpcHandler,
) -> io::Result<bool> {
    let mut buffer = [0u8; 64 * 1024];
    let mut any = false;
    loop {
        match master.read(&mut buffer) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "pty closed",
                ));
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
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                futex_sleep(Duration::from_millis(1))
            }
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
                    if let Err(error) = set_winsize(master, rows, cols) {
                        tracing::error!("{:?}", error);
                    }
                }
                // 親の側を閉じれば、端末の上のプロセスは OS が畳む（実機の SIGHUP と同じ）
                Ok(TerminalMessage::Shutdown) | Err(TryRecvError::Disconnected) => {
                    return;
                }
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

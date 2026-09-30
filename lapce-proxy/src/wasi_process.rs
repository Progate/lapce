//! 子プロセス（WASI・BrowserOS）。
//!
//! WASI preview1 に `fork` / `exec` は無く、std の `Command` も使えない。BrowserOS は
//! その穴を `browser_os_process` という名前空間のホスト関数で埋めている——POSIX の
//! `posix_spawn(3)` / `waitpid(2)` / `kill(2)` と同じ意味で、子の標準入出力は
//! 3 本のパイプ（普通の fd）として返ってくる。
//!
//! ## 止まらずに待つ
//!
//! Lapce は wasi-threads で動いていて、全スレッドの syscall を 1 本のホスト役が順に
//! 捌く。パイプの blocking read や `waitpid` で止まると、**ほかのスレッド（画面を
//! 描くスレッドも）の syscall まで止まる**。だからパイプは `O_NONBLOCK` にし、
//! `waitpid` は `WNOHANG` で聞き、待つのは futex（syscall を出さない）で行う

use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
    time::Duration,
};

#[link(wasm_import_module = "browser_os_process")]
unsafe extern "C" {
    #[link_name = "posix_spawn"]
    fn host_posix_spawn(
        argv: *const u8,
        argv_len: usize,
        env: *const u8,
        env_len: usize,
        cwd: *const u8,
        cwd_len: usize,
        fds: *mut u32,
        pid: *mut u32,
    ) -> i32;
    #[link_name = "waitpid"]
    fn host_waitpid(pid: u32, status: *mut u32, options: i32) -> i32;
}

const WNOHANG: i32 = 1;
/// preview1 の errno（`EAGAIN`）
const ERRNO_AGAIN: i32 = 6;

pub struct Output {
    /// 終了コード（`WEXITSTATUS`）
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}

fn nul_joined<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for item in items {
        bytes.extend_from_slice(item.as_bytes());
        bytes.push(0);
    }
    bytes
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// syscall を出さずに眠る（→ 上の「止まらずに待つ」）
fn futex_sleep(duration: Duration) {
    let (_sender, receiver) = std::sync::mpsc::channel::<()>();
    let _ = receiver.recv_timeout(duration);
}

/// 読めるだけ読む。EOF なら `true`
fn drain(file: &mut File, out: &mut Vec<u8>) -> io::Result<bool> {
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => out.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// 子を起動して、終わるまで待つ（`Command::output()` と同じ役）。
/// `input` があれば標準入力へ流してから閉じる
pub fn output(argv: &[&str], cwd: &Path, input: Option<&[u8]>) -> io::Result<Output> {
    let argv_bytes = nul_joined(argv.iter().copied());
    // 環境はそのまま引き継ぐ（exec と同じ）
    let env: Vec<String> = std::env::vars().map(|(key, value)| format!("{key}={value}")).collect();
    let env_bytes = nul_joined(env.iter().map(String::as_str));
    let cwd = cwd.to_string_lossy();
    let mut fds = [0u32; 3];
    let mut pid = 0u32;
    tracing::debug!("posix_spawn {argv:?} in {cwd}");
    let errno = unsafe {
        host_posix_spawn(
            argv_bytes.as_ptr(),
            argv_bytes.len(),
            env_bytes.as_ptr(),
            env_bytes.len(),
            cwd.as_ptr(),
            cwd.len(),
            fds.as_mut_ptr(),
            &mut pid,
        )
    };
    tracing::debug!("posix_spawn {argv:?} in {cwd} -> errno {errno}, pid {pid}");
    if errno != 0 {
        return Err(io::Error::other(format!(
            "posix_spawn {} failed (errno {errno})",
            argv.first().copied().unwrap_or("")
        )));
    }
    let (mut stdin, mut stdout, mut stderr) = unsafe {
        (
            File::from_raw_fd(fds[0] as i32),
            File::from_raw_fd(fds[1] as i32),
            File::from_raw_fd(fds[2] as i32),
        )
    };
    set_nonblocking(&stdout)?;
    set_nonblocking(&stderr)?;
    if let Some(input) = input {
        // 書き込みは空くまで待ってくれる（ホスト側のパイプの意味論）。量は小さい前提
        stdin.write_all(input)?;
    }
    // 閉じることが子から見た EOF である
    drop(stdin);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let (mut out_done, mut err_done) = (false, false);
    let mut status = 0u32;
    let mut exited = false;
    loop {
        if !out_done {
            out_done = drain(&mut stdout, &mut out)?;
        }
        if !err_done {
            err_done = drain(&mut stderr, &mut err)?;
        }
        if !exited {
            match unsafe { host_waitpid(pid, &mut status, WNOHANG) } {
                0 => exited = true,
                ERRNO_AGAIN => {}
                errno => {
                    return Err(io::Error::other(format!("waitpid failed (errno {errno})")));
                }
            }
        }
        if exited && out_done && err_done {
            break;
        }
        futex_sleep(Duration::from_millis(2));
    }
    let code = ((status >> 8) & 0xff) as i32;
    tracing::debug!("{argv:?} exited with {code}");
    Ok(Output { code, stdout: out, stderr: err })
}

//! 子プロセス（WASI・BrowserOS）。
//!
//! WASI preview1 に `fork` / `exec` は無く、std の `Command` も使えない。BrowserOS は
//! その穴を `browser_os_process` という名前空間のホスト関数で埋めている——POSIX の
//! `pipe(2)` / `posix_spawnp(3)` / `waitpid(2)` と同じ形と意味で、子は親の 0 / 1 / 2 を継ぎ、
//! file actions（`dup2` / `open` / `chdir`）がそれを組み替える（→ browser-os の README
//! 「本物と同じ形で起こす」）。ここは std の `Command` が Unix でしていることを、それで書く。
//!
//! ## 止まらずに待つ
//!
//! Lapce は wasi-threads で動いていて、全スレッドの syscall を 1 本のホスト役が順に
//! 捌く。パイプの blocking read や `waitpid` で止まると、**ほかのスレッド（画面を
//! 描くスレッドも）の syscall まで止まる**。だからこちらの持つパイプの端は `O_NONBLOCK` にし、
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
    #[link_name = "pipe2"]
    fn host_pipe2(fds: *mut i32, flags: u32) -> i32;
    #[link_name = "posix_spawnp"]
    fn host_posix_spawnp(
        pid: *mut u32,
        file: *const u8,
        file_len: usize,
        actions: *const u8,
        actions_len: usize,
        attr: *const u8,
        attr_len: usize,
        argv: *const u8,
        argv_len: usize,
        envp: *const u8,
        envp_len: usize,
    ) -> i32;
    #[link_name = "waitpid"]
    fn host_waitpid(pid: u32, status: *mut u32, options: i32) -> i32;
}

const WNOHANG: i32 = 1;
/// preview1 の errno（`EAGAIN`）
const ERRNO_AGAIN: i32 = 6;

/// file actions の種類（browser-os の process/wasi-spawn.ts の `SPAWN_FILE_ACTION_*`）
const ACTION_OPEN: u32 = 1;
const ACTION_CLOSE: u32 = 2;
const ACTION_DUP2: u32 = 3;
const ACTION_CHDIR: u32 = 4;
/// `addopen` の oflag（wasi-libc の fcntl.h と同じ値）
const O_RDONLY: u32 = 0x0400_0000;
const O_WRONLY: u32 = 0x1000_0000;
const O_RDWR: u32 = O_RDONLY | O_WRONLY;
/// `posix_spawnattr_setflags` の `POSIX_SPAWN_SETSID`（glibc の spawn.h と同じ値）
const POSIX_SPAWN_SETSID: u32 = 0x80;
/// 0 / 1 / 2 を組み替える間だけ使う、子の中の仮の番号（最後に閉じる）
const SCRATCH_FD: i32 = 100;

pub struct Output {
    /// 終了コード（`WEXITSTATUS`。シグナルで止まったら 128 + シグナル番号）
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

/// `posix_spawn_file_actions_t`。子が起動する前に fd に対して行うことを、並べた順に持つ
#[derive(Default)]
struct FileActions(Vec<u8>);

impl FileActions {
    fn add(&mut self, op: u32, fd: i32, newfd: i32, oflag: u32, path: &str) {
        for value in [op, fd as u32, newfd as u32, oflag, 0, path.len() as u32] {
            self.0.extend_from_slice(&value.to_le_bytes());
        }
        self.0.extend_from_slice(path.as_bytes());
        // 次の件は 4 バイト境界から
        while self.0.len() % 4 != 0 {
            self.0.push(0);
        }
    }

    fn dup2(&mut self, fd: i32, newfd: i32) {
        self.add(ACTION_DUP2, fd, newfd, 0, "");
    }

    fn close(&mut self, fd: i32) {
        self.add(ACTION_CLOSE, fd, 0, 0, "");
    }

    fn open(&mut self, fd: i32, path: &str, oflag: u32) {
        self.add(ACTION_OPEN, fd, 0, oflag, path);
    }

    /// 作業ディレクトリ。cwd の持ち主は libc なので、こちらが渡す（カーネルはゲストの `chdir` を知らない）
    fn chdir(&mut self, path: &str) {
        self.add(ACTION_CHDIR, 0, 0, 0, path);
    }
}

/// `posix_spawnp(3)`。`argv[0]` を呼んだ側の `PATH` で探し、環境はこちらのものをそのまま渡す（exec と同じ）
fn spawn(
    argv: &[&str],
    env: &[String],
    actions: &FileActions,
    attr_flags: u32,
) -> io::Result<u32> {
    let file = argv.first().copied().unwrap_or("");
    let argv_bytes = nul_joined(argv.iter().copied());
    let env_bytes = nul_joined(env.iter().map(String::as_str));
    let attr = attr_flags.to_le_bytes();
    let attr_len = if attr_flags == 0 { 0 } else { attr.len() };
    let mut pid = 0u32;
    let errno = unsafe {
        host_posix_spawnp(
            &mut pid,
            file.as_ptr(),
            file.len(),
            actions.0.as_ptr(),
            actions.0.len(),
            attr.as_ptr(),
            attr_len,
            argv_bytes.as_ptr(),
            argv_bytes.len(),
            env_bytes.as_ptr(),
            env_bytes.len(),
        )
    };
    tracing::debug!("posix_spawnp {argv:?} -> errno {errno}, pid {pid}");
    if errno != 0 {
        return Err(io::Error::other(format!(
            "posix_spawnp {file} failed (errno {errno})"
        )));
    }
    Ok(pid)
}

/// カーネルのパイプ（読み端, 書き端）
fn pipe() -> io::Result<(File, File)> {
    let mut fds = [-1i32; 2];
    let errno = unsafe { host_pipe2(fds.as_mut_ptr(), 0) };
    if errno != 0 {
        return Err(io::Error::other(format!("pipe2 failed (errno {errno})")));
    }
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
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
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(false);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// 書けるだけ書く。全部書けたら `true`（読み手が去ったら残りは捨てて `true`）
fn feed(file: &mut File, input: &mut &[u8]) -> io::Result<bool> {
    while !input.is_empty() {
        match file.write(input) {
            Ok(count) => *input = &input[count..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(false);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                return Ok(true);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

/// wait status を終了コードにする。シグナルで止まったら 128 + シグナル番号（シェルの `$?` と同じ表し方）
fn exit_code(status: u32) -> i32 {
    let signal = status & 0x7f;
    if signal != 0 && signal != 0x7f {
        128 + signal as i32
    } else {
        ((status >> 8) & 0xff) as i32
    }
}

/// 子を起動して、終わるまで待つ（`Command::output()` と同じ役）。
/// `input` があれば標準入力へ流してから閉じ、無ければ標準入力は `/dev/null`（`Stdio::null()`）
pub fn output(
    argv: &[&str],
    cwd: &Path,
    input: Option<&[u8]>,
) -> io::Result<Output> {
    // 環境はそのまま引き継ぐ（exec と同じ）
    let env: Vec<String> = std::env::vars()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let (mut stdout, stdout_child) = pipe()?;
    let (mut stderr, stderr_child) = pipe()?;
    let stdin = match input {
        Some(_) => Some(pipe()?),
        None => None,
    };

    // 子の 0 / 1 / 2 に繋ぐ。いったん仮の番号へ写してから移す（先に上書きした fd を後で読まないため）
    let mut actions = FileActions::default();
    let sources = [
        stdin.as_ref().map(|(read, _)| read.as_raw_fd()),
        Some(stdout_child.as_raw_fd()),
        Some(stderr_child.as_raw_fd()),
    ];
    for (target, source) in sources.iter().enumerate() {
        if let Some(source) = source {
            actions.dup2(*source, SCRATCH_FD + target as i32);
        }
    }
    for (target, source) in sources.iter().enumerate() {
        let target = target as i32;
        match source {
            Some(_) => {
                actions.dup2(SCRATCH_FD + target, target);
                actions.close(SCRATCH_FD + target);
            }
            None => actions.open(target, "/dev/null", O_RDONLY),
        }
    }
    actions.chdir(&cwd.to_string_lossy());

    let pid = spawn(argv, &env, &actions, 0)?;
    // 子はもう自分の写しを持っている。こちらの写しを閉じないと、読み手は EOF を見ない
    drop(stdout_child);
    drop(stderr_child);
    let mut stdin = stdin.map(|(read, write)| {
        drop(read);
        write
    });
    set_nonblocking(&stdout)?;
    set_nonblocking(&stderr)?;
    if let Some(stdin) = &stdin {
        set_nonblocking(stdin)?;
    }
    let mut input = input.unwrap_or(&[]);

    let mut out = Vec::new();
    let mut err = Vec::new();
    let (mut out_done, mut err_done) = (false, false);
    let mut status = 0u32;
    let mut exited = false;
    loop {
        // 書き終えたら閉じる。閉じることが子から見た EOF である
        if let Some(file) = &mut stdin {
            if feed(file, &mut input)? {
                stdin = None;
            }
        }
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
                    return Err(io::Error::other(format!(
                        "waitpid failed (errno {errno})"
                    )));
                }
            }
        }
        if exited && out_done && err_done {
            break;
        }
        futex_sleep(Duration::from_millis(2));
    }
    let code = exit_code(status);
    tracing::debug!("{argv:?} exited with {code}");
    Ok(Output {
        code,
        stdout: out,
        stderr: err,
    })
}

/// 子を端末（pty の子の側、`/dev/pts/N`）の上で起動する。0/1/2 がその端末になり、子はその持ち主になる。
///
/// 本物の Unix と同じく、`POSIX_SPAWN_SETSID` で新しいセッションを作り、端末を 0 / 1 / 2 に開く
/// （それが子の制御端末になる）
pub fn spawn_on_tty(
    argv: &[&str],
    env: &[(String, String)],
    cwd: &Path,
    tty: &str,
) -> io::Result<u32> {
    let env: Vec<String> = env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    let mut actions = FileActions::default();
    for fd in 0..3 {
        actions.open(fd, tty, O_RDWR);
    }
    actions.chdir(&cwd.to_string_lossy());
    spawn(argv, &env, &actions, POSIX_SPAWN_SETSID)
        .map_err(|error| io::Error::other(format!("{error} on {tty}")))
}

/// 終わっていれば終了コード。まだなら `None`（待たない）
pub fn try_wait(pid: u32) -> io::Result<Option<i32>> {
    let mut status = 0u32;
    match unsafe { host_waitpid(pid, &mut status, WNOHANG) } {
        0 => Ok(Some(exit_code(status))),
        ERRNO_AGAIN => Ok(None),
        errno => Err(io::Error::other(format!("waitpid failed (errno {errno})"))),
    }
}

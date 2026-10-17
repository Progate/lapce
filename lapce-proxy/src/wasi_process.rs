//! 子プロセス（WASI・BrowserOS）。
//!
//! WASI preview1 に `fork` / `exec` は無く、std の `Command` も使えない。BrowserOS は Linux の syscall を
//! 1 つの口で受けている（→ linux_wasi.rs）ので、std の `Command` が Linux でしていることを、それで書く:
//! `pipe2(2)` でパイプを作り、`clone(2)` を `CLONE_VM | CLONE_VFORK` で呼び、子の段で `setsid(2)`・
//! `dup3(2)`・`close(2)`・`openat(2)`・`chdir(2)` を当ててから `execve(2)` し、`waitid(2)` で待つ。
//!
//! wasm は 1 本のスタックで 2 つの流れを走らせられないので、子の段は BrowserOS が clone の中で、この
//! モジュールの export `__browseros_clone_child_return` を呼び直して走らせる。子の段で execve が通るか
//! exit を呼ぶと syscall が 0 で戻る（Linux では戻らない）ので、そこで戻る。clone は親に子の pid を返す。
//!
//! ## 止まらずに待つ
//!
//! Lapce は wasi-threads で動いていて、全スレッドの syscall を 1 本のホスト役が順に
//! 捌く。パイプの blocking read や待つ syscall で止まると、**ほかのスレッド（画面を
//! 描くスレッドも）の syscall まで止まる**。だからこちらの持つパイプの端は `O_NONBLOCK` にし、
//! `waitid` は `WNOHANG` で聞き、待つのは futex（syscall を出さない）で行う

use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicPtr, Ordering},
    },
    time::Duration,
};

use crate::linux_wasi::{
    self, AT_FDCWD, EACCES, ENOENT, ENOTDIR, O_RDONLY, O_RDWR, SYS_CHDIR, SYS_CLONE,
    SYS_CLOSE, SYS_DUP3, SYS_EXECVE, SYS_EXIT, SYS_FCNTL64, SYS_OPENAT, SYS_PIPE2,
    SYS_SETSID, SYS_WAITID, address, raw,
};

/// clone(2) の flags（vfork の形）と fcntl(2) の命令
const CLONE_VM: i64 = 0x100;
const CLONE_VFORK: i64 = 0x4000;
const SIGCHLD: i64 = 17;
const F_GETFD: i64 = 1;
const F_SETFD: i64 = 2;
const FD_CLOEXEC: i64 = 1;
/// waitid(2) の idtype・options と、`siginfo_t` の欄（riscv32 の Linux）
const P_PID: i64 = 1;
const WNOHANG: i64 = 1;
const WEXITED: i64 = 4;
const SIGINFO_BYTES: usize = 128;
const SIGINFO_CODE: usize = 8;
const SIGINFO_PID: usize = 12;
const SIGINFO_STATUS: usize = 20;
const CLD_KILLED: i32 = 2;
const CLD_DUMPED: i32 = 3;
/// `PATH` が無いときに探す場所
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/bin:/bin";
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

/// 子の段で行うこと 1 件（`posix_spawn` の attr と file actions に当たる）
enum Action {
    /// `setsid(2)`。新しいセッションの頭になる（その後に開いた端末が制御端末になる）
    Setsid,
    /// `dup2(from, to)`
    Dup2(i32, i32),
    /// `close(fd)`
    Close(i32),
    /// `open(path, oflag)` を `fd` に置く（NUL で終わるパス）
    Open(i32, Vec<u8>, i64),
    /// `chdir(path)`（NUL で終わるパス）
    Chdir(Vec<u8>),
}

/// 子の段で使うもの。clone を呼ぶ前に揃えておき、子の入口はここを読むだけにする（子の段ではヒープを使わない）
struct ChildPlan {
    /// 順に execve を試すパス（NUL で終わる）
    candidates: Vec<Vec<u8>>,
    argv: Vec<u32>,
    envp: Vec<u32>,
    actions: Vec<Action>,
    /// 子の段で失敗したときの Linux の errno（0 なら起こせた）
    error: i32,
}

/// いま子の段にある起動（clone を呼んでいるあいだだけ指す）
static PLAN: AtomicPtr<ChildPlan> = AtomicPtr::new(std::ptr::null_mut());
/// PLAN を 1 度に 1 つの起動だけが使うようにする（スレッドどうし）
static SPAWNING: Mutex<()> = Mutex::new(());

/// clone(2) の vfork の形の子の入口。BrowserOS は clone を呼んだスレッドの上で、このインスタンスに入り直して
/// これを呼ぶ。attr と file actions を当ててから、候補のパスを順に execve する（execvp(3) と同じ）。子の段が
/// 終わったら（execve が通るか exit を呼んだら）syscall は 0 で戻るので、そこで戻る
#[unsafe(no_mangle)]
pub extern "C" fn __browseros_clone_child_return() {
    let plan = PLAN.load(Ordering::Acquire);
    if plan.is_null() {
        return;
    }
    // SAFETY: clone を呼んでいるあいだだけ PLAN が指し、子の段はそのあいだに走る
    let plan = unsafe { &mut *plan };
    for action in &plan.actions {
        let r = match action {
            Action::Setsid => raw(SYS_SETSID, [0; 6]),
            // 同じ番号なら close-on-exec を落とす（POSIX.1-2024 の posix_spawn_file_actions_adddup2）
            Action::Dup2(from, to) if from == to => {
                match raw(SYS_FCNTL64, [*from as i64, F_GETFD, 0, 0, 0, 0]) {
                    flags if flags < 0 => flags,
                    flags => raw(
                        SYS_FCNTL64,
                        [*from as i64, F_SETFD, flags & !FD_CLOEXEC, 0, 0, 0],
                    ),
                }
            }
            Action::Dup2(from, to) => {
                raw(SYS_DUP3, [*from as i64, *to as i64, 0, 0, 0, 0])
            }
            Action::Close(fd) => {
                // 閉じている fd を閉じても失敗にしない（musl と同じ）
                raw(SYS_CLOSE, [*fd as i64, 0, 0, 0, 0, 0]);
                0
            }
            Action::Open(fd, path, oflag) => {
                match raw(
                    SYS_OPENAT,
                    [AT_FDCWD, address(path.as_ptr()), *oflag, 0, 0, 0],
                ) {
                    opened if opened < 0 || opened == *fd as i64 => opened,
                    opened => {
                        let r = raw(SYS_DUP3, [opened, *fd as i64, 0, 0, 0, 0]);
                        raw(SYS_CLOSE, [opened, 0, 0, 0, 0, 0]);
                        r
                    }
                }
            }
            Action::Chdir(path) => {
                raw(SYS_CHDIR, [address(path.as_ptr()), 0, 0, 0, 0, 0])
            }
        };
        if r < 0 {
            plan.error = (-r) as i32;
            raw(SYS_EXIT, [127, 0, 0, 0, 0, 0]);
            return;
        }
    }
    let mut denied = false;
    let mut error = ENOENT;
    for candidate in &plan.candidates {
        let r = raw(
            SYS_EXECVE,
            [
                address(candidate.as_ptr()),
                address(plan.argv.as_ptr()),
                address(plan.envp.as_ptr()),
                0,
                0,
                0,
            ],
        );
        if r == 0 {
            return;
        }
        match (-r) as i32 {
            ENOENT | ENOTDIR => continue,
            EACCES => denied = true,
            other => {
                error = other;
                break;
            }
        }
    }
    plan.error = if error == ENOENT && denied {
        EACCES
    } else {
        error
    };
    raw(SYS_EXIT, [127, 0, 0, 0, 0, 0]);
}

/// NUL で終わる文字列の並びと、それを指す NULL で終わる `char *` の並び（argv / envp の渡し方）。
/// 文字列の持ち主（1 つ目）は、ポインターの並び（2 つ目）を使い終わるまで手放さないこと
fn c_strings<'a>(
    items: impl IntoIterator<Item = &'a str>,
) -> (Vec<Vec<u8>>, Vec<u32>) {
    let strings: Vec<Vec<u8>> = items.into_iter().map(linux_wasi::c_path).collect();
    let mut pointers: Vec<u32> =
        strings.iter().map(|s| s.as_ptr() as usize as u32).collect();
    pointers.push(0);
    (strings, pointers)
}

/// execvp(3) が試すパス。`/` の無い名前は、呼んだ側の `PATH` から探す
fn candidates_of(file: &str) -> Vec<Vec<u8>> {
    if file.contains('/') {
        return vec![linux_wasi::c_path(file)];
    }
    let search = std::env::var("PATH").unwrap_or_else(|_| DEFAULT_PATH.to_owned());
    search
        .split(':')
        // 空の要素は作業ディレクトリ（POSIX の取り決め）
        .map(|directory| if directory.is_empty() { "." } else { directory })
        .map(|directory| {
            linux_wasi::c_path(&format!(
                "{}/{file}",
                directory.trim_end_matches('/')
            ))
        })
        .collect()
}

/// `posix_spawnp(3)` と同じ。`argv[0]` を呼んだ側の `PATH` で探し、環境はこちらのものをそのまま渡す（exec と同じ）。
/// 子は clone(2) の vfork の形で作り、子の段で `actions` を当ててから execve する。子の段で失敗したら、
/// 127 で終わったその子を回収して errno を返す
fn spawn(argv: &[&str], env: &[String], actions: Vec<Action>) -> io::Result<u32> {
    let file = argv.first().copied().unwrap_or("");
    let (_argv_strings, argv_pointers) = c_strings(argv.iter().copied());
    let (_env_strings, env_pointers) = c_strings(env.iter().map(String::as_str));
    let mut plan = ChildPlan {
        candidates: candidates_of(file),
        argv: argv_pointers,
        envp: env_pointers,
        actions,
        error: 0,
    };
    let guard = SPAWNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    PLAN.store(&mut plan, Ordering::Release);
    let r = raw(SYS_CLONE, [CLONE_VM | CLONE_VFORK | SIGCHLD, 0, 0, 0, 0, 0]);
    PLAN.store(std::ptr::null_mut(), Ordering::Release);
    drop(guard);
    tracing::debug!("clone {argv:?} -> {r}");
    if r < 0 {
        return Err(io::Error::other(format!(
            "cannot start {file}: {}",
            linux_wasi::error_of((-r) as i32)
        )));
    }
    let pid = r as u32;
    if plan.error != 0 {
        let _ = wait_blocking(pid);
        return Err(io::Error::other(format!(
            "cannot start {file}: {}",
            linux_wasi::error_of(plan.error)
        )));
    }
    Ok(pid)
}

/// 子の段で失敗した子は、すでに終わっている。その子を回収する
fn wait_blocking(pid: u32) -> io::Result<i32> {
    loop {
        if let Some(code) = try_wait(pid)? {
            return Ok(code);
        }
        futex_sleep(Duration::from_millis(1));
    }
}

/// カーネルのパイプ（読み端, 書き端）
fn pipe() -> io::Result<(File, File)> {
    let mut fds = [-1i32; 2];
    linux_wasi::call(SYS_PIPE2, [address(fds.as_mut_ptr()), 0, 0, 0, 0, 0])?;
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
    let mut actions = Vec::new();
    let sources = [
        stdin.as_ref().map(|(read, _)| read.as_raw_fd()),
        Some(stdout_child.as_raw_fd()),
        Some(stderr_child.as_raw_fd()),
    ];
    for (target, source) in sources.iter().enumerate() {
        if let Some(source) = source {
            actions.push(Action::Dup2(*source, SCRATCH_FD + target as i32));
        }
    }
    for (target, source) in sources.iter().enumerate() {
        let target = target as i32;
        match source {
            Some(_) => {
                actions.push(Action::Dup2(SCRATCH_FD + target, target));
                actions.push(Action::Close(SCRATCH_FD + target));
            }
            None => actions.push(Action::Open(
                target,
                linux_wasi::c_path("/dev/null"),
                O_RDONLY,
            )),
        }
    }
    actions.push(Action::Chdir(linux_wasi::c_path(&cwd.to_string_lossy())));

    let pid = spawn(argv, &env, actions)?;
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
    let mut code = None;
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
        if code.is_none() {
            code = try_wait(pid)?;
        }
        if code.is_some() && out_done && err_done {
            break;
        }
        futex_sleep(Duration::from_millis(2));
    }
    let code = code.unwrap_or_default();
    tracing::debug!("{argv:?} exited with {code}");
    Ok(Output {
        code,
        stdout: out,
        stderr: err,
    })
}

/// 子を端末（pty の子の側、`/dev/pts/N`）の上で起動する。0/1/2 がその端末になり、子はその持ち主になる。
///
/// 本物の Linux と同じく、子の段で `setsid(2)` して新しいセッションを作り、端末を 0 / 1 / 2 に開く
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
    let mut actions = vec![Action::Setsid];
    for fd in 0..3 {
        actions.push(Action::Open(fd, linux_wasi::c_path(tty), O_RDWR));
    }
    actions.push(Action::Chdir(linux_wasi::c_path(&cwd.to_string_lossy())));
    spawn(argv, &env, actions)
        .map_err(|error| io::Error::other(format!("{error} on {tty}")))
}

/// 終わっていれば終了コード（シグナルで止まったら 128 + シグナル番号。シェルの `$?` と同じ表し方）。
/// まだなら `None`（`waitid(2)` の `WNOHANG`。待たない）
pub fn try_wait(pid: u32) -> io::Result<Option<i32>> {
    let mut info = [0u8; SIGINFO_BYTES];
    linux_wasi::call(
        SYS_WAITID,
        [
            P_PID,
            pid as i64,
            address(info.as_mut_ptr()),
            WEXITED | WNOHANG,
            0,
            0,
        ],
    )?;
    let field = |offset: usize| {
        i32::from_le_bytes(info[offset..offset + 4].try_into().unwrap())
    };
    // WNOHANG で終わっていなければ si_pid は 0
    if field(SIGINFO_PID) == 0 {
        return Ok(None);
    }
    let status = field(SIGINFO_STATUS);
    Ok(Some(match field(SIGINFO_CODE) {
        CLD_KILLED | CLD_DUMPED => 128 + (status & 0x7f),
        _ => status & 0xff,
    }))
}

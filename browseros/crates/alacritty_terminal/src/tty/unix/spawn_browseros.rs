//! 端末のシェルを pty の上で起こす（BrowserOS）。
//!
//! Linux 版は std の `Command` に `pre_exec` を足して、子の段で `setsid(2)`・`ioctl(TIOCSCTTY)`・シグナルの既定化を
//! する。`pre_exec` は fork(2) を要るが、BrowserOS は wasm のプロセスを fork で写せない（wasm は自分のスタックを
//! 写せない）。std の posix_spawn の道は musl では setsid を頼めない。
//!
//! そこで同じことを posix_spawn(3) でする。`POSIX_SPAWN_SETSID` で新しいセッションの頭になり、file actions で
//! pty の従（`/dev/pts/N`）を `O_NOCTTY` 無しで fd 0 に開く。セッションの頭が端末を開くと、それが制御端末になる
//! （Linux の `open(2)` の決まり。`TIOCSCTTY` と同じ結果）。1・2 は 0 を写す。止めていたシグナルは
//! `POSIX_SPAWN_SETSIGDEF` で既定に戻す（Linux 版の `signal(…, SIG_DFL)` と同じ）。
//!
//! 起こした子は pid で持ち、`waitpid(2)` で待つ（std の `Child` は std の spawn からしか作れない）

use std::ffi::{CString, OsStr, OsString};
use std::io::{Error, ErrorKind, Result};
use std::mem::MaybeUninit;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus};
use std::ptr;

/// 起こしたシェル。Linux 版の `std::process::Child` のうち、alacritty と Lapce が使うところだけを持つ
#[derive(Debug)]
pub struct Child {
    pid: libc::pid_t,
    status: Option<ExitStatus>,
}

impl Child {
    pub fn id(&self) -> u32 {
        self.pid as u32
    }

    /// 終わっていれば終了の状態を返す（`waitpid(WNOHANG)`）
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        let mut status = 0;
        let pid = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
        if pid < 0 {
            return Err(Error::last_os_error());
        }
        if pid == 0 {
            return Ok(None);
        }
        let status = ExitStatus::from_raw(status);
        self.status = Some(status);
        Ok(Some(status))
    }

    /// 終わるまで待つ
    pub fn wait(&mut self) -> Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let mut status = 0;
        loop {
            let pid = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if pid >= 0 {
                break;
            }
            let err = Error::last_os_error();
            if err.kind() != ErrorKind::Interrupted {
                return Err(err);
            }
        }
        let status = ExitStatus::from_raw(status);
        self.status = Some(status);
        Ok(status)
    }
}

fn c_string(bytes: &OsStr) -> Result<CString> {
    CString::new(bytes.as_bytes()).map_err(|_| Error::new(ErrorKind::InvalidInput, "nul byte in argument"))
}

/// 子の環境。いまの環境に `builder` の `env` / `env_remove` を重ねたもの（std の `Command` と同じ）
fn environment(builder: &Command) -> Result<Vec<CString>> {
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    for (key, value) in builder.get_envs() {
        vars.retain(|(existing, _)| existing != key);
        if let Some(value) = value {
            vars.push((key.to_os_string(), value.to_os_string()));
        }
    }
    vars.into_iter()
        .map(|(key, value)| {
            let mut entry = key.into_vec();
            entry.push(b'=');
            entry.extend_from_slice(value.as_bytes());
            CString::new(entry).map_err(|_| Error::new(ErrorKind::InvalidInput, "nul byte in environment"))
        })
        .collect()
}

/// pty の主（`master_fd`）に対する従の名前（`/dev/pts/N`）
fn slave_name(master_fd: libc::c_int) -> Result<CString> {
    let mut buf = [0 as libc::c_char; 64];
    let err = unsafe { libc::ptsname_r(master_fd, buf.as_mut_ptr(), buf.len()) };
    if err != 0 {
        return Err(Error::from_raw_os_error(err));
    }
    Ok(unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_owned())
}

fn check(err: libc::c_int) -> Result<()> {
    if err == 0 { Ok(()) } else { Err(Error::from_raw_os_error(err)) }
}

/// `builder` のプログラムを、`master_fd` の pty の従を制御端末にして起こす
pub fn spawn(builder: &Command, master_fd: libc::c_int) -> Result<Child> {
    let program = c_string(builder.get_program())?;
    let mut argv = vec![program.clone()];
    for arg in builder.get_args() {
        argv.push(c_string(arg)?);
    }
    let envp = environment(builder)?;
    let slave = slave_name(master_fd)?;
    let cwd = builder.get_current_dir().map(|dir| c_string(dir.as_os_str())).transpose()?;

    let mut argv_ptrs: Vec<*mut libc::c_char> = argv.iter().map(|arg| arg.as_ptr() as *mut _).collect();
    argv_ptrs.push(ptr::null_mut());
    let mut envp_ptrs: Vec<*mut libc::c_char> = envp.iter().map(|var| var.as_ptr() as *mut _).collect();
    envp_ptrs.push(ptr::null_mut());

    unsafe {
        let mut actions = MaybeUninit::<libc::posix_spawn_file_actions_t>::uninit();
        check(libc::posix_spawn_file_actions_init(actions.as_mut_ptr()))?;
        let mut actions = actions.assume_init();
        let mut attr = MaybeUninit::<libc::posix_spawnattr_t>::uninit();
        check(libc::posix_spawnattr_init(attr.as_mut_ptr()))?;
        let mut attr = attr.assume_init();

        let result = (|| -> Result<libc::pid_t> {
            // セッションの頭が O_NOCTTY 無しで開いた端末が制御端末になる
            check(libc::posix_spawn_file_actions_addopen(&mut actions, 0, slave.as_ptr(), libc::O_RDWR, 0))?;
            check(libc::posix_spawn_file_actions_adddup2(&mut actions, 0, 1))?;
            check(libc::posix_spawn_file_actions_adddup2(&mut actions, 0, 2))?;
            if let Some(cwd) = &cwd {
                check(libc::posix_spawn_file_actions_addchdir_np(&mut actions, cwd.as_ptr()))?;
            }

            let mut defaults = MaybeUninit::<libc::sigset_t>::uninit();
            libc::sigemptyset(defaults.as_mut_ptr());
            let mut defaults = defaults.assume_init();
            for signal in [libc::SIGCHLD, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGALRM, libc::SIGPIPE] {
                libc::sigaddset(&mut defaults, signal);
            }
            check(libc::posix_spawnattr_setsigdefault(&mut attr, &defaults))?;
            let mut mask = MaybeUninit::<libc::sigset_t>::uninit();
            libc::sigemptyset(mask.as_mut_ptr());
            check(libc::posix_spawnattr_setsigmask(&mut attr, mask.as_ptr()))?;
            let flags = libc::POSIX_SPAWN_SETSID as libc::c_int
                | libc::POSIX_SPAWN_SETSIGDEF
                | libc::POSIX_SPAWN_SETSIGMASK;
            check(libc::posix_spawnattr_setflags(&mut attr, flags as libc::c_short))?;

            let mut pid = 0;
            check(libc::posix_spawnp(
                &mut pid,
                program.as_ptr(),
                &actions,
                &attr,
                argv_ptrs.as_ptr(),
                envp_ptrs.as_ptr(),
            ))?;
            Ok(pid)
        })();

        libc::posix_spawn_file_actions_destroy(&mut actions);
        libc::posix_spawnattr_destroy(&mut attr);
        result.map(|pid| Child { pid, status: None })
    }
}

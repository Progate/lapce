//! Unix ドメインソケット（BrowserOS の WASI）。
//!
//! WASI（preview1）の std は Unix ドメインソケットを持たないが、BrowserOS は Linux の socket(2)・bind(2)・listen(2)・
//! accept4(2)・connect(2) を受ける（→ linux_wasi）。ここはそれで、ほかの OS の `interprocess` の local socket と同じ
//! 「名前（パス）で待ち受け、名前で繋ぐ」ことをする。繋いだソケットの fd は WASI の fd としても読み書きできるので、
//! `File` に包んで `Read`・`Write` で使う。

use std::{fs::File, io, os::fd::FromRawFd, path::Path};

use crate::linux_wasi::{self, address};

const SYS_SOCKET: i32 = 198;
const SYS_BIND: i32 = 200;
const SYS_LISTEN: i32 = 201;
const SYS_CONNECT: i32 = 203;
const SYS_ACCEPT4: i32 = 242;

const AF_UNIX: u16 = 1;
const SOCK_STREAM: i64 = 1;
/// 子を起こしたときに受け継がせない（Lapce が起こす端末やプラグインにソケットが漏れない）
const SOCK_CLOEXEC: i64 = linux_wasi::O_CLOEXEC;
/// 待ち受けの列の長さ（Linux の SOMAXCONN）
const BACKLOG: i64 = 4096;

/// `struct sockaddr_un`（Linux）
#[repr(C)]
struct SockaddrUn {
    sun_family: u16,
    sun_path: [u8; 108],
}

/// パスを `sockaddr_un` と、その長さ（終わりの NUL まで）にする
fn sockaddr_of(path: &Path) -> io::Result<(SockaddrUn, i64)> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut sun_path = [0u8; 108];
    if bytes.len() >= sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path is too long",
        ));
    }
    sun_path[..bytes.len()].copy_from_slice(bytes);
    let len = std::mem::size_of::<u16>() + bytes.len() + 1;
    Ok((
        SockaddrUn {
            sun_family: AF_UNIX,
            sun_path,
        },
        len as i64,
    ))
}

fn socket() -> io::Result<i32> {
    let fd = linux_wasi::call(
        SYS_SOCKET,
        [AF_UNIX as i64, SOCK_STREAM | SOCK_CLOEXEC, 0, 0, 0, 0],
    )?;
    Ok(fd as i32)
}

/// 待ち受けているソケット
pub struct LocalSocketListener {
    fd: i32,
}

impl LocalSocketListener {
    /// `path` で待ち受ける。そこに前のソケットが残っていると bind できないので、呼ぶ側が先に消しておく
    pub fn bind(path: &Path) -> io::Result<Self> {
        let fd = socket()?;
        // 先に包んでおき、bind・listen に失敗して返るときも Drop で fd を閉じる
        let listener = Self { fd };
        let (addr, len) = sockaddr_of(path)?;
        linux_wasi::call(SYS_BIND, [fd as i64, address(&addr), len, 0, 0, 0])?;
        linux_wasi::call(SYS_LISTEN, [fd as i64, BACKLOG, 0, 0, 0, 0])?;
        Ok(listener)
    }

    /// 繋いできた相手を 1 つ受け取るまで待つ
    pub fn accept(&self) -> io::Result<File> {
        let fd = linux_wasi::call(
            SYS_ACCEPT4,
            [self.fd as i64, 0, 0, SOCK_CLOEXEC, 0, 0],
        )?;
        // SAFETY: accept4 が返した fd は、このプロセスで誰も持っていない
        Ok(unsafe { File::from_raw_fd(fd as i32) })
    }

    /// 繋いできた相手を順に受け取る（`interprocess` の `incoming` と同じく、失敗した回も Err で返して続ける）
    pub fn incoming(&self) -> impl Iterator<Item = io::Result<File>> + '_ {
        std::iter::repeat_with(move || self.accept())
    }
}

impl Drop for LocalSocketListener {
    fn drop(&mut self) {
        // SAFETY: fd はこの値だけが持っている
        drop(unsafe { File::from_raw_fd(self.fd) });
    }
}

/// `path` で待ち受けているソケットへ繋ぐ
pub fn connect(path: &Path) -> io::Result<File> {
    let fd = socket()?;
    // SAFETY: socket が返した fd は、このプロセスで誰も持っていない
    let stream = unsafe { File::from_raw_fd(fd) };
    let (addr, len) = sockaddr_of(path)?;
    linux_wasi::call(SYS_CONNECT, [fd as i64, address(&addr), len, 0, 0, 0])?;
    Ok(stream)
}

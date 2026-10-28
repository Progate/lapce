//! BrowserOS の Linux の syscall の入口（WASI）。
//!
//! BrowserOS は Linux の syscall を 1 つの import（`browser_os_linux` の `syscall(nr, a0..a5) -> i64`）で受ける。
//! 番号・引数・errno は riscv32 の Linux（asm-generic）のままで、失敗は `-errno`（Linux の値）で返る。
//! ここはそれを呼んで、std の `io::Error` にするところだけを持つ。
//!
//! std の wasip1 は errno を WASI の番号で読む（`io::Error::from_raw_os_error` → `ErrorKind`）ので、
//! Linux の errno は同じ名前の WASI の番号へ引き直す。

use std::io;

#[link(wasm_import_module = "browser_os_linux")]
unsafe extern "C" {
    #[link_name = "syscall"]
    fn linux_syscall(
        nr: i32,
        a0: i64,
        a1: i64,
        a2: i64,
        a3: i64,
        a4: i64,
        a5: i64,
    ) -> i64;
}

/* syscall の番号（asm-generic の Linux） */
pub const SYS_INOTIFY_INIT1: i32 = 26;
pub const SYS_INOTIFY_ADD_WATCH: i32 = 27;
pub const SYS_INOTIFY_RM_WATCH: i32 = 28;
pub const SYS_DUP3: i32 = 24;
pub const SYS_FCNTL64: i32 = 25;
pub const SYS_IOCTL: i32 = 29;
pub const SYS_CHDIR: i32 = 49;
pub const SYS_OPENAT: i32 = 56;
pub const SYS_CLOSE: i32 = 57;
pub const SYS_PIPE2: i32 = 59;
pub const SYS_EXIT: i32 = 93;
pub const SYS_WAITID: i32 = 95;
pub const SYS_SETSID: i32 = 157;
pub const SYS_CLONE: i32 = 220;
pub const SYS_EXECVE: i32 = 221;

/* 値（Linux のもの） */
pub const AT_FDCWD: i64 = -100;
pub const O_RDONLY: i64 = 0;
pub const O_RDWR: i64 = 0o2;
pub const O_NOCTTY: i64 = 0o400;
pub const O_NONBLOCK: i64 = 0o4000;
pub const O_CLOEXEC: i64 = 0o2000000;

/// Linux の errno（使うものだけ）
pub const ENOENT: i32 = 2;
pub const EACCES: i32 = 13;
pub const ENOTDIR: i32 = 20;

/// Linux の errno（添字 + 1 が WASI の errno）。同じ名前のものを並べた（→ browser-os の wasi/linux/abi.ts）
const LINUX_ERRNO_OF_WASI: [i32; 76] = [
    7, 13, 98, 99, 97, 11, 114, 9, 74, 16, 125, 10, 103, 111, 104, 35, 89, 33, 122,
    17, 14, 27, 113, 43, 84, 115, 4, 22, 5, 106, 21, 40, 24, 31, 90, 72, 36, 100,
    102, 101, 23, 105, 19, 2, 8, 37, 67, 12, 42, 92, 28, 38, 107, 20, 39, 131, 88,
    95, 25, 6, 75, 130, 1, 32, 71, 93, 91, 34, 30, 29, 3, 116, 110, 26, 18, 1,
];

/// Linux の errno を、std が読む WASI の errno にした `io::Error`。知らない値は EIO（WASI の 29）
pub fn error_of(linux: i32) -> io::Error {
    let wasi = LINUX_ERRNO_OF_WASI
        .iter()
        .position(|&value| value == linux)
        .map_or(29, |index| index as i32 + 1);
    io::Error::from_raw_os_error(wasi)
}

/// syscall をそのまま呼ぶ。成功なら 0 以上、失敗なら `-errno`（Linux の値）
pub fn raw(nr: i32, args: [i64; 6]) -> i64 {
    // SAFETY: 引数のポインターは、呼ぶ側が呼び終えるまで生かしている領域を指す
    unsafe {
        linux_syscall(nr, args[0], args[1], args[2], args[3], args[4], args[5])
    }
}

/// syscall を呼ぶ。失敗は `io::Error`
pub fn call(nr: i32, args: [i64; 6]) -> io::Result<i64> {
    let result = raw(nr, args);
    if result < 0 {
        Err(error_of((-result) as i32))
    } else {
        Ok(result)
    }
}

/// 線形メモリの番地を syscall の引数にする（wasm32 のポインターは 32 ビット）
pub fn address<T>(pointer: *const T) -> i64 {
    pointer as usize as i64
}

/// NUL で終わるバイト列（パスを syscall に渡す形）
pub fn c_path(path: &str) -> Vec<u8> {
    let mut bytes = path.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

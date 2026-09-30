//! windowserver への接続 1 本。
//!
//! 行の読み書きと、画素ファイルへの書き込みだけを持つ。窓の状態（大きさ・焦点）は
//! `window.rs`、行の意味は `event_loop.rs` が持つ。

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const DEFAULT_DISPLAY: &str = "127.0.0.1:6000";

/// 窓をもらうのを待つ上限。windowserver が居なければ `surface` は返ってこない
const OPEN_TIMEOUT: Duration = Duration::from_secs(5);

/// 返事を待つあいだ、次に読みに行くまでの間隔
const OPEN_POLL: Duration = Duration::from_millis(2);

/// 係から届いた 1 行
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ServerMessage {
    Surface { id: u32, path: String, width: u32, height: u32 },
    Configure { width: u32, height: u32 },
    Focus { focused: bool },
    Pointer { phase: PointerPhase, x: f64, y: f64 },
    Scroll { x: f64, y: f64, delta_x: f64, delta_y: f64 },
    Key { state: KeyState, code: u16 },
    Text { text: String },
    Preedit { text: String },
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PointerPhase {
    Down,
    Move,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyState {
    Down,
    Up,
    Repeat,
}

/// `%XX` をほどく（windowserver の `encodeText` と対）
fn decode_text(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) =
                u8::from_str_radix(std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or(""), 16)
            {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 英数字以外を `%XX` にする（行が空白区切りなので）
pub(super) fn encode_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

pub(super) fn parse_line(line: &str) -> Option<ServerMessage> {
    let mut fields = line.split_ascii_whitespace();
    let kind = fields.next()?;
    let _id: u32 = fields.next()?.parse().ok()?;
    let rest: Vec<&str> = fields.collect();
    let number = |at: usize| -> Option<f64> { rest.get(at)?.parse::<f64>().ok() };
    let integer = |at: usize| -> Option<u32> { rest.get(at)?.parse::<u32>().ok() };
    Some(match kind {
        "surface" => ServerMessage::Surface {
            id: _id,
            path: (*rest.first()?).to_owned(),
            width: integer(1)?,
            height: integer(2)?,
        },
        "configure" => ServerMessage::Configure { width: integer(0)?, height: integer(1)? },
        "focus" => ServerMessage::Focus { focused: *rest.first()? == "in" },
        "pointer" => ServerMessage::Pointer {
            phase: match *rest.first()? {
                "down" => PointerPhase::Down,
                "move" => PointerPhase::Move,
                "up" => PointerPhase::Up,
                _ => return None,
            },
            x: number(1)?,
            y: number(2)?,
        },
        "scroll" => ServerMessage::Scroll {
            x: number(0)?,
            y: number(1)?,
            delta_x: number(2)?,
            delta_y: number(3)?,
        },
        "key" => ServerMessage::Key {
            state: match *rest.first()? {
                "down" => KeyState::Down,
                "up" => KeyState::Up,
                "repeat" => KeyState::Repeat,
                _ => return None,
            },
            code: rest.get(1)?.parse().ok()?,
        },
        "text" => ServerMessage::Text { text: decode_text(rest.first().copied().unwrap_or("")) },
        "preedit" => {
            ServerMessage::Preedit { text: decode_text(rest.first().copied().unwrap_or("")) }
        },
        "close" => ServerMessage::Close,
        _ => return None,
    })
}

/// `DISPLAY` を `/dev/tcp/<host>/<port>` にする
fn display_path() -> String {
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| DEFAULT_DISPLAY.to_owned());
    let (host, port) = display.rsplit_once(':').unwrap_or(("", "6000"));
    let host = if host.is_empty() { "127.0.0.1" } else { host };
    let port: u16 = port.parse().unwrap_or(6000);
    format!("/dev/tcp/{host}/{port}")
}

fn set_nonblocking(file: &File) -> io::Result<()> {
    let fd = file.as_raw_fd();
    // wasi-libc の fcntl(F_SETFL) は fd_fdstat_set_flags になる
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) struct Connection {
    socket: Mutex<File>,
    /// 行の途中で切れて届いたぶん
    pending: Mutex<Vec<u8>>,
    /// 画素のファイル。`surface` の返事で決まる
    pixels: Mutex<File>,
    pub(super) id: u32,
    closed: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").field("id", &self.id).finish_non_exhaustive()
    }
}

impl Connection {
    /// 繋いで窓を 1 つもらう。返事（`surface`）が来るまでここで待つ
    pub(super) fn open(width: u32, height: u32) -> io::Result<(Self, u32, u32)> {
        let path = display_path();
        let mut socket = OpenOptions::new().read(true).write(true).open(&path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not connect to the window server at {path}: {error}"),
            )
        })?;
        set_nonblocking(&socket)?;
        socket.write_all(format!("surface {width} {height}\n").as_bytes())?;

        let started = Instant::now();
        let mut pending = Vec::new();
        let mut early = Vec::new();
        loop {
            read_available(&mut socket, &mut pending)?;
            for line in take_lines(&mut pending) {
                match parse_line(&line) {
                    Some(ServerMessage::Surface { id, path, width, height }) => {
                        let pixels = OpenOptions::new().write(true).open(&path)?;
                        let connection = Self {
                            socket: Mutex::new(socket),
                            pending: Mutex::new(early_bytes(&early, &pending)),
                            pixels: Mutex::new(pixels),
                            id,
                            closed: std::sync::atomic::AtomicBool::new(false),
                        };
                        return Ok((connection, width, height));
                    },
                    _ => early.push(line),
                }
            }
            if started.elapsed() > OPEN_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the window server did not answer `surface`",
                ));
            }
            futex_sleep(OPEN_POLL);
        }
    }

    pub(super) fn send(&self, line: &str) {
        let mut socket = self.socket.lock().unwrap();
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        let mut written = 0;
        // nonblocking なので書き切れないことがある。行を割ると係が読み違えるので書き切る
        while written < bytes.len() {
            match socket.write(&bytes[written..]) {
                Ok(0) => return,
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    futex_sleep(Duration::from_millis(1))
                },
                Err(_) => return,
            }
        }
    }

    /// 届いているぶんの行を読む。**待たない**
    pub(super) fn poll(&self) -> Vec<ServerMessage> {
        let mut socket = self.socket.lock().unwrap();
        let mut pending = self.pending.lock().unwrap();
        if read_available(&mut socket, &mut pending).is_err() {
            self.closed.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        take_lines(&mut pending).iter().filter_map(|line| parse_line(line)).collect()
    }

    /// 読める行が届いているか（読んだぶんは次の `poll` に残す）。**待たない**
    pub(super) fn has_pending(&self) -> bool {
        let mut socket = self.socket.lock().unwrap();
        let mut pending = self.pending.lock().unwrap();
        if read_available(&mut socket, &mut pending).is_err() {
            self.closed.store(true, std::sync::atomic::Ordering::Relaxed);
            return true;
        }
        pending.contains(&b'\n')
    }

    pub(super) fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// RGBA の行をファイルへ置き、「この行を書き換えた」と言う
    pub(super) fn present(&self, rgba: &[u8], width: u32, y: u32, height: u32) {
        let stride = width as u64 * 4;
        {
            let mut pixels = self.pixels.lock().unwrap();
            if pixels.seek(SeekFrom::Start(y as u64 * stride)).is_err() {
                return;
            }
            let _ = pixels.write_all(rgba);
        }
        self.send(&format!("commit {} {y} {height}", self.id));
    }
}

fn early_bytes(early: &[String], pending: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for line in early {
        bytes.extend_from_slice(line.as_bytes());
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(pending);
    bytes
}

/// 読めるだけ読む。**接続が閉じたら `Err`**、何も無ければ `Ok(false)`
fn read_available(socket: &mut File, pending: &mut Vec<u8>) -> io::Result<bool> {
    let mut buffer = [0u8; 16 * 1024];
    let mut any = false;
    loop {
        match socket.read(&mut buffer) {
            Ok(0) => {
                return if any {
                    Ok(true)
                } else {
                    Err(io::Error::new(io::ErrorKind::UnexpectedEof, "window server closed"))
                };
            },
            Ok(count) => {
                pending.extend_from_slice(&buffer[..count]);
                any = true;
                if count < buffer.len() {
                    return Ok(true);
                }
            },
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(any),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

fn take_lines(pending: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = pending.drain(..=index).collect();
        let line = String::from_utf8_lossy(&line[..line.len() - 1]).trim().to_owned();
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines
}

/// syscall を出さずに眠る（→ mod.rs の「待ち方」）
pub(super) fn futex_sleep(duration: Duration) {
    let (_sender, receiver) = std::sync::mpsc::channel::<()>();
    let _ = receiver.recv_timeout(duration);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_lines() {
        assert_eq!(
            parse_line("surface 3 /run/windows/6000/3 800 600"),
            Some(ServerMessage::Surface {
                id: 3,
                path: "/run/windows/6000/3".into(),
                width: 800,
                height: 600
            })
        );
        assert_eq!(
            parse_line("pointer 3 down 10 20"),
            Some(ServerMessage::Pointer { phase: PointerPhase::Down, x: 10.0, y: 20.0 })
        );
        assert_eq!(
            parse_line("text 3 %E3%81%82"),
            Some(ServerMessage::Text { text: "あ".into() })
        );
        assert_eq!(parse_line("bogus 3"), None);
    }

    #[test]
    fn encodes_text() {
        assert_eq!(encode_text("a b"), "a%20b");
    }
}

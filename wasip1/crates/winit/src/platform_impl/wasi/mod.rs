//! BrowserOS の windowserver（WASI）。
//!
//! BrowserOS はブラウザーの中で動く小さな OS で、画面を独占する係（`windowserver`）が
//! `/dev/fb0` と `/dev/input/*` を持つ。アプリはそこへ繋いで窓をもらう。取り決めは
//! 2 つだけである。
//!
//! - **制御はソケットに 1 行ずつ書く。** `DISPLAY`（既定 `127.0.0.1:6000`）へ
//!   `/dev/tcp/<host>/<port>` を開いて繋ぐ（bash と同じ書き方）
//! - **画素はファイルに書く。** 窓をもらうと `/run/windows/<port>/<id>` が渡されるので、
//!   RGBA をそこへ置き、`commit` で「この行を書き換えた」と言う
//!
//! ```text
//!   surface <w> <h>                  窓をください（0 0 なら画面いっぱい）
//!   ◀ surface <id> <path> <w> <h>    画素はこのファイルへ
//!   commit <id> <y> <height>         この行を書き換えた
//!   ◀ configure / focus / pointer / scroll / key / text / preedit / close
//! ```
//!
//! 形は Redox の orbital と同じで、**窓 1 つにつき接続 1 本**にしている。接続が切れると
//! 窓も消える（windowserver がそう決めている）。
//!
//! ## 待ち方
//!
//! wasi-threads では、全スレッドの syscall を 1 つのホスト役が順に捌く。だから
//! **syscall の中で止まると、ほかのスレッドの syscall まで止まる**。ソケットは
//! nonblocking で読み、待つのは futex（`mpsc::Receiver::recv_timeout`。
//! `memory.atomic.wait32` で止まるので syscall を出さない）で行う。
#![cfg(target_os = "wasi")]

use smol_str::SmolStr;

pub(crate) use self::event_loop::{ActiveEventLoop, EventLoop};
pub use self::window::Window;
use crate::dpi::PhysicalPosition;
use crate::keyboard::Key;
use crate::monitor::VideoMode;

mod connection;
mod event_loop;
mod keymap;
mod window;

pub(crate) use crate::cursor::{
    NoCustomCursor as PlatformCustomCursor, NoCustomCursor as PlatformCustomCursorSource,
};
pub(crate) use crate::icon::NoIcon as PlatformIcon;

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub(crate) struct PlatformSpecificEventLoopAttributes {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PlatformSpecificWindowAttributes;

/// 画面の拡大率。ホストが `WINIT_SCALE_FACTOR` で渡す（既定 1）。
///
/// 画面の画素と見た目の 1px の比はホストが決めることで、窓の中からは測れない。
/// X11 の `WINIT_X11_SCALE_FACTOR` と同じ扱いにしている
pub(crate) fn scale_factor() -> f64 {
    std::env::var("WINIT_SCALE_FACTOR")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0)
}

/// `WINIT_WASI_TRACE=1` のとき、入力から描き上がりまでの時間を標準エラーへ出す
pub(crate) fn trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("WINIT_WASI_TRACE").is_ok_and(|value| value == "1"))
}

static INPUT_AT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// 最初に届いてまだ描かれていない入力の時刻を覚える
pub(crate) fn note_input() {
    if trace_enabled() {
        INPUT_AT.lock().unwrap().get_or_insert_with(std::time::Instant::now);
    }
}

pub(crate) fn take_input_instant() -> Option<std::time::Instant> {
    INPUT_AT.lock().unwrap().take()
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MonitorHandle;

impl MonitorHandle {
    pub fn name(&self) -> Option<String> {
        Some("fb0".to_owned())
    }

    pub fn position(&self) -> Option<PhysicalPosition<i32>> {
        Some((0, 0).into())
    }

    pub fn scale_factor(&self) -> f64 {
        scale_factor()
    }

    pub fn current_video_mode(&self) -> Option<VideoMode> {
        None
    }

    pub fn video_modes(&self) -> impl Iterator<Item = VideoMode> {
        std::iter::empty()
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct KeyEventExtra {
    pub key_without_modifiers: Key,
    pub text_with_all_modifiers: Option<SmolStr>,
}

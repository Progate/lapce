//! 貼り付け板（windowserver の selection）。
//!
//! 持ち主は windowserver で、焦点のある窓へだけ中身が届く（Wayland の
//! `wl_data_device.selection` と同じ）。届いた中身をここに控えておき、アプリは
//! 同期で読む。置くときは焦点のある窓の接続から `clipboard-set` を送る。
//!
//! ```text
//!   ◀ selection <id> <text>       中身が変わった・焦点が来た
//!   clipboard-set <id> <text>     コピーした
//! ```

use std::sync::{Arc, Mutex, Weak};

use super::connection::encode_text;
use super::window::Shared;

static SELECTION: Mutex<String> = Mutex::new(String::new());

/// 焦点のある窓。`clipboard-set` はその窓の番号で送る（係は焦点の無い窓からは受けない）
static FOCUSED: Mutex<Option<Weak<Shared>>> = Mutex::new(None);

pub(super) fn note_selection(text: String) {
    *SELECTION.lock().unwrap() = text;
}

pub(super) fn note_focus(shared: &Arc<Shared>, focused: bool) {
    let mut current = FOCUSED.lock().unwrap();
    if focused {
        *current = Some(Arc::downgrade(shared));
    } else if current.as_ref().is_some_and(|weak| weak.ptr_eq(&Arc::downgrade(shared))) {
        *current = None;
    }
}

/// 控えている中身
pub fn contents() -> String {
    SELECTION.lock().unwrap().clone()
}

/// 中身を置く。焦点のある窓が無ければ、このプロセスの中にだけ置く
pub fn set_contents(text: &str) {
    *SELECTION.lock().unwrap() = text.to_owned();
    let focused = FOCUSED.lock().unwrap().as_ref().and_then(Weak::upgrade);
    if let Some(shared) = focused {
        shared
            .connection
            .send(&format!("clipboard-set {} {}", shared.connection.id, encode_text(text)));
    }
}

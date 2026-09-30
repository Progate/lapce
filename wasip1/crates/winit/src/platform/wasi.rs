//! # WASI（BrowserOS の windowserver）
//!
//! 窓に絵を出す口は raw-window-handle に無いので、ここに生やしている。

use crate::window::Window;

/// WASI の窓にだけある口
pub trait WindowExtWasi {
    /// RGBA（1 画素 4 バイト、行の詰め物なし）を窓に出す。
    ///
    /// 大きさが窓と合わないフレームは捨てる。前のフレームから変わった行だけが送られる
    fn present_rgba(&self, rgba: &[u8], width: u32, height: u32);
}

impl WindowExtWasi for dyn Window + '_ {
    fn present_rgba(&self, rgba: &[u8], width: u32, height: u32) {
        if let Some(window) = self.as_any().downcast_ref::<crate::platform_impl::Window>() {
            window.present(rgba, width, height);
        }
    }
}

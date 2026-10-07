use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use super::connection::{encode_text, Connection};
use super::{scale_factor, ActiveEventLoop, MonitorHandle};
use crate::cursor::Cursor;
use crate::dpi::{PhysicalInsets, PhysicalPosition, PhysicalSize, Position, Size};
use crate::error::{NotSupportedError, RequestError};
use crate::monitor::MonitorHandle as CoreMonitorHandle;
use crate::window::{self, Fullscreen, ImePurpose, Window as CoreWindow, WindowId};

/// 窓とイベントループが両方から触る状態
#[derive(Debug)]
pub(super) struct Shared {
    pub(super) connection: Connection,
    pub(super) size: Mutex<PhysicalSize<u32>>,
    pub(super) focused: Mutex<bool>,
    title: Mutex<String>,
    /// 最後に送った画素。**変わった行だけを送る**ために比べる相手
    last_frame: Mutex<Vec<u8>>,
}

impl Shared {
    pub(super) fn id(&self) -> WindowId {
        WindowId::from_raw(self.connection.id as usize)
    }
}

pub struct Window {
    pub(super) shared: Arc<Shared>,
    redraws: Arc<Mutex<VecDeque<WindowId>>>,
    destroys: Arc<Mutex<VecDeque<WindowId>>>,
    wake: Mutex<Sender<()>>,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window").field("id", &self.shared.connection.id).finish_non_exhaustive()
    }
}

impl Window {
    pub(crate) fn new(
        el: &ActiveEventLoop,
        attrs: window::WindowAttributes,
    ) -> Result<Self, RequestError> {
        let scale = scale_factor();
        // 大きさを言わなければ画面いっぱい（`surface 0 0`）。窓が 1 つなら係が
        // どのみち画面いっぱいにする
        let (width, height): (u32, u32) = match attrs.surface_size {
            Some(size) if attrs.fullscreen.is_none() && !attrs.maximized => {
                size.to_physical::<u32>(scale).into()
            },
            _ => (0, 0),
        };
        let (connection, width, height) =
            Connection::open(width, height).map_err(|error| os_error!(format!("{error}")))?;
        if !attrs.title.is_empty() {
            connection.send(&format!("title {} {}", connection.id, encode_text(&attrs.title)));
        }
        let shared = Arc::new(Shared {
            connection,
            size: Mutex::new(PhysicalSize::new(width, height)),
            focused: Mutex::new(false),
            title: Mutex::new(attrs.title.clone()),
            last_frame: Mutex::new(Vec::new()),
        });
        el.creates.lock().unwrap().push_back(shared.clone());
        let _ = el.wake.lock().unwrap().send(());
        Ok(Self {
            shared,
            redraws: el.redraws.clone(),
            destroys: el.destroys.clone(),
            wake: Mutex::new(el.wake.lock().unwrap().clone()),
        })
    }

    fn wake(&self) {
        let _ = self.wake.lock().unwrap().send(());
    }

    /// RGBA（1 画素 4 バイト、行の詰め物なし）を窓に出す。
    ///
    /// 前に出したものと比べて、**変わった行だけ**をファイルへ書く。エディタのカーソルの
    /// 点滅のように数行しか変わらないフレームで、画面全体を書き直さずに済む
    pub fn present(&self, rgba: &[u8], width: u32, height: u32) {
        let started = std::time::Instant::now();
        let stride = width as usize * 4;
        if stride == 0 || rgba.len() < stride * height as usize {
            return;
        }
        let size = *self.shared.size.lock().unwrap();
        // 大きさが変わった途中のフレームは捨てる（係のファイルは新しい大きさで並んでいる）
        if size.width != width || size.height != height {
            return;
        }
        let rgba = &rgba[..stride * height as usize];
        let mut last = self.shared.last_frame.lock().unwrap();
        let (top, bottom) = if last.len() == rgba.len() {
            let rows = || (0..height as usize).map(|row| row * stride);
            let differs = |start: usize| last[start..start + stride] != rgba[start..start + stride];
            let Some(top) = rows().find(|start| differs(*start)) else {
                return;
            };
            let bottom = rows().rev().find(|start| differs(*start)).unwrap_or(top);
            (top / stride, bottom / stride + 1)
        } else {
            (0, height as usize)
        };
        self.shared.connection.present(
            &rgba[top * stride..bottom * stride],
            width,
            top as u32,
            (bottom - top) as u32,
        );
        if super::trace_enabled() {
            let since_input = super::take_input_instant().map(|at| at.elapsed().as_millis());
            eprintln!(
                "[winit-wasi] present rows {top}..{bottom} took {}ms, {}ms after input",
                started.elapsed().as_millis(),
                since_input.map_or("-".to_owned(), |ms| ms.to_string()),
            );
        }
        if last.len() != rgba.len() {
            last.clear();
            last.extend_from_slice(rgba);
        } else {
            last[top * stride..bottom * stride].copy_from_slice(&rgba[top * stride..bottom * stride]);
        }
    }
}

impl CoreWindow for Window {
    fn id(&self) -> WindowId {
        self.shared.id()
    }

    fn primary_monitor(&self) -> Option<CoreMonitorHandle> {
        Some(CoreMonitorHandle { inner: MonitorHandle })
    }

    fn available_monitors(&self) -> Box<dyn Iterator<Item = CoreMonitorHandle>> {
        Box::new(vec![CoreMonitorHandle { inner: MonitorHandle }].into_iter())
    }

    fn current_monitor(&self) -> Option<CoreMonitorHandle> {
        Some(CoreMonitorHandle { inner: MonitorHandle })
    }

    fn scale_factor(&self) -> f64 {
        scale_factor()
    }

    fn request_redraw(&self) {
        let window_id = self.id();
        let mut redraws = self.redraws.lock().unwrap();
        if !redraws.contains(&window_id) {
            redraws.push_back(window_id);
            drop(redraws);
            self.wake();
        }
    }

    fn pre_present_notify(&self) {}

    fn reset_dead_keys(&self) {}

    fn surface_position(&self) -> PhysicalPosition<i32> {
        (0, 0).into()
    }

    fn outer_position(&self) -> Result<PhysicalPosition<i32>, RequestError> {
        Ok((0, 0).into())
    }

    fn set_outer_position(&self, _position: Position) {}

    fn surface_size(&self) -> PhysicalSize<u32> {
        *self.shared.size.lock().unwrap()
    }

    fn request_surface_size(&self, _size: Size) -> Option<PhysicalSize<u32>> {
        // 大きさは係が決める（画面いっぱい・全画面）。頼む口は取り決めに無い
        None
    }

    fn outer_size(&self) -> PhysicalSize<u32> {
        self.surface_size()
    }

    fn safe_area(&self) -> PhysicalInsets<u32> {
        PhysicalInsets::new(0, 0, 0, 0)
    }

    fn set_min_surface_size(&self, _: Option<Size>) {}

    fn set_max_surface_size(&self, _: Option<Size>) {}

    fn title(&self) -> String {
        self.shared.title.lock().unwrap().clone()
    }

    fn set_title(&self, title: &str) {
        *self.shared.title.lock().unwrap() = title.to_owned();
        let id = self.shared.connection.id;
        self.shared.connection.send(&format!("title {id} {}", encode_text(title)));
    }

    fn set_transparent(&self, _transparent: bool) {}

    fn set_blur(&self, _blur: bool) {}

    fn set_visible(&self, _visible: bool) {}

    fn is_visible(&self) -> Option<bool> {
        Some(true)
    }

    fn surface_resize_increments(&self) -> Option<PhysicalSize<u32>> {
        None
    }

    fn set_surface_resize_increments(&self, _increments: Option<Size>) {}

    fn set_resizable(&self, _resizeable: bool) {}

    fn is_resizable(&self) -> bool {
        false
    }

    fn set_minimized(&self, _minimized: bool) {}

    fn is_minimized(&self) -> Option<bool> {
        Some(false)
    }

    fn set_maximized(&self, _maximized: bool) {}

    fn is_maximized(&self) -> bool {
        false
    }

    fn set_fullscreen(&self, _monitor: Option<Fullscreen>) {}

    fn fullscreen(&self) -> Option<Fullscreen> {
        None
    }

    fn set_decorations(&self, _decorations: bool) {}

    fn is_decorated(&self) -> bool {
        true
    }

    fn set_window_level(&self, _level: window::WindowLevel) {}

    fn set_window_icon(&self, _window_icon: Option<crate::icon::Icon>) {}

    /// 文字を打つ場所を windowserver へ言う（`cursor <id> x y w h`。Wayland の
    /// `zwp_text_input_v3.set_cursor_rectangle`）。ホストはそこへ隠し入力を動かし、IME の
    /// 候補をその下に出す。
    ///
    /// **`size` は使わず、`position` に高さ 1 の四角を置く。** floem / Lapce は行の下端を
    /// `position` に、候補を出してよい広さ（800×600）を `size` に渡してくる。そのまま
    /// 四角にすると、候補は 600 下へ押し出される
    fn set_ime_cursor_area(&self, position: Position, _size: Size) {
        let position: PhysicalPosition<i32> = position.to_physical(self.scale_factor());
        let id = self.shared.connection.id;
        self.shared.connection.send(&format!("cursor {id} {} {} 1 1", position.x, position.y));
    }

    /// 打てなくなったら、打つ場所も無いと言う（候補を出す場所を残さない）
    fn set_ime_allowed(&self, allowed: bool) {
        if !allowed {
            let id = self.shared.connection.id;
            self.shared.connection.send(&format!("cursor {id}"));
        }
    }

    fn set_ime_purpose(&self, _purpose: ImePurpose) {}

    fn focus_window(&self) {}

    fn request_user_attention(&self, _request_type: Option<window::UserAttentionType>) {}

    fn set_cursor(&self, _: Cursor) {}

    fn set_cursor_position(&self, _: Position) -> Result<(), RequestError> {
        Err(NotSupportedError::new("set_cursor_position is not supported").into())
    }

    fn set_cursor_grab(&self, _mode: window::CursorGrabMode) -> Result<(), RequestError> {
        Err(NotSupportedError::new("set_cursor_grab is not supported").into())
    }

    fn set_cursor_visible(&self, _visible: bool) {}

    fn drag_window(&self) -> Result<(), RequestError> {
        Err(NotSupportedError::new("drag_window is not supported").into())
    }

    fn drag_resize_window(&self, _direction: window::ResizeDirection) -> Result<(), RequestError> {
        Err(NotSupportedError::new("drag_resize_window is not supported").into())
    }

    fn show_window_menu(&self, _position: Position) {}

    fn set_cursor_hittest(&self, _hittest: bool) -> Result<(), RequestError> {
        Err(NotSupportedError::new("set_cursor_hittest is not supported").into())
    }

    fn set_enabled_buttons(&self, _buttons: window::WindowButtons) {}

    fn enabled_buttons(&self) -> window::WindowButtons {
        window::WindowButtons::all()
    }

    fn theme(&self) -> Option<window::Theme> {
        None
    }

    fn has_focus(&self) -> bool {
        *self.shared.focused.lock().unwrap()
    }

    fn set_theme(&self, _theme: Option<window::Theme>) {}

    fn set_content_protected(&self, _protected: bool) {}

    fn rwh_06_window_handle(&self) -> &dyn rwh_06::HasWindowHandle {
        self
    }

    fn rwh_06_display_handle(&self) -> &dyn rwh_06::HasDisplayHandle {
        self
    }
}

/// raw-window-handle に WASI の種類は無い。**描く口は [`Window::present`]** で、
/// ハンドルを受け取って描く softbuffer や wgpu はここでは使えない
impl rwh_06::HasWindowHandle for Window {
    fn window_handle(&self) -> Result<rwh_06::WindowHandle<'_>, rwh_06::HandleError> {
        Err(rwh_06::HandleError::NotSupported)
    }
}

impl rwh_06::HasDisplayHandle for Window {
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        Err(rwh_06::HandleError::NotSupported)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let id = self.shared.connection.id;
        self.shared.connection.send(&format!("destroy {id}"));
        self.destroys.lock().unwrap().push_back(self.id());
        self.wake();
    }
}

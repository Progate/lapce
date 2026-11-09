use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use smol_str::SmolStr;

use super::connection::{KeyState, PointerPhase, ServerMessage};
use super::keymap;
use super::window::Shared;
use super::{MonitorHandle, PlatformSpecificEventLoopAttributes};
use crate::application::ApplicationHandler;
use crate::error::{EventLoopError, NotSupportedError, RequestError};
use crate::event::{self, ElementState, Ime, Modifiers, StartCause};
use crate::event_loop::{
    ActiveEventLoop as RootActiveEventLoop, ControlFlow, DeviceEvents,
    EventLoopProxy as CoreEventLoopProxy, EventLoopProxyProvider,
    OwnedDisplayHandle as CoreOwnedDisplayHandle,
};
use crate::keyboard::{
    Key, KeyCode, KeyLocation, ModifiersKeys, ModifiersState, NativeKey, NativeKeyCode,
    PhysicalKey,
};
use crate::platform_impl::Window;
use crate::window::{CustomCursor as RootCustomCursor, CustomCursorSource, Theme, Window as CoreWindow, WindowId};

/// 何も起きていないとき、窓のソケットを見に行く間隔。
///
/// ソケットの読みを待つ口（poll）は、ほかのスレッドの syscall を止めてしまうので使えない
/// （→ mod.rs の「待ち方」）。代わりに futex で刻んで見に行く
const POLL_INTERVAL: Duration = Duration::from_millis(4);

#[derive(Default)]
struct WindowState {
    /// 押したと伝えたキー（evdev の番号）。**離したことは、押したと伝えたキーにだけ伝える**
    pressed_keys: std::collections::HashSet<u16>,
    modifiers: ModifiersState,
    pressed: ModifiersKeys,
    pointer: Option<(f64, f64)>,
    button_down: bool,
    /// Shift を押さずに打ったときの、そのキーの記号（evdev の番号 → 記号）。Shift を押したまま打ったキーの
    /// `key_without_modifiers` に使う。ホストは Shift の効いた記号しか添えないので、押さずに打ったときに覚えておく
    unshifted: std::collections::HashMap<u16, char>,
}

impl WindowState {
    fn modifiers(&self) -> Modifiers {
        Modifiers { state: self.modifiers, pressed_mods: self.pressed }
    }

    /// 文字を打った打鍵が使い切った Alt・Ctrl を除いた修飾（→ `process_key` の「使い切った修飾」）
    fn modifiers_without_consumed(&self) -> Modifiers {
        let mut state = self.modifiers;
        state.remove(ModifiersState::ALT | ModifiersState::CONTROL);
        let mut pressed = self.pressed;
        pressed.remove(ModifiersKeys::LALT | ModifiersKeys::RALT | ModifiersKeys::LCONTROL | ModifiersKeys::RCONTROL);
        Modifiers { state, pressed_mods: pressed }
    }

    fn update(&mut self, code: KeyCode, pressed: bool) -> bool {
        let before = (self.modifiers, self.pressed);
        let key = match code {
            KeyCode::ShiftLeft => ModifiersKeys::LSHIFT,
            KeyCode::ShiftRight => ModifiersKeys::RSHIFT,
            KeyCode::ControlLeft => ModifiersKeys::LCONTROL,
            KeyCode::ControlRight => ModifiersKeys::RCONTROL,
            KeyCode::AltLeft => ModifiersKeys::LALT,
            KeyCode::AltRight => ModifiersKeys::RALT,
            KeyCode::SuperLeft => ModifiersKeys::LSUPER,
            KeyCode::SuperRight => ModifiersKeys::RSUPER,
            _ => return false,
        };
        self.pressed.set(key, pressed);
        let mut state = ModifiersState::empty();
        let any = |left, right| self.pressed.intersects(left | right);
        state.set(ModifiersState::SHIFT, any(ModifiersKeys::LSHIFT, ModifiersKeys::RSHIFT));
        state.set(ModifiersState::CONTROL, any(ModifiersKeys::LCONTROL, ModifiersKeys::RCONTROL));
        state.set(ModifiersState::ALT, any(ModifiersKeys::LALT, ModifiersKeys::RALT));
        state.set(ModifiersState::SUPER, any(ModifiersKeys::LSUPER, ModifiersKeys::RSUPER));
        self.modifiers = state;
        before != (self.modifiers, self.pressed)
    }
}

pub struct EventLoop {
    windows: Vec<(Arc<Shared>, WindowState)>,
    window_target: ActiveEventLoop,
    wake_receiver: Receiver<()>,
}

impl EventLoop {
    pub(crate) fn new(_: &PlatformSpecificEventLoopAttributes) -> Result<Self, EventLoopError> {
        let (wake, wake_receiver) = mpsc::channel();
        let (user_events_sender, user_events_receiver) = mpsc::sync_channel(1);
        Ok(Self {
            windows: Vec::new(),
            window_target: ActiveEventLoop {
                control_flow: Cell::new(ControlFlow::default()),
                exit: Cell::new(false),
                creates: Mutex::new(VecDeque::new()),
                redraws: Arc::new(Mutex::new(VecDeque::new())),
                destroys: Arc::new(Mutex::new(VecDeque::new())),
                wake: Mutex::new(wake.clone()),
                event_loop_proxy: Arc::new(EventLoopProxy {
                    user_events_sender,
                    wake: Mutex::new(wake),
                }),
                user_events_receiver,
            },
            wake_receiver,
        })
    }

    fn process_message<A: ApplicationHandler>(
        shared: &Arc<Shared>,
        state: &mut WindowState,
        message: ServerMessage,
        target: &ActiveEventLoop,
        app: &mut A,
    ) {
        let window_id = shared.id();
        let emit = |app: &mut A, event| app.window_event(target, window_id, event);
        if matches!(message, ServerMessage::Scroll { .. } | ServerMessage::Key { .. } | ServerMessage::Pointer { .. }) {
            super::note_input();
        }
        match message {
            ServerMessage::Surface { .. } => {},
            ServerMessage::Configure { width, height } => {
                *shared.size.lock().unwrap() = (width, height).into();
                emit(app, event::WindowEvent::SurfaceResized((width, height).into()));
                target.request_redraw(window_id);
            },
            ServerMessage::Focus { focused } => {
                *shared.focused.lock().unwrap() = focused;
                super::clipboard::note_focus(shared, focused);
                emit(app, event::WindowEvent::Focused(focused));
            },
            ServerMessage::Pointer { phase, x, y, button } => {
                let position = dpi::PhysicalPosition::new(x, y);
                if state.pointer.is_none() {
                    emit(app, event::WindowEvent::PointerEntered {
                        device_id: None,
                        primary: true,
                        position,
                        kind: event::PointerKind::Mouse,
                    });
                }
                if state.pointer != Some((x, y)) {
                    state.pointer = Some((x, y));
                    emit(app, event::WindowEvent::PointerMoved {
                        device_id: None,
                        primary: true,
                        position,
                        source: event::PointerSource::Mouse,
                    });
                }
                let pressed = match phase {
                    PointerPhase::Down => Some(ElementState::Pressed),
                    PointerPhase::Up => Some(ElementState::Released),
                    PointerPhase::Move => None,
                };
                if let Some(button_state) = pressed {
                    let down = button_state == ElementState::Pressed;
                    if state.button_down != down {
                        state.button_down = down;
                        emit(app, event::WindowEvent::PointerButton {
                            device_id: None,
                            primary: true,
                            state: button_state,
                            position,
                            button: match button {
                                2 => event::MouseButton::Middle,
                                3 => event::MouseButton::Right,
                                _ => event::MouseButton::Left,
                            }
                            .into(),
                        });
                    }
                }
            },
            ServerMessage::Scroll { x, y, delta_x, delta_y } => {
                if state.pointer != Some((x, y)) {
                    state.pointer = Some((x, y));
                    emit(app, event::WindowEvent::PointerMoved {
                        device_id: None,
                        primary: true,
                        position: (x, y).into(),
                        source: event::PointerSource::Mouse,
                    });
                }
                // DOM の wheel は「下へ巻くと正」、winit は「中身が下へ動くと正」
                emit(app, event::WindowEvent::MouseWheel {
                    device_id: None,
                    delta: event::MouseScrollDelta::PixelDelta((-delta_x, -delta_y).into()),
                    phase: event::TouchPhase::Moved,
                });
            },
            ServerMessage::Key { state: key_state, code, text, symbol } => {
                Self::process_key(state, key_state, code, text, symbol, &mut |event| emit(app, event));
            },
            ServerMessage::Text { text } => {
                emit(app, event::WindowEvent::Ime(Ime::Preedit(String::new(), None)));
                emit(app, event::WindowEvent::Ime(Ime::Commit(text)));
            },
            ServerMessage::Preedit { text } => {
                let cursor = if text.is_empty() { None } else { Some((text.len(), text.len())) };
                emit(app, event::WindowEvent::Ime(Ime::Preedit(text, cursor)));
            },
            ServerMessage::Selection { text } => super::clipboard::note_selection(text),
            ServerMessage::Close => emit(app, event::WindowEvent::CloseRequested),
        }
    }

    fn process_key(
        state: &mut WindowState,
        key_state: KeyState,
        code: u16,
        host_text: Option<String>,
        host_symbol: Option<String>,
        emit: &mut dyn FnMut(event::WindowEvent),
    ) {
        let pressed = key_state != KeyState::Up;
        /*
         * **押したと伝えていないキーの「離した」は捨てる。** IME が握ったキー（確定の Enter
         * など）はブラウザーが keydown を IME へ渡すので、こちらには keyup だけが届く。
         * それをそのまま伝えると、受け取った側が Enter として扱って改行が入る。
         * ネイティブの macOS 版 winit も、IME が握ったキーの離した出来事は送らない
         */
        if pressed {
            state.pressed_keys.insert(code);
        } else if !state.pressed_keys.remove(&code) {
            return;
        }
        let info = keymap::lookup(code);
        let physical_key = match &info {
            Some(info) => PhysicalKey::Code(info.code),
            None => PhysicalKey::Unidentified(NativeKeyCode::Xkb(code as u32 + 8)),
        };
        let modifiers_changed = match &info {
            Some(info) => state.update(info.code, pressed),
            None => false,
        };

        let shift = state.modifiers.shift_key();
        let control = state.modifiers.control_key();
        /*
         * **使い切った修飾。** ホストが文字（`text`）を添えたら、Alt・Ctrl を押していてもその打鍵は文字を打つ
         * （windowserver の `key` の取り決め）。Mac の Option（Option+¥ は `\`）や Windows の AltGr（Ctrl+Alt として届く。
         * ドイツ語の配列の AltGr+Q は `@`）がそれで、Alt・Ctrl はその文字を選ぶのに使い切られている。xkb が
         * 3 段目を選んだ修飾を consumed として外すのと同じく、この打鍵のあいだだけ Alt・Ctrl を外して知らせる。
         * 外さないと、アプリ（Lapce・floem）は Alt・Ctrl つきのショートカットとみなして文字を捨てる。
         * 文字を添えられないキー（Option+← など）は Alt のまま届くので、ショートカットはそのまま効く
         */
        let consumed = pressed && host_text.is_some() && (control || state.modifiers.alt_key());
        let control = control && !consumed;
        /*
         * **記号はホストが添えたものを使う。** キーの番号から文字を作り直すと、配列を知らないので US 配列の文字になり、
         * JIS の Shift+2 が `@` になる。ホストは学習者の配列で決めた記号（Shift は効き、Ctrl・Alt・⌘ は効かない。
         * xkb の keysym と同じ）を添えるので、それを `logical_key` にする。添えられていないときだけ US 配列の表に落とす
         */
        let symbol = host_symbol.as_deref().and_then(single_char);
        if let (Some(symbol), false) = (symbol, shift) {
            state.unshifted.insert(code, symbol);
        }
        let fallback = info.as_ref().and_then(|info| info.chars).map(|(lower, upper)| if shift { upper } else { lower });
        let named = info.as_ref().and_then(|info| info.named);
        let character = match (symbol, named) {
            // 空白は文字を添えられても名前のキーのまま（Linux の winit も `NamedKey::Space` にする）
            (Some(_), Some(crate::keyboard::NamedKey::Space)) => None,
            (Some(symbol), _) => Some(symbol),
            (None, Some(_)) => None,
            (None, None) => fallback,
        };
        let (logical_key, key_without_modifiers, text, text_with_all_modifiers) = match (character, named) {
            (Some(character), _) => {
                let logical = Key::Character(SmolStr::from(character.to_string()));
                /*
                 * Shift を押さずに打った記号（Lapce はショートカットをこれで引く）。Shift を押したままなら、押さずに打った
                 * ときに覚えた記号を使い、まだ覚えていなければ英字は小文字に、ほかは US 配列の表にする
                 */
                let unshifted = if !shift {
                    Some(character)
                } else {
                    state.unshifted.get(&code).copied().or_else(|| {
                        if character.is_alphabetic() {
                            character.to_lowercase().next()
                        } else {
                            info.as_ref().and_then(|info| info.chars).map(|(lower, _)| lower)
                        }
                    })
                };
                let without = Key::Character(SmolStr::from(unshifted.unwrap_or(character).to_string()));
                // 打たれる文字はホストが決めたもの。Ctrl などを押したままで添えられていなければ、Linux の winit と同じく記号の文字
                let text = pressed.then(|| SmolStr::from(host_text.clone().unwrap_or_else(|| character.to_string())));
                // Ctrl を押していれば制御文字になる（xkb の XkbToControl と同じ。Ctrl+A → 0x01、JIS の Ctrl+@ → NUL）
                let all = match control.then(|| control_character(character)).flatten() {
                    Some(control_char) => pressed.then(|| SmolStr::from(control_char.to_string())),
                    None => text.clone(),
                };
                (logical, without, text, all)
            },
            (None, Some(named)) => {
                let text = match named {
                    crate::keyboard::NamedKey::Enter => Some(SmolStr::new("\r")),
                    crate::keyboard::NamedKey::Tab => Some(SmolStr::new("\t")),
                    crate::keyboard::NamedKey::Space => Some(SmolStr::new(" ")),
                    _ => None,
                };
                let key = Key::Named(named);
                (key.clone(), key, text.clone().filter(|_| pressed), text.filter(|_| pressed))
            },
            (None, None) => {
                let key = Key::Unidentified(NativeKey::Xkb(code as u32 + 8));
                (key.clone(), key, None, None)
            },
        };
        let location = match info.as_ref().map(|info| info.code) {
            _ if info.as_ref().is_some_and(|info| info.numpad) => KeyLocation::Numpad,
            Some(
                KeyCode::ShiftLeft | KeyCode::ControlLeft | KeyCode::AltLeft | KeyCode::SuperLeft,
            ) => KeyLocation::Left,
            Some(
                KeyCode::ShiftRight
                | KeyCode::ControlRight
                | KeyCode::AltRight
                | KeyCode::SuperRight,
            ) => KeyLocation::Right,
            _ => KeyLocation::Standard,
        };

        if consumed {
            emit(event::WindowEvent::ModifiersChanged(state.modifiers_without_consumed()));
        }
        emit(event::WindowEvent::KeyboardInput {
            device_id: None,
            event: event::KeyEvent {
                physical_key,
                logical_key,
                text,
                location,
                state: if pressed { ElementState::Pressed } else { ElementState::Released },
                repeat: key_state == KeyState::Repeat,
                platform_specific: super::KeyEventExtra {
                    key_without_modifiers,
                    text_with_all_modifiers,
                },
            },
            is_synthetic: false,
        });
        if modifiers_changed || consumed {
            emit(event::WindowEvent::ModifiersChanged(state.modifiers()));
        }
    }

    pub fn run_app<A: ApplicationHandler>(mut self, mut app: A) -> Result<(), EventLoopError> {
        let mut start_cause = StartCause::Init;
        loop {
            app.new_events(&self.window_target, start_cause);

            if start_cause == StartCause::Init {
                app.can_create_surfaces(&self.window_target);
            }

            // 開いた窓。最初の大きさを知らせる
            while let Some(shared) = {
                let mut creates = self.window_target.creates.lock().unwrap();
                creates.pop_front()
            } {
                let size = *shared.size.lock().unwrap();
                let window_id = shared.id();
                self.windows.push((shared, WindowState::default()));
                app.window_event(
                    &self.window_target,
                    window_id,
                    event::WindowEvent::SurfaceResized(size),
                );
                self.window_target.request_redraw(window_id);
            }

            // 閉じた窓
            while let Some(destroy_id) = {
                let mut destroys = self.window_target.destroys.lock().unwrap();
                destroys.pop_front()
            } {
                app.window_event(&self.window_target, destroy_id, event::WindowEvent::Destroyed);
                self.windows.retain(|(shared, _)| shared.id() != destroy_id);
            }

            // 窓から届いた行
            let mut index = 0;
            while index < self.windows.len() {
                let shared = self.windows[index].0.clone();
                let messages = coalesce_scrolls(shared.connection.poll());
                if super::trace_enabled() && messages.len() > 1 {
                    eprintln!("[winit-wasi] {} messages in one poll", messages.len());
                }
                for message in messages {
                    let state = &mut self.windows[index].1;
                    Self::process_message(&shared, state, message, &self.window_target, &mut app);
                }
                if shared.connection.is_closed() {
                    // 係が居なくなった。窓は消えているので、閉じてほしいと伝える
                    app.window_event(
                        &self.window_target,
                        shared.id(),
                        event::WindowEvent::CloseRequested,
                    );
                }
                index += 1;
            }

            while self.window_target.user_events_receiver.try_recv().is_ok() {
                app.proxy_wake_up(&self.window_target);
            }

            // 描き直し。ロックを持ったまま呼ぶと、その中で request_redraw されて止まる
            while let Some(window_id) = {
                let mut redraws = self.window_target.redraws.lock().unwrap();
                redraws.pop_front()
            } {
                app.window_event(
                    &self.window_target,
                    window_id,
                    event::WindowEvent::RedrawRequested,
                );
            }

            app.about_to_wait(&self.window_target);

            if self.window_target.exiting() {
                break;
            }

            let requested_resume = match self.window_target.control_flow() {
                ControlFlow::Poll => {
                    start_cause = StartCause::Poll;
                    continue;
                },
                ControlFlow::Wait => None,
                ControlFlow::WaitUntil(instant) => Some(instant),
            };

            // 起こされるか、期限が来るか、窓の行が届くまで待つ
            let start = Instant::now();
            start_cause = loop {
                let now = Instant::now();
                if let Some(deadline) = requested_resume {
                    if now >= deadline {
                        break StartCause::ResumeTimeReached {
                            start,
                            requested_resume: deadline,
                        };
                    }
                }
                let slice = requested_resume
                    .map(|deadline| deadline.saturating_duration_since(now).min(POLL_INTERVAL))
                    .unwrap_or(POLL_INTERVAL);
                match self.wake_receiver.recv_timeout(slice) {
                    Ok(()) => {
                        while self.wake_receiver.try_recv().is_ok() {}
                        break StartCause::WaitCancelled { start, requested_resume };
                    },
                    Err(RecvTimeoutError::Disconnected) => {
                        break StartCause::WaitCancelled { start, requested_resume };
                    },
                    Err(RecvTimeoutError::Timeout) => {},
                }
                if self.windows.iter().any(|(shared, _)| shared.connection.has_pending()) {
                    break StartCause::WaitCancelled { start, requested_resume };
                }
            };
        }

        app.exiting(&self.window_target);

        Ok(())
    }

    pub fn window_target(&self) -> &dyn RootActiveEventLoop {
        &self.window_target
    }
}

/// 続けて届いた巻き上げを 1 つにまとめる。
///
/// 描いているあいだに届いたぶんを 1 つずつ描くと、指を止めたあとも画面が遅れて動き続ける。
/// 量は足し合わせるので、巻いた距離は変わらない（ブラウザーが wheel をまとめるのと同じ）
fn coalesce_scrolls(messages: Vec<ServerMessage>) -> Vec<ServerMessage> {
    let mut out: Vec<ServerMessage> = Vec::with_capacity(messages.len());
    for message in messages {
        if let (
            Some(ServerMessage::Scroll { x: last_x, y: last_y, delta_x: last_dx, delta_y: last_dy }),
            ServerMessage::Scroll { x, y, delta_x, delta_y },
        ) = (out.last_mut(), &message)
        {
            *last_x = *x;
            *last_y = *y;
            *last_dx += delta_x;
            *last_dy += delta_y;
            continue;
        }
        out.push(message);
    }
    out
}

pub struct EventLoopProxy {
    user_events_sender: mpsc::SyncSender<()>,
    wake: Mutex<Sender<()>>,
}

impl EventLoopProxyProvider for EventLoopProxy {
    fn wake_up(&self) {
        // 送れないのは、前に起こしたのをまだ読んでいないから（まとめて 1 回でよい）
        if self.user_events_sender.try_send(()).is_ok() {
            let _ = self.wake.lock().unwrap().send(());
        }
    }
}

impl Unpin for EventLoopProxy {}

pub struct ActiveEventLoop {
    control_flow: Cell<ControlFlow>,
    exit: Cell<bool>,
    pub(super) creates: Mutex<VecDeque<Arc<Shared>>>,
    pub(super) redraws: Arc<Mutex<VecDeque<WindowId>>>,
    pub(super) destroys: Arc<Mutex<VecDeque<WindowId>>>,
    pub(super) wake: Mutex<Sender<()>>,
    event_loop_proxy: Arc<EventLoopProxy>,
    user_events_receiver: mpsc::Receiver<()>,
}

impl ActiveEventLoop {
    fn request_redraw(&self, window_id: WindowId) {
        let mut redraws = self.redraws.lock().unwrap();
        if !redraws.contains(&window_id) {
            redraws.push_back(window_id);
        }
    }
}

impl RootActiveEventLoop for ActiveEventLoop {
    fn create_proxy(&self) -> CoreEventLoopProxy {
        CoreEventLoopProxy::new(self.event_loop_proxy.clone())
    }

    fn create_window(
        &self,
        window_attributes: crate::window::WindowAttributes,
    ) -> Result<Box<dyn CoreWindow>, RequestError> {
        Ok(Box::new(Window::new(self, window_attributes)?))
    }

    fn create_custom_cursor(
        &self,
        _: CustomCursorSource,
    ) -> Result<RootCustomCursor, RequestError> {
        Err(NotSupportedError::new("create_custom_cursor is not supported").into())
    }

    fn available_monitors(&self) -> Box<dyn Iterator<Item = crate::monitor::MonitorHandle>> {
        Box::new(std::iter::once(crate::monitor::MonitorHandle { inner: MonitorHandle }))
    }

    fn system_theme(&self) -> Option<Theme> {
        None
    }

    fn primary_monitor(&self) -> Option<crate::monitor::MonitorHandle> {
        Some(crate::monitor::MonitorHandle { inner: MonitorHandle })
    }

    fn listen_device_events(&self, _allowed: DeviceEvents) {}

    fn set_control_flow(&self, control_flow: ControlFlow) {
        self.control_flow.set(control_flow)
    }

    fn control_flow(&self) -> ControlFlow {
        self.control_flow.get()
    }

    fn exit(&self) {
        self.exit.set(true);
    }

    fn exiting(&self) -> bool {
        self.exit.get()
    }

    fn owned_display_handle(&self) -> CoreOwnedDisplayHandle {
        CoreOwnedDisplayHandle::new(Arc::new(OwnedDisplayHandle))
    }

    fn rwh_06_handle(&self) -> &dyn rwh_06::HasDisplayHandle {
        self
    }
}

impl rwh_06::HasDisplayHandle for ActiveEventLoop {
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        Err(rwh_06::HandleError::NotSupported)
    }
}

#[derive(Clone)]
pub(crate) struct OwnedDisplayHandle;

impl rwh_06::HasDisplayHandle for OwnedDisplayHandle {
    fn display_handle(&self) -> Result<rwh_06::DisplayHandle<'_>, rwh_06::HandleError> {
        Err(rwh_06::HandleError::NotSupported)
    }
}

/// 1 文字の文字列ならその文字
fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let first = chars.next()?;
    chars.next().is_none().then_some(first)
}

/// Ctrl と組んだ記号が産む制御文字（xkb の `XkbToControl` と同じ。`@`〜`~` と空白は下位 5 ビット、`2` は NUL、
/// `3`〜`7` は ESC〜US、`8` は DEL、`/` は US）
fn control_character(character: char) -> Option<char> {
    let byte = u8::try_from(u32::from(character)).ok()?;
    Some(char::from(match byte {
        b'@'..=b'~' | b' ' => byte & 0x1f,
        b'2' => 0,
        b'3'..=b'7' => byte - b'3' + 0x1b,
        b'8' => 0x7f,
        b'/' => 0x1f,
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyboard::NamedKey;

    /// キーを 1 つ流して、届いた KeyboardInput を返す
    fn press(state: &mut WindowState, code: u16, text: Option<&str>, symbol: Option<&str>) -> event::KeyEvent {
        let mut events = Vec::new();
        EventLoop::process_key(
            state,
            KeyState::Down,
            code,
            text.map(str::to_owned),
            symbol.map(str::to_owned),
            &mut |event| events.push(event),
        );
        events
            .into_iter()
            .find_map(|event| match event {
                event::WindowEvent::KeyboardInput { event, .. } => Some(event),
                _ => None,
            })
            .unwrap()
    }

    fn character(text: &str) -> Key {
        Key::Character(SmolStr::new(text))
    }

    #[test]
    fn jis_shift_2_types_the_quote() {
        let mut state = WindowState::default();
        press(&mut state, 42, None, None);
        let event = press(&mut state, 3, Some("\""), Some("\""));
        assert_eq!(event.logical_key, character("\""));
        assert_eq!(event.text.as_deref(), Some("\""));
        assert_eq!(event.platform_specific.key_without_modifiers, character("2"));
    }

    #[test]
    fn control_uses_the_host_symbol_not_the_us_position() {
        let mut state = WindowState::default();
        press(&mut state, 29, None, None);
        // JIS の @ は US 配列の [ の位置（26）にある
        let event = press(&mut state, 26, None, Some("@"));
        assert_eq!(event.logical_key, character("@"));
        assert_eq!(event.platform_specific.key_without_modifiers, character("@"));
        assert_eq!(event.platform_specific.text_with_all_modifiers.as_deref(), Some("\0"));
        // JIS の ] は US 配列の \ の位置（43）にある
        let event = press(&mut state, 43, None, Some("]"));
        assert_eq!(event.platform_specific.key_without_modifiers, character("]"));
    }

    #[test]
    fn shifted_key_remembers_the_unshifted_symbol() {
        let mut state = WindowState::default();
        press(&mut state, 26, Some("@"), Some("@"));
        press(&mut state, 42, None, None);
        let event = press(&mut state, 26, Some("`"), Some("`"));
        assert_eq!(event.logical_key, character("`"));
        assert_eq!(event.platform_specific.key_without_modifiers, character("@"));
    }

    #[test]
    fn without_a_symbol_falls_back_to_the_us_layout() {
        let mut state = WindowState::default();
        let event = press(&mut state, 26, None, None);
        assert_eq!(event.logical_key, character("["));
        assert_eq!(event.text.as_deref(), Some("["));
    }

    #[test]
    fn jis_and_numpad_keys_are_known() {
        let mut state = WindowState::default();
        let yen = press(&mut state, 124, Some("¥"), Some("¥"));
        assert_eq!(yen.physical_key, PhysicalKey::Code(KeyCode::IntlYen));
        assert_eq!(yen.logical_key, character("¥"));
        let seven = press(&mut state, 71, Some("7"), Some("7"));
        assert_eq!(seven.location, KeyLocation::Numpad);
        assert_eq!(seven.logical_key, character("7"));
        // NumLock を切ったテンキーには記号が付かない（ブラウザーの key が Home になる）
        let home = press(&mut state, 71, None, None);
        assert_eq!(home.logical_key, Key::Named(NamedKey::Home));
        let enter = press(&mut state, 96, None, None);
        assert_eq!(enter.logical_key, Key::Named(NamedKey::Enter));
    }

    /// キーを 1 つ流して、届いた出来事をすべて返す
    fn press_all(state: &mut WindowState, code: u16, text: Option<&str>, symbol: Option<&str>) -> Vec<event::WindowEvent> {
        let mut events = Vec::new();
        EventLoop::process_key(
            state,
            KeyState::Down,
            code,
            text.map(str::to_owned),
            symbol.map(str::to_owned),
            &mut |event| events.push(event),
        );
        events
    }

    /// KeyboardInput の直前に知らせた修飾
    fn modifiers_at_key(events: &[event::WindowEvent]) -> Option<ModifiersState> {
        let index = events.iter().position(|event| matches!(event, event::WindowEvent::KeyboardInput { .. }))?;
        events[..index].iter().rev().find_map(|event| match event {
            event::WindowEvent::ModifiersChanged(modifiers) => Some(modifiers.state()),
            _ => None,
        })
    }

    #[test]
    fn mac_option_yen_types_a_backslash_without_alt() {
        let mut state = WindowState::default();
        press(&mut state, 56, None, None);
        let events = press_all(&mut state, 124, Some("\\"), Some("\\"));
        assert_eq!(modifiers_at_key(&events), Some(ModifiersState::empty()));
        let key = events.iter().find_map(|event| match event {
            event::WindowEvent::KeyboardInput { event, .. } => Some(event.clone()),
            _ => None,
        });
        let key = key.unwrap();
        assert_eq!(key.logical_key, character("\\"));
        assert_eq!(key.text.as_deref(), Some("\\"));
        // 打ち終えたら Alt を押したままに戻す
        assert!(matches!(events.last(), Some(event::WindowEvent::ModifiersChanged(m)) if m.state() == ModifiersState::ALT));
    }

    #[test]
    fn windows_altgr_types_the_character_without_control_and_alt() {
        let mut state = WindowState::default();
        press(&mut state, 29, None, None);
        press(&mut state, 100, None, None);
        let events = press_all(&mut state, 16, Some("@"), Some("@"));
        assert_eq!(modifiers_at_key(&events), Some(ModifiersState::empty()));
        let key = events.iter().find_map(|event| match event {
            event::WindowEvent::KeyboardInput { event, .. } => Some(event.clone()),
            _ => None,
        });
        assert_eq!(key.unwrap().platform_specific.text_with_all_modifiers.as_deref(), Some("@"));
    }

    #[test]
    fn alt_without_text_stays_a_shortcut() {
        let mut state = WindowState::default();
        press(&mut state, 56, None, None);
        // Option+←（文字を添えられない）と Linux の Alt+A（記号だけ）は Alt のまま
        let left = press_all(&mut state, 105, None, None);
        assert!(!left.iter().any(|event| matches!(event, event::WindowEvent::ModifiersChanged(_))));
        let a = press_all(&mut state, 30, None, Some("a"));
        assert!(!a.iter().any(|event| matches!(event, event::WindowEvent::ModifiersChanged(_))));
    }

    #[test]
    fn space_stays_a_named_key() {
        let mut state = WindowState::default();
        let event = press(&mut state, 57, Some(" "), Some(" "));
        assert_eq!(event.logical_key, Key::Named(NamedKey::Space));
        assert_eq!(event.text.as_deref(), Some(" "));
    }
}

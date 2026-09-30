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
}

impl WindowState {
    fn modifiers(&self) -> Modifiers {
        Modifiers { state: self.modifiers, pressed_mods: self.pressed }
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
        shared: &Shared,
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
                emit(app, event::WindowEvent::Focused(focused));
            },
            ServerMessage::Pointer { phase, x, y } => {
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
                            button: event::MouseButton::Left.into(),
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
            ServerMessage::Key { state: key_state, code } => {
                Self::process_key(state, key_state, code, &mut |event| emit(app, event));
            },
            ServerMessage::Text { text } => {
                emit(app, event::WindowEvent::Ime(Ime::Preedit(String::new(), None)));
                emit(app, event::WindowEvent::Ime(Ime::Commit(text)));
            },
            ServerMessage::Preedit { text } => {
                let cursor = if text.is_empty() { None } else { Some((text.len(), text.len())) };
                emit(app, event::WindowEvent::Ime(Ime::Preedit(text, cursor)));
            },
            ServerMessage::Close => emit(app, event::WindowEvent::CloseRequested),
        }
    }

    fn process_key(
        state: &mut WindowState,
        key_state: KeyState,
        code: u16,
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
        let (logical_key, key_without_modifiers, text, text_with_all_modifiers) = match &info {
            Some(keymap::KeyInfo { named: Some(named), chars: None, .. }) => {
                let text = match named {
                    crate::keyboard::NamedKey::Enter => Some(SmolStr::new("\r")),
                    crate::keyboard::NamedKey::Tab => Some(SmolStr::new("\t")),
                    _ => None,
                };
                let key = Key::Named(*named);
                (key.clone(), key, text.clone().filter(|_| pressed), text.filter(|_| pressed))
            },
            Some(keymap::KeyInfo { named, chars: Some((lower, upper)), .. }) => {
                let character = if shift { *upper } else { *lower };
                let logical = match named {
                    Some(named) => Key::Named(*named),
                    None => Key::Character(SmolStr::from(character.to_string())),
                };
                let without = match named {
                    Some(named) => Key::Named(*named),
                    None => Key::Character(SmolStr::from(lower.to_string())),
                };
                let text = pressed.then(|| SmolStr::from(character.to_string()));
                // Ctrl を押していれば制御文字になる（Ctrl+A → 0x01。実物の端末と同じ）
                let all = if control && character.is_ascii_alphabetic() {
                    let control_char =
                        ((character.to_ascii_lowercase() as u8 - b'a') + 1) as char;
                    pressed.then(|| SmolStr::from(control_char.to_string()))
                } else {
                    text.clone()
                };
                (logical, without, text, all)
            },
            _ => {
                let key = Key::Unidentified(NativeKey::Xkb(code as u32 + 8));
                (key.clone(), key, None, None)
            },
        };
        let location = match info.as_ref().map(|info| info.code) {
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
        if modifiers_changed {
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

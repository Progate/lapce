use parking_lot::Mutex;
use raw_window_handle::RawDisplayHandle;

#[cfg(not(target_os = "wasi"))]
use copypasta::ClipboardContext;
use copypasta::ClipboardProvider;

static CLIPBOARD: Mutex<Option<Clipboard>> = Mutex::new(None);

pub struct Clipboard {
    clipboard: Box<dyn ClipboardProvider>,
    #[allow(dead_code)]
    selection: Option<Box<dyn ClipboardProvider>>,
}

#[derive(Clone, Debug)]
pub enum ClipboardError {
    NotAvailable,
    ProviderError(String),
}

impl Clipboard {
    pub fn get_contents() -> Result<String, ClipboardError> {
        CLIPBOARD
            .lock()
            .as_mut()
            .ok_or(ClipboardError::NotAvailable)?
            .clipboard
            .get_contents()
            .map_err(|e| ClipboardError::ProviderError(e.to_string()))
    }

    pub fn set_contents(s: String) -> Result<(), ClipboardError> {
        if s.is_empty() {
            return Err(ClipboardError::ProviderError(
                "content is empty".to_string(),
            ));
        }
        CLIPBOARD
            .lock()
            .as_mut()
            .ok_or(ClipboardError::NotAvailable)?
            .clipboard
            .set_contents(s)
            .map_err(|e| ClipboardError::ProviderError(e.to_string()))
    }

    #[cfg(windows)]
    pub fn get_file_list() -> Result<Vec<std::path::PathBuf>, ClipboardError> {
        clipboard_win::Clipboard::new_attempts(10)
            .and_then(|x| x.get_file_list())
            .map_err(|e| ClipboardError::ProviderError(e.to_string()))
    }

    #[cfg(not(target_os = "wasi"))]
    pub(crate) unsafe fn init(display: RawDisplayHandle) {
        *CLIPBOARD.lock() = Some(Self::new(display));
    }

    /// WASI では windowserver の貼り付け板を使う（→ winit の `platform::wasi::clipboard`）。
    /// 中身は焦点のある窓へ届いたものを winit が控えているので、同期で読める
    #[cfg(target_os = "wasi")]
    pub(crate) fn init_in_process() {
        *CLIPBOARD.lock() = Some(Self {
            clipboard: Box::new(WindowServerClipboard),
            selection: None,
        });
    }

    /// # Safety
    /// The `display` must be valid as long as the returned Clipboard exists.
    #[cfg(not(target_os = "wasi"))]
    unsafe fn new(
        #[allow(unused_variables)] /* on some platforms */ display: RawDisplayHandle,
    ) -> Self {
        #[cfg(not(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "ios",
            target_os = "android",
            target_os = "wasi",
            all(target_arch = "wasm32", target_os = "unknown")
        )))]
        {
            if let RawDisplayHandle::Wayland(display) = display {
                use copypasta::wayland_clipboard;
                let (selection, clipboard) =
                    wayland_clipboard::create_clipboards_from_external(display.display.as_ptr());
                return Self {
                    clipboard: Box::new(clipboard),
                    selection: Some(Box::new(selection)),
                };
            }

            use copypasta::x11_clipboard::{Primary, X11ClipboardContext};
            Self {
                clipboard: Box::new(ClipboardContext::new().unwrap()),
                selection: Some(Box::new(X11ClipboardContext::<Primary>::new().unwrap())),
            }
        }

        // TODO: Implement clipboard support for the web, ios, and android
        #[cfg(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "ios",
            target_os = "android",
            all(target_arch = "wasm32", target_os = "unknown")
        ))]
        return Self {
            clipboard: Box::new(ClipboardContext::new().unwrap()),
            selection: None,
        };
    }
}

#[cfg(target_os = "wasi")]
struct WindowServerClipboard;

#[cfg(target_os = "wasi")]
impl ClipboardProvider for WindowServerClipboard {
    fn get_contents(&mut self) -> Result<String, Box<dyn std::error::Error + Send + Sync + 'static>> {
        Ok(winit::platform::wasi::clipboard::contents())
    }

    fn set_contents(
        &mut self,
        contents: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
        winit::platform::wasi::clipboard::set_contents(&contents);
        Ok(())
    }
}

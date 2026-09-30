//! ファイルの見張り（WASI・BrowserOS）。**Linux と同じ inotify を使う。**
//!
//! notify は Linux では inotify を使うが、WASI では周期的に全体を見に行く PollWatcher に
//! なり（既定 30 秒）、保存やコミットが画面に出るまで待たされる。BrowserOS は inotify
//! （`inotify_init1` / `inotify_add_watch` / `inotify_rm_watch`）を `browser_os_inotify` の
//! ホスト関数として持っているので、それを直に呼ぶ。
//!
//! 出来事の組み立ては notify の inotify 実装（`notify/src/inotify.rs`）を写してある。
//! 見張りは再帰ではないので、ディレクトリができたらそこにも見張りを足し、名前の変更は
//! `IN_MOVED_FROM` と `IN_MOVED_TO` を cookie で組にする。
//!
//! ## 止まらずに待つ
//!
//! 読む fd は `IN_NONBLOCK` にして、無ければ futex で眠る（→ wasi_process.rs と同じ理由）。

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read},
    os::fd::FromRawFd,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use notify::{
    Config, Error, Event, EventHandler, EventKind, RecursiveMode, Result,
    Watcher, WatcherKind,
    event::{
        AccessKind, AccessMode, CreateKind, DataChange, Flag, MetadataKind, ModifyKind,
        RemoveKind, RenameMode,
    },
};
use parking_lot::Mutex;
use walkdir::WalkDir;

#[link(wasm_import_module = "browser_os_inotify")]
unsafe extern "C" {
    #[link_name = "inotify_init1"]
    fn host_inotify_init1(flags: i32, fd: *mut u32) -> i32;
    #[link_name = "inotify_add_watch"]
    fn host_inotify_add_watch(
        fd: u32,
        path: *const u8,
        path_len: usize,
        mask: u32,
        wd: *mut i32,
    ) -> i32;
    #[link_name = "inotify_rm_watch"]
    fn host_inotify_rm_watch(fd: u32, wd: i32) -> i32;
}

/* `linux/inotify.h` */
const IN_MODIFY: u32 = 0x0000_0002;
const IN_ATTRIB: u32 = 0x0000_0004;
const IN_CLOSE_WRITE: u32 = 0x0000_0008;
const IN_MOVED_FROM: u32 = 0x0000_0040;
const IN_MOVED_TO: u32 = 0x0000_0080;
const IN_CREATE: u32 = 0x0000_0100;
const IN_DELETE: u32 = 0x0000_0200;
const IN_DELETE_SELF: u32 = 0x0000_0400;
const IN_MOVE_SELF: u32 = 0x0000_0800;
const IN_Q_OVERFLOW: u32 = 0x0000_4000;
const IN_IGNORED: u32 = 0x0000_8000;
const IN_ISDIR: u32 = 0x4000_0000;
/// `O_NONBLOCK`
const IN_NONBLOCK: i32 = 0o4000;

/// 何も来ていないとき、次に読みに行くまでの間隔
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// syscall を出さずに眠る
fn futex_sleep(duration: Duration) {
    let (_sender, receiver) = std::sync::mpsc::channel::<()>();
    let _ = receiver.recv_timeout(duration);
}

fn os_error(errno: i32, path: &Path) -> Error {
    Error::io(io::Error::other(format!("inotify failed (errno {errno})")))
        .add_path(path.to_path_buf())
}

struct RawEvent {
    wd: i32,
    mask: u32,
    cookie: u32,
    name: Option<String>,
}

/// `struct inotify_event` の並びをほどく（名前は NUL で詰めてある）
fn parse_events(bytes: &[u8]) -> Vec<RawEvent> {
    let mut events = Vec::new();
    let mut offset = 0;
    while offset + 16 <= bytes.len() {
        let field = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        let wd = field(offset) as i32;
        let mask = field(offset + 4);
        let cookie = field(offset + 8);
        let len = field(offset + 12) as usize;
        let start = offset + 16;
        let end = (start + len).min(bytes.len());
        let name = (len > 0).then(|| {
            let raw = &bytes[start..end];
            let raw = &raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())];
            String::from_utf8_lossy(raw).into_owned()
        });
        events.push(RawEvent { wd, mask, cookie, name });
        offset = start + len;
    }
    events
}

struct State {
    fd: u32,
    file: File,
    event_handler: Box<dyn EventHandler>,
    /// パス → (wd, 見張る中身, 再帰か)
    watches: HashMap<PathBuf, (i32, u32, bool)>,
    paths: HashMap<i32, PathBuf>,
    rename_event: Option<Event>,
}

impl State {
    fn add_watch(&mut self, path: PathBuf, is_recursive: bool, mut watch_self: bool) -> Result<()> {
        if !is_recursive || !std::fs::metadata(&path).map_err(Error::io)?.is_dir() {
            return self.add_single_watch(path, false, true);
        }
        for entry in WalkDir::new(path)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_dir())
        {
            self.add_single_watch(entry.path().to_path_buf(), is_recursive, watch_self)?;
            watch_self = false;
        }
        Ok(())
    }

    fn add_single_watch(&mut self, path: PathBuf, is_recursive: bool, watch_self: bool) -> Result<()> {
        let mut mask =
            IN_ATTRIB | IN_CREATE | IN_DELETE | IN_CLOSE_WRITE | IN_MODIFY | IN_MOVED_FROM | IN_MOVED_TO;
        if watch_self {
            mask |= IN_DELETE_SELF | IN_MOVE_SELF;
        }
        if let Some(&(_, old, _)) = self.watches.get(&path) {
            mask |= old;
        }
        let bytes = path.to_string_lossy();
        let mut wd = 0i32;
        let errno =
            unsafe { host_inotify_add_watch(self.fd, bytes.as_ptr(), bytes.len(), mask, &mut wd) };
        if errno != 0 {
            return Err(os_error(errno, &path));
        }
        self.watches.insert(path.clone(), (wd, mask, is_recursive));
        self.paths.insert(wd, path);
        Ok(())
    }

    fn remove_watch(&mut self, path: PathBuf, remove_recursive: bool) -> Result<()> {
        let Some((wd, _, is_recursive)) = self.watches.remove(&path) else {
            return Err(Error::watch_not_found().add_path(path));
        };
        unsafe { host_inotify_rm_watch(self.fd, wd) };
        self.paths.remove(&wd);
        if is_recursive || remove_recursive {
            let children: Vec<PathBuf> =
                self.watches.keys().filter(|p| p.starts_with(&path)).cloned().collect();
            for child in children {
                if let Some((wd, _, _)) = self.watches.remove(&child) {
                    unsafe { host_inotify_rm_watch(self.fd, wd) };
                    self.paths.remove(&wd);
                }
            }
        }
        Ok(())
    }

    /// 親が再帰で見張られていて、できたのがディレクトリなら、そこにも見張りを足す
    fn is_under_recursive(&self, path: &Path) -> bool {
        path.parent()
            .and_then(|parent| self.watches.get(parent))
            .is_some_and(|(_, _, recursive)| *recursive)
    }

    fn send(&mut self, event: Event) {
        self.event_handler.handle_event(Ok(event));
    }

    fn flush_rename(&mut self) {
        if let Some(event) = self.rename_event.take() {
            self.send(event);
        }
    }

    /// 読めるだけ読んで配る。何か読めたら true
    fn pump(&mut self) -> bool {
        let mut buffer = vec![0u8; 16 * 1024];
        let count = match self.file.read(&mut buffer) {
            Ok(count) => count,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                // 組になる MOVED_TO が来ないまま次の読みが空なら、外へ出ていった
                self.flush_rename();
                return false;
            }
            Err(err) => {
                self.event_handler.handle_event(Err(Error::io(err)));
                return false;
            }
        };
        let mut add = Vec::new();
        let mut remove = Vec::new();
        for event in parse_events(&buffer[..count]) {
            if event.mask & IN_Q_OVERFLOW != 0 {
                self.send(Event::new(EventKind::Other).set_flag(Flag::Rescan));
                continue;
            }
            if event.mask & IN_IGNORED != 0 {
                if let Some(path) = self.paths.remove(&event.wd) {
                    self.watches.remove(&path);
                }
                continue;
            }
            let Some(root) = self.paths.get(&event.wd).cloned() else {
                continue;
            };
            let path = match &event.name {
                Some(name) => root.join(name),
                None => root,
            };
            let is_dir = event.mask & IN_ISDIR != 0;
            let created = || EventKind::Create(if is_dir { CreateKind::Folder } else { CreateKind::File });

            if event.mask & IN_MOVED_FROM != 0 {
                self.flush_rename();
                if self.watches.contains_key(&path) {
                    remove.push(path.clone());
                }
                self.rename_event = Some(
                    Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                        .add_path(path)
                        .set_tracker(event.cookie as usize),
                );
                continue;
            }
            if event.mask & IN_MOVED_TO != 0 {
                match self.rename_event.take() {
                    Some(from) if from.tracker() == Some(event.cookie as usize) => {
                        let old = from.paths.first().cloned();
                        self.send(from);
                        self.send(
                            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
                                .set_tracker(event.cookie as usize)
                                .add_path(path.clone()),
                        );
                        let mut both = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                            .set_tracker(event.cookie as usize);
                        if let Some(old) = old {
                            both = both.add_path(old);
                        }
                        self.send(both.add_path(path.clone()));
                    }
                    other => {
                        if let Some(other) = other {
                            self.send(other);
                        }
                        self.send(Event::new(created()).add_path(path.clone()));
                    }
                }
                if is_dir && self.is_under_recursive(&path) {
                    add.push(path.clone());
                }
            }
            if event.mask & IN_MOVE_SELF != 0 {
                self.send(
                    Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                        .add_path(path.clone()),
                );
            }
            if event.mask & IN_CREATE != 0 {
                self.send(Event::new(created()).add_path(path.clone()));
                if is_dir && self.is_under_recursive(&path) {
                    add.push(path.clone());
                }
            }
            if event.mask & (IN_DELETE | IN_DELETE_SELF) != 0 {
                self.send(
                    Event::new(EventKind::Remove(if is_dir { RemoveKind::Folder } else { RemoveKind::File }))
                        .add_path(path.clone()),
                );
                if self.watches.contains_key(&path) {
                    remove.push(path.clone());
                }
            }
            if event.mask & IN_MODIFY != 0 {
                self.send(
                    Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
                        .add_path(path.clone()),
                );
            }
            if event.mask & IN_CLOSE_WRITE != 0 {
                self.send(
                    Event::new(EventKind::Access(AccessKind::Close(AccessMode::Write)))
                        .add_path(path.clone()),
                );
            }
            if event.mask & IN_ATTRIB != 0 {
                self.send(
                    Event::new(EventKind::Modify(ModifyKind::Metadata(MetadataKind::Any)))
                        .add_path(path.clone()),
                );
            }
        }
        for path in remove {
            let _ = self.remove_watch(path, true);
        }
        for path in add {
            let _ = self.add_watch(path, true, false);
        }
        true
    }
}

/// notify の `Watcher` として振る舞う inotify の見張り役
pub struct InotifyWatcher {
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

impl Watcher for InotifyWatcher {
    fn new<F: EventHandler>(event_handler: F, _config: Config) -> Result<Self> {
        let mut fd = 0u32;
        let errno = unsafe { host_inotify_init1(IN_NONBLOCK, &mut fd) };
        if errno != 0 {
            return Err(Error::io(io::Error::other(format!(
                "inotify_init1 failed (errno {errno})"
            ))));
        }
        let state = Arc::new(Mutex::new(State {
            fd,
            file: unsafe { File::from_raw_fd(fd as i32) },
            event_handler: Box::new(event_handler),
            watches: HashMap::new(),
            paths: HashMap::new(),
            rename_event: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        {
            let state = state.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("inotify".to_owned())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        // 読めたらすぐ次を見る。空なら眠る
                        if !state.lock().pump() {
                            futex_sleep(POLL_INTERVAL);
                        }
                    }
                })
                .map_err(Error::io)?;
        }
        Ok(Self { state, stop })
    }

    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> Result<()> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_err(Error::io)?.join(path)
        };
        self.state.lock().add_watch(path, recursive_mode == RecursiveMode::Recursive, true)
    }

    fn unwatch(&mut self, path: &Path) -> Result<()> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().map_err(Error::io)?.join(path)
        };
        self.state.lock().remove_watch(path, false)
    }

    fn kind() -> WatcherKind {
        WatcherKind::Inotify
    }
}

impl Drop for InotifyWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

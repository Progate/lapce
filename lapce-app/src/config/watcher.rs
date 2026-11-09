use std::sync::{Arc, atomic::AtomicBool, mpsc::Sender};

pub struct ConfigWatcher {
    tx: Sender<()>,
    delay_handler: Arc<AtomicBool>,
}

impl notify::EventHandler for ConfigWatcher {
    fn handle_event(&mut self, event: notify::Result<notify::Event>) {
        match event {
            Ok(event) => match event.kind {
                notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_) => {
                    if self
                        .delay_handler
                        .compare_exchange(
                            false,
                            true,
                            std::sync::atomic::Ordering::Relaxed,
                            std::sync::atomic::Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        let config_mutex = self.delay_handler.clone();
                        let tx = self.tx.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(
                                500,
                            ));
                            if let Err(err) = tx.send(()) {
                                tracing::error!("{:?}", err);
                            }
                            config_mutex
                                .store(false, std::sync::atomic::Ordering::Relaxed);
                        });
                    }
                }
                _ => {}
            },
            Err(err) => {
                tracing::error!("{:?}", err);
            }
        }
    }
}

impl ConfigWatcher {
    pub fn new(tx: Sender<()>) -> Self {
        Self {
            tx,
            delay_handler: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// 設定の置き場のファイルを書いたら読み直しを頼む口（BrowserOS 版だけ使う。→ `config_file_written`）
#[cfg(target_vendor = "browseros")]
static WRITTEN: std::sync::OnceLock<std::sync::Mutex<Sender<()>>> =
    std::sync::OnceLock::new();

/// BrowserOS 版は設定の置き場を見張らない（見張りは notify のスレッドを 1 本ずっと起こしておく。BrowserOS では
/// スレッド 1 本がワーカー 1 つになり、数 MB を食う）。設定の置き場を書き換えるのは Lapce 自身（設定の画面・
/// 割り当ての画面・拡大縮小）だけで、ホストは起動の前に書き終えているので、Lapce が書いたときに読み直せば足りる
#[cfg(target_vendor = "browseros")]
pub fn notify_on_write(tx: Sender<()>) {
    let _ = WRITTEN.set(std::sync::Mutex::new(tx));
}

/// 設定の置き場のファイルを書いたことを知らせる。BrowserOS 版は見張りが無いので、ここで読み直しを頼む
/// （ほかは見張りが拾うので何もしない）
pub fn config_file_written() {
    #[cfg(target_vendor = "browseros")]
    if let Some(tx) = WRITTEN.get() {
        if let Ok(tx) = tx.lock() {
            if let Err(err) = tx.send(()) {
                tracing::error!("{:?}", err);
            }
        }
    }
}

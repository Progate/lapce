//! 演習の画面に埋め込んで開くときに、Lapce の窓から外すもの（BrowserOS 版だけが受け付ける起動の引数）。
//!
//! 本物の Lapce には無い。演習ではタイルの一枠に Lapce を置き、学習者が見るのはファイルの一覧とエディタだけでよいので、
//! 窓の上の帯（コマンド パレットと行き来の矢印）・開いているエディターの一覧・ソース管理の板を出さない形を選べるようにする。
//! 決めるのは起動の引数で、板の並び（`PanelKind::is_available`）を組むより先に読まれるので、`read_only` と同じく
//! 窓を開く前に 1 度だけ決める

use std::sync::atomic::{AtomicBool, Ordering};

static NO_TITLE_BAR: AtomicBool = AtomicBool::new(false);
static NO_OPEN_EDITORS: AtomicBool = AtomicBool::new(false);
static NO_SOURCE_CONTROL: AtomicBool = AtomicBool::new(false);

/// 埋め込むときに外すもの
#[derive(Debug, Clone, Copy, Default)]
pub struct Embed {
    /// 窓の上の帯（`--no-title-bar`）
    pub no_title_bar: bool,
    /// ファイルの一覧の上の「開いているエディター」（`--no-open-editors`）
    pub no_open_editors: bool,
    /// ソース管理の板（`--no-source-control`）
    pub no_source_control: bool,
}

/// 外すものを決める。起動の引数を読んだところで、窓を開く前に 1 度だけ呼ぶ
pub fn set(embed: Embed) {
    NO_TITLE_BAR.store(embed.no_title_bar, Ordering::Relaxed);
    NO_OPEN_EDITORS.store(embed.no_open_editors, Ordering::Relaxed);
    NO_SOURCE_CONTROL.store(embed.no_source_control, Ordering::Relaxed);
}

/// 窓の上の帯を出さないか
pub fn hides_title_bar() -> bool {
    NO_TITLE_BAR.load(Ordering::Relaxed)
}

/// 開いているエディターの一覧を出さないか
pub fn hides_open_editors() -> bool {
    NO_OPEN_EDITORS.load(Ordering::Relaxed)
}

/// ソース管理の板を出さないか
pub fn hides_source_control() -> bool {
    NO_SOURCE_CONTROL.load(Ordering::Relaxed)
}

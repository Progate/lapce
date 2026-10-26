//! ワークスペース全体を読み取り専用で開くか（`lapce --read-only`。BrowserOS 版だけが受け付ける）。
//!
//! 作品や模範解答のコードを見せるための形で、本物の Lapce には無い。決めるのは起動の引数で、
//! 画面（lapce-app）と proxy の両方が読むので、どちらも使うこの crate に置く。
//!
//! 読み取り専用のときは、
//!
//! - 開く文書をすべて読み取り専用にする（打っても変わらない）
//! - proxy がファイルを書き換える頼み（保存・作成・名前の変更・削除）と、端末の起動と git を断る
//! - 端末とソース管理の板を出さない（→ lapce-app の `PanelKind::is_available`）
//!
//! 書き換える口を proxy で断るので、画面の側で取りこぼした口があってもファイルは変わらない。
//! 端末と git の子プロセスを起こさないぶん、起動も軽くなる

use std::sync::atomic::{AtomicBool, Ordering};

static READ_ONLY: AtomicBool = AtomicBool::new(false);

/// 読み取り専用にする。起動の引数を読んだところで、窓を開く前に 1 度だけ呼ぶ
pub fn enable() {
    READ_ONLY.store(true, Ordering::Relaxed);
}

/// 読み取り専用で開いているか
pub fn is_enabled() -> bool {
    READ_ONLY.load(Ordering::Relaxed)
}

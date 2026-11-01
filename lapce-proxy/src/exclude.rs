//! 演習の画面に埋め込んだ Lapce で、学習者に見せないもの（`--exclude-file`。BrowserOS 版だけが受け付ける起動の引数）。
//!
//! 演習の作業場所には、学習者が書くファイルのほかに、依存（`node_modules`）やビルドの成果物、演習の仕掛けのファイルが並ぶ。
//! ページの側のエディタはそれらを隠しているので、同じものを Lapce でも隠す。外すものは gitignore と同じ書き方のファイルで受け取り
//! （ディレクトリだけに当てる `name/`・否定の `!`）、ファイルの一覧・ファイルの検索・全体の検索のどれにも当てる。
//!
//! **場所を含む行（`/progate/main` など）はファイルシステムの根から当てる。** ファイルが根に置かれているものとして読む。
//! 開いたフォルダから当てると、ファイルの一覧を最初に読む時点では proxy がまだ開いたフォルダを知らず、何も外せない。
//! ファイルは開いている途中で書き換わる（演習の `.ignore` が変わったとき）ので、使うたびに読み直す

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// 見せないものの規則
pub struct Exclude(Gitignore);

impl Exclude {
    /// 見せないものを読む。指定が無い・読めないときは None（何も外さない）
    pub fn load() -> Option<Self> {
        let file = lapce_core::embed::exclude_file()?;
        let mut builder = GitignoreBuilder::new("/");
        // 読めない行があっても、読めた行は当てる（1 行の誤りで全部を見せてしまわないように）
        if let Some(error) = builder.add(file) {
            tracing::warn!("{file:?}: {error}");
        }
        match builder.build() {
            Ok(gitignore) => Some(Self(gitignore)),
            Err(error) => {
                tracing::error!("{file:?}: {error}");
                None
            }
        }
    }

    /// 見せないものか
    pub fn is_excluded(&self, path: &Path, is_dir: bool) -> bool {
        // 相対のパスと根そのものには当てない。`matched_path_or_any_parents` は根の外のパスで panic する
        path.is_absolute()
            && path.parent().is_some()
            && self.0.matched_path_or_any_parents(path, is_dir).is_ignore()
    }
}

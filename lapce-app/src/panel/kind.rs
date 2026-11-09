use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;

use super::{data::PanelOrder, position::PanelPosition};
use crate::config::icon::LapceIcons;

#[derive(
    Clone, Copy, PartialEq, Serialize, Deserialize, Hash, Eq, Debug, EnumIter,
)]
pub enum PanelKind {
    Terminal,
    FileExplorer,
    SourceControl,
    Plugin,
    Search,
    Problem,
    Debug,
    CallHierarchy,
    DocumentSymbol,
    References,
    Implementation,
}

impl PanelKind {
    pub fn svg_name(&self) -> &'static str {
        match &self {
            PanelKind::Terminal => LapceIcons::TERMINAL,
            PanelKind::FileExplorer => LapceIcons::FILE_EXPLORER,
            PanelKind::SourceControl => LapceIcons::SCM,
            PanelKind::Plugin => LapceIcons::EXTENSIONS,
            PanelKind::Search => LapceIcons::SEARCH,
            PanelKind::Problem => LapceIcons::PROBLEM,
            PanelKind::Debug => LapceIcons::DEBUG,
            PanelKind::CallHierarchy => LapceIcons::TYPE_HIERARCHY,
            PanelKind::DocumentSymbol => LapceIcons::DOCUMENT_SYMBOL,
            PanelKind::References => LapceIcons::REFERENCES,
            PanelKind::Implementation => LapceIcons::IMPLEMENTATION,
        }
    }

    pub fn position(&self, order: &PanelOrder) -> Option<(usize, PanelPosition)> {
        for (pos, panels) in order.iter() {
            let index = panels.iter().position(|k| k == self);
            if let Some(index) = index {
                return Some((index, *pos));
            }
        }
        None
    }

    /// この環境で働く板か。働かない板は並びから外し、絵の並び（activity bar）にも出さない
    /// （→ `retain_available_panels`）。
    ///
    /// BrowserOS では wasm の拡張（wasmtime）もデバッガー（DAP）も起こせないので、それらに頼る板は何も映らない。
    /// 消すと設定や保存した並びとの行き来が崩れるので、種類は残して並びからだけ外す。LSP に頼る板（Problems・
    /// シンボル・参照など）は、OS が言語サーバーを配っていれば働くので残す（→ lapce-proxy の plugin/language_server.rs）。
    /// 読み取り専用で開いたときは、端末とソース管理も外す
    pub fn is_available(&self) -> bool {
        if !cfg!(target_vendor = "browseros") {
            return true;
        }
        match self {
            // 読み取り専用で開いたときは、書き換えられる口（端末・コミット）を出さない（→ lapce_core::read_only）
            PanelKind::Terminal => !lapce_core::read_only::is_enabled(),
            // 演習の画面に埋め込むときは、ファイルの一覧だけを並べるために外せる（→ lapce_core::embed）
            PanelKind::SourceControl => {
                !lapce_core::read_only::is_enabled()
                    && !lapce_core::embed::hides_source_control()
            }
            PanelKind::FileExplorer
            | PanelKind::Search
            | PanelKind::Problem
            | PanelKind::CallHierarchy
            | PanelKind::DocumentSymbol
            | PanelKind::References
            | PanelKind::Implementation => true,
            PanelKind::Plugin | PanelKind::Debug => false,
        }
    }

    /// 板の切り替えの並び（絵の並び）に絵を出すか。
    ///
    /// BrowserOS 版は演習の画面に埋め込むので、下の板の並びには学習者が自分で開く端末と Problems だけを置く。
    /// 検索・呼び出し階層・参照・実装は、コマンド（検索、参照へ移動など）が結果を映す板として並びに残し、絵だけを出さない。
    /// 絵を出し直すときは、下のコメントアウトした行を戻す
    pub fn is_pickable(&self) -> bool {
        if !cfg!(target_vendor = "browseros") {
            return true;
        }
        match self {
            PanelKind::Terminal
            | PanelKind::Problem
            | PanelKind::FileExplorer
            | PanelKind::SourceControl
            | PanelKind::Plugin
            | PanelKind::Debug
            | PanelKind::DocumentSymbol => true,
            // PanelKind::Search => true,
            // PanelKind::CallHierarchy => true,
            // PanelKind::References => true,
            // PanelKind::Implementation => true,
            _ => false,
        }
    }

    pub fn default_position(&self) -> PanelPosition {
        match self {
            PanelKind::Terminal => PanelPosition::BottomLeft,
            PanelKind::FileExplorer => PanelPosition::LeftTop,
            PanelKind::SourceControl => PanelPosition::LeftTop,
            PanelKind::Plugin => PanelPosition::LeftTop,
            PanelKind::Search => PanelPosition::BottomLeft,
            PanelKind::Problem => PanelPosition::BottomLeft,
            PanelKind::Debug => PanelPosition::LeftTop,
            PanelKind::CallHierarchy => PanelPosition::BottomLeft,
            PanelKind::DocumentSymbol => PanelPosition::RightTop,
            PanelKind::References => PanelPosition::BottomLeft,
            PanelKind::Implementation => PanelPosition::BottomLeft,
        }
    }
}

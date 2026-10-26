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
    /// WASI（BrowserOS）では拡張（wasmtime）も LSP もデバッガー（DAP）も起こせないので、
    /// それらに頼る板は何も映らない。消すと設定や保存した並びとの行き来が崩れるので、種類は残して並びからだけ外す。
    /// 読み取り専用で開いたときは、端末とソース管理も外す
    pub fn is_available(&self) -> bool {
        if !cfg!(target_os = "wasi") {
            return true;
        }
        match self {
            // 読み取り専用で開いたときは、書き換えられる口（端末・コミット）を出さない（→ lapce_core::read_only）
            PanelKind::Terminal | PanelKind::SourceControl => {
                !lapce_core::read_only::is_enabled()
            }
            PanelKind::FileExplorer | PanelKind::Search => true,
            PanelKind::Plugin
            | PanelKind::Problem
            | PanelKind::Debug
            | PanelKind::CallHierarchy
            | PanelKind::DocumentSymbol
            | PanelKind::References
            | PanelKind::Implementation => false,
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

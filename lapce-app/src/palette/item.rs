use std::path::PathBuf;

use lapce_core::line_ending::LineEnding;
use lapce_rpc::dap_types::RunDebugConfig;
use lsp_types::{Range, SymbolKind};
use strum::EnumMessage;

use crate::{
    command::{LapceCommand, LapceWorkbenchCommand},
    debug::RunDebugMode,
    editor::location::EditorLocation,
    workspace::{LapceWorkspace, SshHost},
};

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    pub content: PaletteItemContent,
    pub filter_text: String,
    pub score: u32,
    pub indices: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteItemContent {
    PaletteHelp {
        cmd: LapceWorkbenchCommand,
    },
    File {
        path: PathBuf,
        full_path: PathBuf,
    },
    Line {
        line: usize,
        content: String,
    },
    Command {
        cmd: LapceCommand,
    },
    Workspace {
        workspace: LapceWorkspace,
    },
    Reference {
        path: PathBuf,
        location: EditorLocation,
    },
    DocumentSymbol {
        kind: SymbolKind,
        name: String,
        range: Range,
        container_name: Option<String>,
    },
    WorkspaceSymbol {
        kind: SymbolKind,
        name: String,
        container_name: Option<String>,
        location: EditorLocation,
    },
    SshHost {
        host: SshHost,
    },
    #[cfg(windows)]
    WslHost {
        host: crate::workspace::WslHost,
    },
    RunAndDebug {
        mode: RunDebugMode,
        config: RunDebugConfig,
    },
    ColorTheme {
        name: String,
    },
    IconTheme {
        name: String,
    },
    Language {
        name: String,
    },
    LineEnding {
        kind: LineEnding,
    },
    SCMReference {
        name: String,
    },
    TerminalProfile {
        name: String,
        profile: lapce_rpc::terminal::TerminalProfile,
    },
}

impl PaletteItem {
    /// 表示している `filter_text` が訳文のとき、その英語の原文。
    /// 英語に慣れた人が英語のコマンド名を打っても見つかるよう、絞り込みで訳文と合わせて照らす。
    pub fn english_filter_text(&self) -> Option<&'static str> {
        let english = match &self.content {
            PaletteItemContent::Command { cmd } => cmd.kind.desc()?,
            PaletteItemContent::PaletteHelp { cmd } => cmd.get_message()?,
            _ => return None,
        };
        (!self.filter_text.ends_with(english)).then_some(english)
    }
}

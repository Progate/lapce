//! 言語サーバーを起こすだけのプラグイン（BrowserOS）。
//!
//! ネイティブ版の言語のプラグイン（lapce-typescript など）は wasm のモジュールで、wasmtime の中で動き、
//! `startLspServer` を頼んで言語サーバー（`typescript-language-server --stdio` など）を起こしているだけである。
//! wasm の中の Lapce は wasmtime を持ち込めないので、その頼みの中身をプラグインのディレクトリの
//! `language-server.toml` に書いておき、Lapce が同じ `LspClient` で言語サーバーを起こす。
//!
//! ```toml
//! # /usr/share/lapce/plugins/progate.typescript-language-server/language-server.toml
//! command = "typescript-language-server"   # `/` が無ければ PATH から引く
//! args = ["--stdio"]
//!
//! [initialization-options]                 # LSP の initialize の initializationOptions（省いてよい）
//! preferences = { includeInlayParameterNameHints = "all" }
//! ```
//!
//! 起こすのは volt.toml の `[activation]` の言語の文書を開いたとき（ネイティブ版のプラグインが起こされるときと同じ）で、
//! 言語サーバーが受け持つ文書もその言語に絞る。利用者の設定（settings.toml のプラグインの名前の節）の
//! `initializationOptions` があれば、`initialization-options` に重ねる

use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::{Context, Result, anyhow};
use lapce_rpc::plugin::VoltMetadata;
use lsp_types::{DocumentFilter, Url};
use serde::Deserialize;
use serde_json::Value;

use super::{PluginCatalogRpcHandler, lsp::LspClient};

/// プラグインのディレクトリの中の、言語サーバーの起こし方
pub const FILE_NAME: &str = "language-server.toml";

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LanguageServer {
    /// 起こすもの。`/` が無ければ PATH から引く（execvp(3) と同じ）
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// LSP の initialize の `initializationOptions`
    pub initialization_options: Option<Value>,
}

impl LanguageServer {
    /// プラグインが言語サーバーの起こし方を持っていれば読む
    pub fn of(meta: &VoltMetadata) -> Result<Option<Self>> {
        let Some(path) = meta.dir.as_ref().map(|dir| dir.join(FILE_NAME)) else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("can't read {}", path.display()))?;
        let server = toml::from_str(&text)
            .with_context(|| format!("can't parse {}", path.display()))?;
        Ok(Some(server))
    }

    /// `startLspServer` の `serverUri`。パスなら `file:`、名前なら `urn:`（どちらも `LspClient` が受け付ける形）
    fn server_uri(&self) -> Result<Url> {
        if self.command.contains('/') {
            Url::from_file_path(&self.command)
                .map_err(|_| anyhow!("{} is not an absolute path", self.command))
        } else {
            Ok(Url::parse(&format!("urn:{}", self.command))?)
        }
    }

    /// 利用者の設定の `initializationOptions` を、置かれた `initialization-options` に重ねる（表は中まで重ねる）
    fn options(
        &self,
        configurations: Option<&HashMap<String, Value>>,
    ) -> Option<Value> {
        let user = configurations.and_then(|c| c.get("initializationOptions"));
        match (self.initialization_options.clone(), user) {
            (base, None) => base,
            (None, Some(user)) => Some(user.clone()),
            (Some(mut base), Some(user)) => {
                merge(&mut base, user);
                Some(base)
            }
        }
    }

    /// 言語サーバーを起こす。受け持つ文書は volt.toml の `[activation]` の言語
    pub fn start(
        &self,
        workspace: Option<PathBuf>,
        configurations: Option<HashMap<String, Value>>,
        plugin_rpc: PluginCatalogRpcHandler,
        meta: &VoltMetadata,
    ) -> Result<()> {
        let document_selector = meta
            .activation
            .iter()
            .flat_map(|activation| activation.language.iter().flatten())
            .map(|language| DocumentFilter {
                language: Some(language.clone()),
                pattern: None,
                scheme: None,
            })
            .collect();
        LspClient::start(
            plugin_rpc,
            document_selector,
            workspace,
            meta.id(),
            meta.display_name.clone(),
            None,
            None,
            meta.dir.clone(),
            self.server_uri()?,
            self.args.clone(),
            self.options(configurations.as_ref()),
        )?;
        Ok(())
    }
}

fn merge(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_way_to_start_a_server() {
        let server: LanguageServer = toml::from_str(
            r#"
            command = "typescript-language-server"
            args = ["--stdio"]
            [initialization-options]
            preferences = { includeInlayParameterNameHints = "all" }
            "#,
        )
        .unwrap();
        assert_eq!(server.command, "typescript-language-server");
        assert_eq!(server.args, vec!["--stdio"]);
        assert_eq!(
            server.server_uri().unwrap().as_str(),
            "urn:typescript-language-server"
        );
    }

    #[test]
    fn a_path_is_a_file_uri() {
        let server: LanguageServer =
            toml::from_str(r#"command = "/usr/bin/pylsp""#).unwrap();
        assert_eq!(
            server.server_uri().unwrap().as_str(),
            "file:///usr/bin/pylsp"
        );
    }

    #[test]
    fn user_options_go_over_the_given_ones() {
        let server: LanguageServer = toml::from_str(
            r#"
            command = "x"
            [initialization-options]
            preferences = { a = 1, b = 2 }
            "#,
        )
        .unwrap();
        let user = HashMap::from([(
            "initializationOptions".to_string(),
            serde_json::json!({"preferences": {"b": 3}}),
        )]);
        assert_eq!(
            server.options(Some(&user)),
            Some(serde_json::json!({"preferences": {"a": 1, "b": 3}}))
        );
    }
}

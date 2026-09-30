#![allow(clippy::manual_clamp)]

pub mod directory;
pub mod encoding;
pub mod language;
pub mod lens;
pub mod meta;
pub mod rope_text_pos;
pub mod style;
pub mod syntax;
#[cfg(target_os = "wasi")]
pub mod xdg;

// 文法の crate から使うのは、C の記号（`tree_sitter_<name>`）と、同梱のハイライトの
// 定義（`HIGHLIGHTS_QUERY` など）だけである（→ language.rs）
#[cfg(target_os = "wasi")]
extern crate tree_sitter_bash;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_c;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_css;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_html;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_javascript;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_json;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_python;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_rust;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_toml_ng;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_typescript;
#[cfg(target_os = "wasi")]
extern crate tree_sitter_yaml;
// This is primarily being re-exported to avoid changing every single usage
// in lapce-app. We should probably remove this at some point.
pub use floem_editor_core::*;

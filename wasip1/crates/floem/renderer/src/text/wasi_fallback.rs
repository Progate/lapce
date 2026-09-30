//! WASI の代わりの書体の一覧。
//!
//! cosmic-text は Linux・macOS・Windows 以外で空の一覧を使う（`fallback/other.rs`）ので、
//! 書体名が当たらない文字は何も描けず、`no default font found` で落ちる。BrowserOS は
//! Linux と同じ形の OS なので、cosmic-text 0.14 の `fallback/unix.rs` をそのまま写して使う
//!（MIT OR Apache-2.0）

use cosmic_text::Fallback;
use unicode_script::Script;

/// Linux と同じ代わりの書体の一覧
#[derive(Debug)]
pub struct WasiFallback;

impl Fallback for WasiFallback {
    fn common_fallback(&self) -> &'static [&'static str] {
        common_fallback()
    }

    fn forbidden_fallback(&self) -> &'static [&'static str] {
        forbidden_fallback()
    }

    fn script_fallback(
        &self,
        script: unicode_script::Script,
        locale: &str,
    ) -> &'static [&'static str] {
        script_fallback(script, locale)
    }
}

// Fallbacks to use after any script specific fallbacks
fn common_fallback() -> &'static [&'static str] {
    //TODO: abstract style (sans/serif/monospaced)
    &[
        /* Sans-serif fallbacks */
        "Noto Sans",
        /* More sans-serif fallbacks */
        "DejaVu Sans",
        "FreeSans",
        /* Mono fallbacks */
        "Noto Sans Mono",
        "DejaVu Sans Mono",
        "FreeMono",
        /* Symbols fallbacks */
        "Noto Sans Symbols",
        "Noto Sans Symbols2",
        /* Emoji fallbacks*/
        "Noto Color Emoji",
        //TODO: Add CJK script here for doublewides?
    ]
}

// Fallbacks to never use
fn forbidden_fallback() -> &'static [&'static str] {
    &[]
}

fn han_unification(locale: &str) -> &'static [&'static str] {
    // unix.rs は locale を `ja` ちょうどでしか見ないが、LANG から作ると `ja-JP` になる。
    // 書体は Noto の地域別サブセット（書体名が `Noto Sans JP`）でもよいように並べる
    let language = locale.split(['-', '_']).next().unwrap_or(locale);
    match (language, locale) {
        // Japan
        ("ja", _) => &["Noto Sans CJK JP", "Noto Sans JP"],
        // Korea
        ("ko", _) => &["Noto Sans CJK KR", "Noto Sans KR"],
        _ => han_unification_by_region(locale),
    }
}

fn han_unification_by_region(locale: &str) -> &'static [&'static str] {
    match locale {
        // Hong Kong
        "zh-HK" => &["Noto Sans CJK HK"],
        // Taiwan
        "zh-TW" => &["Noto Sans CJK TC"],
        // Simplified Chinese is the default (also catches "zh-CN" for China)
        // BrowserOS には日本語の書体しか置かないことがあるので、最後に JP も探す
        _ => &["Noto Sans CJK SC", "Noto Sans CJK JP", "Noto Sans JP"],
    }
}

// Fallbacks to use per script
fn script_fallback(script: Script, locale: &str) -> &'static [&'static str] {
    //TODO: abstract style (sans/serif/monospaced)
    match script {
        Script::Adlam => &["Noto Sans Adlam", "Noto Sans Adlam Unjoined"],
        Script::Arabic => &["Noto Sans Arabic"],
        Script::Armenian => &["Noto Sans Armenian"],
        Script::Bengali => &["Noto Sans Bengali"],
        Script::Bopomofo => han_unification(locale),
        //TODO: DejaVu Sans would typically be selected for braille characters,
        // but this breaks alignment when used alongside monospaced text.
        // By requesting the use of FreeMono first, this issue can be avoided.
        Script::Braille => &["FreeMono"],
        Script::Buhid => &["Noto Sans Buhid"],
        Script::Chakma => &["Noto Sans Chakma"],
        Script::Cherokee => &["Noto Sans Cherokee"],
        Script::Deseret => &["Noto Sans Deseret"],
        Script::Devanagari => &["Noto Sans Devanagari"],
        Script::Ethiopic => &["Noto Sans Ethiopic"],
        Script::Georgian => &["Noto Sans Georgian"],
        Script::Gothic => &["Noto Sans Gothic"],
        Script::Grantha => &["Noto Sans Grantha"],
        Script::Gujarati => &["Noto Sans Gujarati"],
        Script::Gurmukhi => &["Noto Sans Gurmukhi"],
        Script::Han => han_unification(locale),
        Script::Hangul => han_unification("ko"),
        Script::Hanunoo => &["Noto Sans Hanunoo"],
        Script::Hebrew => &["Noto Sans Hebrew"],
        Script::Hiragana => han_unification("ja"),
        Script::Javanese => &["Noto Sans Javanese"],
        Script::Kannada => &["Noto Sans Kannada"],
        Script::Katakana => han_unification("ja"),
        Script::Khmer => &["Noto Sans Khmer"],
        Script::Lao => &["Noto Sans Lao"],
        Script::Malayalam => &["Noto Sans Malayalam"],
        Script::Mongolian => &["Noto Sans Mongolian"],
        Script::Myanmar => &["Noto Sans Myanmar"],
        Script::Oriya => &["Noto Sans Oriya"],
        Script::Runic => &["Noto Sans Runic"],
        Script::Sinhala => &["Noto Sans Sinhala"],
        Script::Syriac => &["Noto Sans Syriac"],
        Script::Tagalog => &["Noto Sans Tagalog"],
        Script::Tagbanwa => &["Noto Sans Tagbanwa"],
        Script::Tai_Le => &["Noto Sans Tai Le"],
        Script::Tai_Tham => &["Noto Sans Tai Tham"],
        Script::Tai_Viet => &["Noto Sans Tai Viet"],
        Script::Tamil => &["Noto Sans Tamil"],
        Script::Telugu => &["Noto Sans Telugu"],
        Script::Thaana => &["Noto Sans Thaana"],
        Script::Thai => &["Noto Sans Thai"],
        //TODO: no sans script?
        Script::Tibetan => &["Noto Serif Tibetan"],
        Script::Tifinagh => &["Noto Sans Tifinagh"],
        Script::Vai => &["Noto Sans Vai"],
        //TODO: Use han_unification?
        Script::Yi => &["Noto Sans Yi", "Noto Sans CJK SC"],
        _ => &[],
    }
}

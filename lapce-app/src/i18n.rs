//! Lapce 自身の画面（メニュー・パネル・コマンド パレット・設定）の言語。本物の Lapce には無い。
//!
//! gettext と同じく、ソースに書いた英語の原文をそのままキーにする。`t("Open Folder")` は画面の言語の訳を返し、
//! 訳が無ければ原文を返すので、訳し漏れた文字は英語のまま出る（空になったり壊れたりしない）。
//! 訳は `defaults/locales/<言語>/` の TOML（英語の原文 = 訳文）に置く。
//!
//! 画面の言語は設定の `core.language` で決める。既定の `system` は、ほかの Linux のプログラムと同じく
//! ロケールの環境変数（`LANGUAGE`・`LC_ALL`・`LC_MESSAGES`・`LANG`）に従う。起動する側が言語を決め、
//! 利用者は設定で別の言語を選べる（次に起動したときから効く → set_language）

use std::{
    collections::HashMap,
    fmt::Display,
    sync::{
        LazyLock,
        atomic::{AtomicU8, Ordering},
    },
};

/// 画面を訳してある言語
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Language {
    #[default]
    English,
    Japanese,
}

impl Language {
    /// すべての言語。設定の選択肢に並べる順
    pub const ALL: [Language; 2] = [Language::English, Language::Japanese];

    /// この言語を選ぶ `core.language` の値
    pub fn code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Japanese => "ja",
        }
    }

    /// その言語での言語の名前。言語を選ぶ欄はどの言語でもこれを出す（読めない言語の中からでも自分の言語を探せる）
    pub fn native_name(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Japanese => "日本語",
        }
    }

    /// ロケールの名前（`ja_JP.UTF-8`・`ja`・`en_US`）が指す言語。訳が無い言語なら None
    fn from_locale(locale: &str) -> Option<Language> {
        let language = locale
            .split(['_', '.', '@', '-'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        Language::ALL
            .into_iter()
            .find(|candidate| candidate.code() == language)
    }

    /// ロケールの環境変数が指す言語。gettext と同じ順に読む。`C`・`POSIX`（とロケールが無いとき）は英語
    pub fn from_env() -> Language {
        Self::from_env_vars(|name| std::env::var(name).ok())
    }

    fn from_env_vars(var: impl Fn(&str) -> Option<String>) -> Language {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .filter_map(&var)
            .find(|value| !value.is_empty());
        if matches!(locale.as_deref(), None | Some("C") | Some("POSIX")) {
            return Language::English;
        }
        // LANGUAGE は好みの言語の並び（`ja:en`）で、gettext と同じく C 以外のロケールがあるときだけ効く
        var("LANGUAGE")
            .iter()
            .flat_map(|list| list.split(':'))
            .chain(locale.as_deref())
            .find_map(Language::from_locale)
            .unwrap_or(Language::English)
    }

    /// `core.language` の値が指す言語。`system`（と訳の無い言語）は環境変数に従う
    pub fn from_setting(setting: &str) -> Language {
        Language::ALL
            .into_iter()
            .find(|language| language.code() == setting)
            .unwrap_or_else(Language::from_env)
    }
}

/// 環境変数に従う `core.language` の値
pub const SYSTEM_LANGUAGE: &str = "system";

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// 画面の言語を決める。起動したときに 1 度だけ呼ぶ（→ app.rs の `launch`）。
///
/// ビューは作るときに文字を取り、Lapce は窓のビューとデータを一緒に作って一緒に捨てる（ビューだけを作り直すと、
/// ビューの中で作ったデータの signal が捨てられて落ちる）。動いている途中で言語を変えると、新しく開いたメニューだけが
/// 新しい言語になって混ざるので、VS Code の表示言語と同じく、設定で選び直した言語は次に起動したときから効く
pub fn set_language(language: Language) {
    CURRENT.store(language as u8, Ordering::Relaxed);
}

/// 画面の言語
pub fn language() -> Language {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Language::Japanese,
        _ => Language::English,
    }
}

/// 日本語の訳。画面の部分ごとに 1 ファイル
const JAPANESE_FILES: [&str; 3] = [
    include_str!("../../defaults/locales/ja/commands.toml"),
    include_str!("../../defaults/locales/ja/settings.toml"),
    include_str!("../../defaults/locales/ja/ui.toml"),
];

static JAPANESE: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| catalog(&JAPANESE_FILES));

/// 訳のファイルを読む。訳は Lapce が終わるまで使うので、1 度だけ leak して `&'static str` で渡す
fn catalog(sources: &[&'static str]) -> HashMap<&'static str, &'static str> {
    let mut messages = HashMap::new();
    for source in sources {
        let table: toml::Table = match toml::from_str(source) {
            Ok(table) => table,
            Err(err) => {
                tracing::error!("Failed to read a translation file: {err}");
                continue;
            }
        };
        for (id, value) in table {
            match value {
                toml::Value::String(text) => {
                    messages.insert(leak(id), leak(text));
                }
                // `[context.<名前>]` は 1 つの文脈の訳（→ pt）
                toml::Value::Table(contexts) if id == "context" => {
                    for (context, table) in contexts {
                        let toml::Value::Table(table) = table else {
                            continue;
                        };
                        for (id, value) in table {
                            if let toml::Value::String(text) = value {
                                messages.insert(
                                    leak(context_key(&context, &id)),
                                    leak(text),
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    messages
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// 文脈つきの訳のキー。gettext と同じく EOT でつなぐ
fn context_key(context: &str, id: &str) -> String {
    format!("{context}\u{4}{id}")
}

fn translation(language: Language, id: &str) -> Option<&'static str> {
    match language {
        Language::English => None,
        Language::Japanese => JAPANESE.get(id).copied(),
    }
}

/// `id`（英語の原文）の、画面の言語での文字
pub fn t(id: &'static str) -> &'static str {
    translation(language(), id).unwrap_or(id)
}

/// 組むときには決まっていない文字（設定の項目名など）の [`t`]。訳が無ければその文字を返す
pub fn t_dynamic(id: &str) -> String {
    translation(language(), id)
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

/// 置き場で訳が変わる短い語の [`t_dynamic`]（gettext の `pgettext`）。ボタンの置き場の "Off"（出さない）と
/// スイッチの "Off"（オフ）は訳が違う。訳はファイルの `[context.<文脈>]` に置く
pub fn pt(context: &str, id: &str) -> String {
    translation(language(), &context_key(context, id))
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

/// 値を埋める文の [`t`]。原文の `{}` には順に値が入る。言語によって語順が違うので、訳では `{0}`・`{1}` で
/// 何番目の値かを指して並べ替えてよい
pub fn tf(id: &'static str, args: &[&dyn Display]) -> String {
    format_message(t(id), args)
}

fn format_message(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    let mut next = 0;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let index = match &after[..end] {
            "" => {
                next += 1;
                Some(next - 1)
            }
            digits => digits.parse::<usize>().ok(),
        };
        match index.and_then(|index| args.get(index)) {
            Some(arg) => out.push_str(&arg.to_string()),
            None => out.push_str(&rest[start..start + end + 2]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    #[test]
    fn follows_the_locale_variables_in_gettext_order() {
        assert_eq!(
            Language::from_env_vars(env(&[("LANG", "ja_JP.UTF-8")])),
            Language::Japanese
        );
        assert_eq!(
            Language::from_env_vars(env(&[
                ("LANG", "ja_JP.UTF-8"),
                ("LC_ALL", "en_US.UTF-8")
            ])),
            Language::English
        );
        assert_eq!(
            Language::from_env_vars(env(&[
                ("LANG", "en_US.UTF-8"),
                ("LC_MESSAGES", "ja_JP.UTF-8")
            ])),
            Language::Japanese
        );
        assert_eq!(Language::from_env_vars(env(&[])), Language::English);
        assert_eq!(
            Language::from_env_vars(env(&[("LANG", "fr_FR.UTF-8")])),
            Language::English
        );
    }

    #[test]
    fn language_is_a_list_that_counts_only_with_a_locale() {
        assert_eq!(
            Language::from_env_vars(env(&[
                ("LANG", "en_US.UTF-8"),
                ("LANGUAGE", "fr:ja:en")
            ])),
            Language::Japanese
        );
        assert_eq!(
            Language::from_env_vars(env(&[("LANG", "C"), ("LANGUAGE", "ja")])),
            Language::English
        );
    }

    #[test]
    fn the_setting_picks_a_language_or_follows_the_environment() {
        assert_eq!(Language::from_setting("ja"), Language::Japanese);
        assert_eq!(Language::from_setting("en"), Language::English);
    }

    #[test]
    fn formats_values_in_order_or_by_position() {
        assert_eq!(format_message("Go to line {}", &[&12]), "Go to line 12");
        assert_eq!(format_message("{1} の {0}", &[&"a", &"b"]), "b の a");
        assert_eq!(format_message("{} of {}", &[&1]), "1 of {}");
    }

    #[test]
    fn reads_translations_in_a_context_apart_from_plain_ones() {
        let messages = catalog(&[r#"
"Off" = "オフ"

[context.tab-close-button]
"Off" = "出さない"
"#]);
        assert_eq!(messages.get("Off"), Some(&"オフ"));
        assert_eq!(
            messages.get(context_key("tab-close-button", "Off").as_str()),
            Some(&"出さない")
        );
    }

    // 同じ原文はどのファイルでも同じ訳にする（ファイルを読む順で、どちらの訳が出るかが変わってしまう）
    #[test]
    fn the_same_text_has_one_translation_across_files() {
        let mut seen: HashMap<String, String> = HashMap::new();
        for source in JAPANESE_FILES {
            let table: toml::Table = toml::from_str(source).unwrap();
            for (id, value) in table {
                let toml::Value::String(text) = value else {
                    continue;
                };
                if let Some(other) = seen.insert(id.clone(), text.clone()) {
                    assert_eq!(other, text, "{id} has two translations");
                }
            }
        }
    }

    #[test]
    fn every_japanese_file_reads() {
        for source in JAPANESE_FILES {
            assert!(toml::from_str::<toml::Table>(source).is_ok());
        }
        assert!(!JAPANESE.is_empty());
    }
}

//! The language of Lapce's own UI (menus, panels, the palette, settings).
//!
//! Like gettext, the English text in the source is the message id: `t("Open
//! Folder")` returns its translation in the UI language, or the English text
//! itself when there is none, so a string nobody has translated yet still
//! shows. The translations are TOML tables of English text to translated text
//! in `defaults/locales/<language>/`.
//!
//! The UI language is the `core.language` setting. Its default, `system`,
//! follows the locale environment variables as other Linux programs do
//! (`LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`), so whoever starts Lapce
//! decides the language and the user can still choose another in the settings.

use std::{
    collections::HashMap,
    fmt::Display,
    sync::{
        LazyLock,
        atomic::{AtomicU8, Ordering},
    },
};

/// A language Lapce's UI is translated into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Language {
    #[default]
    English,
    Japanese,
}

impl Language {
    /// Every language, in the order the settings list them.
    pub const ALL: [Language; 2] = [Language::English, Language::Japanese];

    /// The value of `core.language` that picks this language.
    pub fn code(self) -> &'static str {
        match self {
            Language::English => "en",
            Language::Japanese => "ja",
        }
    }

    /// The language's name in that language, as language pickers show it.
    pub fn native_name(self) -> &'static str {
        match self {
            Language::English => "English",
            Language::Japanese => "日本語",
        }
    }

    /// The language a locale name (`ja_JP.UTF-8`, `ja`, `en_US`) asks for, if
    /// Lapce has it.
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

    /// The language the locale environment variables ask for, read in the
    /// order gettext reads them. `C` and `POSIX` (and no locale at all) are
    /// English.
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
        // LANGUAGE is a list of preferences (`ja:en`) and, as in gettext, only
        // counts once a locale other than C is set.
        var("LANGUAGE")
            .iter()
            .flat_map(|list| list.split(':'))
            .chain(locale.as_deref())
            .find_map(Language::from_locale)
            .unwrap_or(Language::English)
    }

    /// The language a `core.language` value asks for: `system` (or anything
    /// Lapce does not have) follows the environment.
    pub fn from_setting(setting: &str) -> Language {
        Language::ALL
            .into_iter()
            .find(|language| language.code() == setting)
            .unwrap_or_else(Language::from_env)
    }
}

/// The value of `core.language` that follows the environment.
pub const SYSTEM_LANGUAGE: &str = "system";

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Sets the UI language. Views built after this show it; the window rebuilds
/// its views when the language in the config changes (see `app.rs`).
pub fn set_language(language: Language) {
    CURRENT.store(language as u8, Ordering::Relaxed);
}

/// The UI language.
pub fn language() -> Language {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Language::Japanese,
        _ => Language::English,
    }
}

/// The Japanese translations, one file per part of the UI.
const JAPANESE_FILES: [&str; 3] = [
    include_str!("../../defaults/locales/ja/commands.toml"),
    include_str!("../../defaults/locales/ja/settings.toml"),
    include_str!("../../defaults/locales/ja/ui.toml"),
];

static JAPANESE: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| catalog(&JAPANESE_FILES));

/// Reads translation files. The strings live as long as Lapce, so they are
/// leaked once to hand out `&'static str`.
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
            if let toml::Value::String(text) = value {
                messages.insert(
                    &*Box::leak(id.into_boxed_str()),
                    &*Box::leak(text.into_boxed_str()),
                );
            }
        }
    }
    messages
}

fn translation(language: Language, id: &str) -> Option<&'static str> {
    match language {
        Language::English => None,
        Language::Japanese => JAPANESE.get(id).copied(),
    }
}

/// The UI text for `id` (its English text) in the UI language.
pub fn t(id: &'static str) -> &'static str {
    translation(language(), id).unwrap_or(id)
}

/// Like [`t`] for text that is not known at compile time (setting names,
/// command titles from a table). Returns the text itself when there is no
/// translation.
pub fn t_dynamic(id: &str) -> String {
    translation(language(), id)
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

/// [`t`] for a message with values in it. In the English text each `{}` takes
/// the next value; a translation can put them in another order with `{0}`,
/// `{1}`, as word order differs between languages.
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
    fn every_japanese_file_reads() {
        for source in JAPANESE_FILES {
            assert!(toml::from_str::<toml::Table>(source).is_ok());
        }
        assert!(!JAPANESE.is_empty());
    }
}

//! Choosing the display language from the shell environment.
//!
//! The language is never a command-line option: a user who wants Chinese sets
//! `LANG` or `LC_ALL`, exactly as they would for any other program in that
//! shell. Machine-readable output stays English, so a script does not change
//! meaning when a human switches locale.

use std::env;

/// Language used for human-readable output and alert emails.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Language {
    /// English.
    #[default]
    #[value(name = "en")]
    English,
    /// Simplified Chinese.
    #[value(name = "zh-CN")]
    SimplifiedChinese,
}

impl Language {
    /// Read the language from the shell locale.
    ///
    /// `LC_ALL` overrides `LC_MESSAGES`, which overrides `LANG`, as POSIX
    /// defines the precedence. A variable that is set but empty counts as
    /// unset, so `LC_ALL= LANG=zh_CN.UTF-8` still selects Chinese.
    pub fn from_env() -> Self {
        Self::from_vars(|name| env::var(name).ok())
    }

    /// [`Language::from_env`] with the lookups injected, so the precedence is
    /// testable without mutating the process environment.
    fn from_vars(lookup: impl Fn(&str) -> Option<String>) -> Self {
        for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Some(value) = lookup(name) {
                if !value.trim().is_empty() {
                    return Self::from_locale(&value);
                }
            }
        }
        Self::default()
    }

    /// Map a POSIX locale name onto a supported language.
    ///
    /// Only the language part is read, so `zh_CN.UTF-8`, `zh-CN.utf8` and
    /// `zh_TW.UTF-8` all select Simplified Chinese, the only Chinese
    /// translation the tool ships. Everything else, including `C` and
    /// `POSIX`, falls back to English.
    pub fn from_locale(value: &str) -> Self {
        let base = value
            .trim()
            .split(['.', '@'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .replace('-', "_");
        let language = base.split('_').next().unwrap_or_default();
        match language {
            "zh" => Self::SimplifiedChinese,
            _ => Self::English,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_set_variable_wins() {
        let vars = [("LC_ALL", "zh_CN.UTF-8"), ("LANG", "en_US.UTF-8")];
        assert_eq!(
            Language::from_vars(|name| find(&vars, name)),
            Language::SimplifiedChinese
        );

        let vars = [("LC_MESSAGES", "zh_CN.UTF-8"), ("LANG", "en_US.UTF-8")];
        assert_eq!(
            Language::from_vars(|name| find(&vars, name)),
            Language::SimplifiedChinese
        );

        let vars = [("LANG", "zh_CN.UTF-8")];
        assert_eq!(
            Language::from_vars(|name| find(&vars, name)),
            Language::SimplifiedChinese
        );
    }

    #[test]
    fn an_empty_variable_falls_through() {
        let vars = [
            ("LC_ALL", ""),
            ("LC_MESSAGES", "   "),
            ("LANG", "zh_CN.UTF-8"),
        ];
        assert_eq!(
            Language::from_vars(|name| find(&vars, name)),
            Language::SimplifiedChinese
        );
    }

    #[test]
    fn nothing_set_means_english() {
        assert_eq!(Language::from_vars(|_| None), Language::English);
    }

    #[test]
    fn locale_names_lose_their_encoding_and_territory() {
        for name in [
            "zh_CN.UTF-8",
            "zh_CN.utf8",
            "zh-CN",
            "zh_TW.UTF-8",
            "zh",
            "ZH_CN@euro",
        ] {
            assert_eq!(
                Language::from_locale(name),
                Language::SimplifiedChinese,
                "locale {name}"
            );
        }
    }

    #[test]
    fn other_locales_stay_english() {
        for name in ["en_US.UTF-8", "C", "C.UTF-8", "POSIX", "de_DE.UTF-8", ""] {
            assert_eq!(
                Language::from_locale(name),
                Language::English,
                "locale {name}"
            );
        }
    }

    fn find(vars: &[(&str, &str)], name: &str) -> Option<String> {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_string())
    }
}

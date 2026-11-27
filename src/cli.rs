//! Command-line surface.

use std::path::{Path, PathBuf};

use clap::{Command, CommandFactory, Parser};

use crate::alert::Repeat;
use crate::i18n::Language;

/// Monitor Cloudflare R2 usage against the free tier.
///
/// Reads two Cloudflare GraphQL Analytics datasets and reports how much of the
/// monthly free allowance each billable metric has consumed. Exits non-zero
/// once a configurable threshold is crossed, so a supervisor can act on it.
///
/// The token needs exactly one permission: Account → Account Analytics → Read.
#[derive(Debug, Clone, Parser)]
#[command(name = "cf-r2-monitor", version, about, long_about = None)]
pub struct Args {
    /// Cloudflare account ID (32 hex characters, not an email address).
    #[arg(long, env = "CLOUDFLARE_ACCOUNT_ID")]
    pub account_id: Option<String>,

    /// File containing the API token. Preferred over --api-token: the token
    /// never appears in the process list or a shell history.
    ///
    /// This deliberately does not declare `conflicts_with = "api_token"`.
    /// clap counts an environment variable as a source for the argument it
    /// feeds, so any shell that exported `CLOUDFLARE_API_TOKEN` could then
    /// never pass `--api-token-file`: every invocation would fail with
    /// "cannot be used with". The file simply wins when both are set, which is
    /// what the precedence documented here has always promised.
    #[arg(long, env = "CLOUDFLARE_API_TOKEN_FILE")]
    pub api_token_file: Option<PathBuf>,

    /// API token passed directly.
    #[arg(long, env = "CLOUDFLARE_API_TOKEN", hide_env_values = true)]
    pub api_token: Option<String>,

    /// Fraction of the free tier that counts as a warning.
    #[arg(long, default_value_t = 0.80, value_parser = ratio)]
    pub warn: f64,

    /// Fraction of the free tier that counts as critical.
    #[arg(long, default_value_t = 0.95, value_parser = ratio)]
    pub critical: f64,

    /// Reporting window, for example 30d, 7d or 24h. Cloudflare caps the
    /// window at 31 days.
    #[arg(long, default_value = "30d", value_parser = parse_period)]
    pub period: Period,

    /// Print the report as JSON instead of a table.
    #[arg(long)]
    pub json: bool,

    /// Override the GraphQL endpoint. Intended for testing.
    #[arg(long, env = "CLOUDFLARE_GRAPHQL_ENDPOINT")]
    pub endpoint: Option<String>,

    /// Send a mail through himalaya when a threshold is crossed. Repeat the
    /// flag or separate addresses with commas.
    #[arg(long, value_name = "ADDRESS", value_delimiter = ',')]
    pub alert_to: Vec<String>,

    /// Envelope sender, written as the `From:` header.
    ///
    /// himalaya takes the SMTP envelope sender from `From:` and refuses a
    /// message without it, so this is not optional when alerting: leaving it
    /// out would make every alert fail at the mailer.
    #[arg(long, value_name = "ADDRESS", requires = "alert_to")]
    pub alert_from: Option<String>,

    /// Mailer to run. It must read a raw message on stdin.
    #[arg(
        long,
        value_name = "PROGRAM",
        default_value = "himalaya",
        requires = "alert_to"
    )]
    pub alert_command: String,

    /// himalaya account to send from (`-a`).
    #[arg(long, value_name = "NAME", requires = "alert_to")]
    pub alert_account: Option<String>,

    /// himalaya configuration file (`-c`). Keep it out of the Nix store: it
    /// usually holds an SMTP password.
    #[arg(long, value_name = "PATH", requires = "alert_to")]
    pub alert_config: Option<PathBuf>,

    /// File that remembers the last alert, so an unchanged condition is not
    /// repeated on every run. Without it every run mails.
    #[arg(long, value_name = "PATH", requires = "alert_to")]
    pub alert_state: Option<PathBuf>,

    /// How long an unchanged condition stays quiet. `0` mails every run.
    #[arg(
        long,
        value_name = "DURATION",
        default_value = "24h",
        value_parser = parse_repeat,
        requires = "alert_to"
    )]
    pub alert_repeat: Repeat,

    /// Language of alert emails; follows the shell locale when omitted.
    #[arg(long, value_enum, requires = "alert_to")]
    pub alert_language: Option<Language>,

    /// Do not announce that a metric came back under its threshold.
    #[arg(long, requires = "alert_to")]
    pub alert_no_recovery: bool,

    /// Print the message instead of sending it.
    #[arg(long, requires = "alert_to")]
    pub alert_dry_run: bool,
}

/// Chinese help for every argument, keyed by the id clap derives from the field
/// name.
///
/// The English text lives on the fields themselves; this table replaces it when
/// the shell locale asks for Chinese. An argument missing here keeps its English
/// text, so a test checks every id against the real command.
const ZH_ARG_HELP: &[(&str, &str)] = &[
    (
        "account_id",
        "Cloudflare 账号 ID（32 位十六进制，不是邮箱地址）",
    ),
    (
        "api_token_file",
        "存放 API token 的文件，优先于 --api-token：token 不会出现在进程列表或 shell 历史里",
    ),
    ("api_token", "直接传入 API token"),
    ("warn", "算作警告的免费额度占比，例如 0.8"),
    ("critical", "算作严重的免费额度占比，例如 0.95"),
    (
        "period",
        "统计窗口，例如 30d、7d、24h；Cloudflare 最多允许 31 天",
    ),
    ("json", "输出 JSON 而不是表格"),
    ("endpoint", "覆盖 GraphQL 端点，仅供测试"),
    (
        "alert_to",
        "越线时通过 himalaya 发信；可重复，或用逗号分隔多个地址",
    ),
    (
        "alert_from",
        "信封发件人，写入 From: 头；himalaya 拒绝没有它的邮件",
    ),
    (
        "alert_command",
        "实际执行的发信程序，必须从 stdin 读取原始邮件",
    ),
    ("alert_account", "用 himalaya 的哪个账号发送（-a）"),
    (
        "alert_config",
        "himalaya 配置文件（-c），通常含 SMTP 密码，不要放进 Nix store",
    ),
    ("alert_state", "记录上次告警的文件；不给就每次都发"),
    ("alert_repeat", "同一状态的最短静默时间，0 表示每次运行都发"),
    ("alert_language", "告警邮件语言；不给就跟随 shell locale"),
    ("alert_no_recovery", "指标恢复时不发邮件"),
    ("alert_dry_run", "只打印邮件内容，不实际发送"),
];

/// Build the command with its help text in `language`.
///
/// clap has no translation layer, so the scaffolding it writes itself (`Usage:`,
/// `Options:` and its parse errors) stays English. The program description and
/// every argument explanation are replaced here.
pub fn command(language: Language) -> Command {
    let mut command = Args::command();
    if language == Language::SimplifiedChinese {
        command = command.about("检查 Cloudflare R2 用量距离免费额度还有多远");
        for (id, text) in ZH_ARG_HELP {
            command = command.mut_arg(*id, |arg| arg.help(*text).long_help(*text));
        }
    }
    command
}

impl Args {
    /// Validate combinations clap cannot express on its own.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.warn > self.critical {
            return Err("--warn must not exceed --critical");
        }
        if self
            .alert_to
            .iter()
            .any(|address| address.trim().is_empty())
        {
            return Err("--alert-to must not be empty");
        }
        // himalaya takes the envelope sender from `From:` and refuses a
        // message without one, so an alert without --alert-from would fail
        // every single time. Rejecting it here is far clearer than a mailer
        // error at 3am.
        if !self.alert_to.is_empty() && self.alert_from.is_none() {
            return Err("--alert-from is required with --alert-to: himalaya needs a From header");
        }
        Ok(())
    }

    /// Resolve the token, reading a file when one was given.
    ///
    /// The file wins over an inline token: passing both is not an error,
    /// because an inherited `CLOUDFLARE_API_TOKEN` is not under the caller's
    /// control and must not make `--api-token-file` unusable.
    ///
    /// A trailing newline is stripped because editors add one and Cloudflare
    /// rejects tokens with trailing whitespace.
    pub fn token(&self) -> Result<Option<String>, TokenError> {
        if let Some(path) = &self.api_token_file {
            return read_token(path).map(Some);
        }
        if let Some(raw) = &self.api_token {
            return Ok(Some(raw.trim().to_string()));
        }
        Ok(None)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("could not read token file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("token file {0} is empty")]
    Empty(PathBuf),
}

/// Read a token from disk, trimming surrounding whitespace.
pub fn read_token(path: &Path) -> Result<String, TokenError> {
    let raw = std::fs::read_to_string(path).map_err(|source| TokenError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let trimmed = raw.trim().to_string();
    if trimmed.is_empty() {
        return Err(TokenError::Empty(path.to_path_buf()));
    }
    Ok(trimmed)
}

/// A reporting window expressed in the units Cloudflare accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    /// Window length in hours, always at least one.
    pub hours: u32,
}

impl Period {
    /// Cloudflare rejects windows wider than this.
    pub const MAX_HOURS: u32 = 31 * 24;

    /// Whole days in the window, rounded down, at least one.
    pub fn days(self) -> u32 {
        (self.hours / 24).max(1)
    }

    /// Render the window the way it was most likely written.
    pub fn describe(self) -> String {
        if self.hours.is_multiple_of(24) {
            format!("{}d", self.hours / 24)
        } else {
            format!("{}h", self.hours)
        }
    }
}

impl std::fmt::Display for Period {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

/// Parse `30d`, `7d`, `24h`, `90m` or a bare number of days.
pub fn parse_period(input: &str) -> Result<Period, String> {
    let text = input.trim();
    if text.is_empty() {
        return Err("period must not be empty".into());
    }
    let (digits, unit) = match text.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((index, _)) => text.split_at(index),
        None => (text, "d"),
    };
    let value: u32 = digits
        .trim()
        .parse()
        .map_err(|_| format!("`{text}` is not a number followed by d, h or m"))?;
    if value == 0 {
        return Err("period must be greater than zero".into());
    }
    let hours = match unit.trim() {
        "m" => value.div_ceil(60).max(1),
        "h" => value,
        "d" => value * 24,
        other => return Err(format!("unknown period unit `{other}`, expected d, h or m")),
    };
    if hours > Period::MAX_HOURS {
        return Err(format!(
            "period is longer than Cloudflare's {}-day query limit",
            Period::MAX_HOURS / 24
        ));
    }
    Ok(Period { hours })
}

/// Accept a ratio written either as `0.8` or as `80`.
fn ratio(input: &str) -> Result<f64, String> {
    let value: f64 = input
        .trim()
        .parse()
        .map_err(|_| format!("`{input}` is not a number"))?;
    if !(value.is_finite() && value > 0.0) {
        return Err("threshold must be a positive number".into());
    }
    // Interpret values above 1 as percentages so `--warn 80` works.
    let ratio = if value > 1.0 { value / 100.0 } else { value };
    if ratio > 1.0 {
        return Err("threshold must not exceed 100%".into());
    }
    Ok(ratio)
}

/// clap adapter for [`Repeat::parse`].
fn parse_repeat(input: &str) -> Result<Repeat, String> {
    Repeat::parse(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn period_accepts_days_hours_minutes() {
        assert_eq!(parse_period("30d").unwrap().hours, 720);
        assert_eq!(parse_period("7d").unwrap().days(), 7);
        assert_eq!(parse_period("24h").unwrap().hours, 24);
        assert_eq!(parse_period("90m").unwrap().hours, 2);
        assert_eq!(parse_period("30").unwrap().hours, 720);
    }

    #[test]
    fn period_rejects_windows_cloudflare_would_refuse() {
        let err = parse_period("90d").unwrap_err();
        assert!(err.contains("31-day"), "got: {err}");
    }

    #[test]
    fn period_rejects_junk() {
        assert!(parse_period("").is_err());
        assert!(parse_period("0d").is_err());
        assert!(parse_period("abc").is_err());
        assert!(parse_period("5w").is_err());
    }

    #[test]
    fn period_describes_itself() {
        assert_eq!(parse_period("30d").unwrap().describe(), "30d");
        assert_eq!(parse_period("12h").unwrap().describe(), "12h");
    }

    #[test]
    fn thresholds_accept_fractions_and_percentages() {
        assert_eq!(ratio("0.8").unwrap(), 0.8);
        assert_eq!(ratio("80").unwrap(), 0.8);
        assert!(ratio("0").is_err());
        assert!(ratio("101").is_err());
        assert!(ratio("NaN").is_err());
    }

    #[test]
    fn every_translated_argument_exists() {
        let command = Args::command();
        for (id, _) in ZH_ARG_HELP {
            assert!(
                command
                    .get_arguments()
                    .any(|arg| arg.get_id().as_str() == *id),
                "ZH_ARG_HELP names an argument that does not exist: {id}"
            );
        }
    }

    #[test]
    fn chinese_help_replaces_the_derived_text() {
        let help = command(Language::SimplifiedChinese)
            .render_help()
            .to_string();
        assert!(help.contains("检查 Cloudflare R2 用量"), "got:\n{help}");
        assert!(help.contains("账号 ID"), "got:\n{help}");
        assert!(
            !help.contains("Cloudflare account ID (32 hex"),
            "got:\n{help}"
        );
    }

    #[test]
    fn english_help_keeps_the_derived_text() {
        let help = command(Language::English).render_help().to_string();
        assert!(
            help.contains("Cloudflare account ID (32 hex"),
            "got:\n{help}"
        );
        assert!(!help.contains("账号 ID"), "got:\n{help}");
    }
    #[test]
    fn defaults_are_sane() {
        let args = Args::parse_from(["cf-r2-monitor"]);
        assert_eq!(args.warn, 0.80);
        assert_eq!(args.critical, 0.95);
        assert_eq!(args.period.hours, 720);
        assert!(!args.json);
        assert!(args.validate().is_ok());
    }

    #[test]
    fn alert_language_is_optional_and_rejects_unknown_values() {
        // None means "follow the shell locale"; the caller decides.
        assert_eq!(Args::parse_from(["cf-r2-monitor"]).alert_language, None);
        let args = Args::try_parse_from([
            "cf-r2-monitor",
            "--alert-to",
            "ops@example.org",
            "--alert-language",
            "zh-CN",
        ])
        .unwrap();
        assert_eq!(args.alert_language, Some(Language::SimplifiedChinese));
        assert!(Args::try_parse_from([
            "cf-r2-monitor",
            "--alert-to",
            "ops@example.org",
            "--alert-language",
            "fr"
        ])
        .is_err());
    }

    #[test]
    fn warn_above_critical_is_rejected() {
        let args = Args::parse_from(["cf-r2-monitor", "--warn", "0.9", "--critical", "0.5"]);
        assert!(args.validate().is_err());
    }

    /// Passing both is allowed on purpose: an inherited `CLOUDFLARE_API_TOKEN`
    /// must not make `--api-token-file` unusable, so the file simply wins.
    #[test]
    fn the_token_file_wins_over_an_inline_token() {
        let dir = Scratch::new();
        let path = dir.join("token");
        std::fs::write(&path, "from-file\n").unwrap();
        let args = Args::parse_from([
            "cf-r2-monitor",
            "--api-token",
            "from-flag",
            "--api-token-file",
            path.to_str().unwrap(),
        ]);
        assert_eq!(args.token().unwrap().as_deref(), Some("from-file"));
    }

    #[test]
    fn token_is_trimmed() {
        let args = Args::parse_from(["cf-r2-monitor", "--api-token", "  secret\n"]);
        assert_eq!(args.token().unwrap().as_deref(), Some("secret"));
    }

    #[test]
    fn missing_token_file_names_the_path() {
        let dir = Scratch::new();
        let path = dir.join("nope");
        let err = read_token(&path).unwrap_err();
        assert!(err.to_string().contains("nope"), "got: {err}");
    }

    #[test]
    fn empty_token_file_is_rejected() {
        let dir = Scratch::new();
        let path = dir.join("empty");
        std::fs::write(&path, "  \n").unwrap();
        let err = read_token(&path).unwrap_err();
        assert!(matches!(err, TokenError::Empty(_)));
    }

    #[test]
    fn token_file_is_read_and_trimmed() {
        let dir = Scratch::new();
        let path = dir.join("token");
        std::fs::write(&path, "tok123\n").unwrap();
        assert_eq!(read_token(&path).unwrap(), "tok123");
    }

    /// A scratch directory for one test, removed again when it drops.
    ///
    /// Earlier versions left these behind: a full test run created one per
    /// test and nothing ever deleted them, so repeated runs quietly filled
    /// `/tmp` with empty directories.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "cf-r2-monitor-test-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn join(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // Best effort: a leftover directory is untidy, not a test failure.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

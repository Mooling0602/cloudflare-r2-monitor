//! Threshold alerts delivered by an external mailer.
//!
//! Nothing here speaks SMTP. The report is rendered into a raw RFC 5322
//! message and handed to `himalaya message send` on standard input, which is
//! the only supported mailer. That keeps the dependency tree small and lets
//! the user keep their mail credentials in one place instead of in this
//! program's configuration.
//!
//! The module is deliberately split in two:
//!
//! * pure functions build the decision ([`Decision`]) and the message
//!   ([`compose`]) from a report and a remembered [`AlertState`], so the
//!   interesting logic is unit-testable without a mailer or a filesystem;
//! * [`Sender`] runs the mailer and reports what happened.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::i18n::Language;
use crate::quota::{humanize_bytes, Metric, MetricReading, Status, UsageReport};

/// How long an unchanged condition stays quiet before repeating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repeat {
    /// Zero means "send every time".
    minutes: u32,
}

impl Repeat {
    /// Parse `24h`, `2d`, `90m`, `0` or a bare number of hours.
    ///
    /// Bare numbers are hours, matching `--period`. `0` disables the quiet
    /// period entirely, which is what a user wants when they only run the
    /// check once a day and would otherwise miss a repeat.
    pub fn parse(input: &str) -> Result<Self, String> {
        let text = input.trim();
        if text.is_empty() {
            return Err("repeat must not be empty".into());
        }
        let (digits, unit) = match text.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
            Some((index, _)) => text.split_at(index),
            None => (text, "h"),
        };
        let value: u32 = digits
            .trim()
            .parse()
            .map_err(|_| format!("`{text}` is not a number of hours, minutes or days"))?;
        let minutes = match unit.trim() {
            "m" => value,
            "h" => value.saturating_mul(60),
            "d" => value.saturating_mul(60 * 24),
            other => return Err(format!("unknown repeat unit `{other}`, expected d, h or m")),
        };
        Ok(Self { minutes })
    }

    /// How long to stay quiet. `None` means "never stay quiet".
    pub fn quiet_period(self) -> Option<Duration> {
        if self.minutes == 0 {
            None
        } else {
            Some(Duration::minutes(i64::from(self.minutes)))
        }
    }

    /// Render the value the way it was most likely written.
    pub fn describe(self) -> String {
        match self.minutes {
            0 => "0".into(),
            minutes if minutes.is_multiple_of(60 * 24) => format!("{}d", minutes / (60 * 24)),
            minutes if minutes.is_multiple_of(60) => format!("{}h", minutes / 60),
            minutes => format!("{minutes}m"),
        }
    }
}

/// What the previous run remembered, one entry per metric.
///
/// The file is intentionally a plain map so an operator can read it, and so a
/// future metric can be added without a migration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AlertState {
    #[serde(default)]
    pub metrics: BTreeMap<String, Remembered>,
}

/// The last thing said about one metric.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Remembered {
    /// The status the last message described.
    pub status: Status,
    /// When that message was sent.
    pub sent_at: DateTime<Utc>,
}

impl AlertState {
    /// Read a state file, treating a missing one as empty history.
    ///
    /// A corrupt file is an error rather than a silent reset: silently
    /// forgetting history would flood the operator with repeats, which is the
    /// failure mode this file exists to prevent.
    pub fn load(path: &Path) -> Result<Self, StateError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => {
                return Err(StateError::Io {
                    path: path.to_path_buf(),
                    error,
                })
            }
        };
        serde_json::from_str(&raw).map_err(|error| StateError::Parse {
            path: path.to_path_buf(),
            error,
        })
    }

    /// Write the state file through a temporary file in the same directory.
    ///
    /// The rename keeps a crash from leaving a half-written file that the next
    /// run would refuse to parse.
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|error| StateError::Io {
                    path: parent.to_path_buf(),
                    error,
                })?;
            }
        }
        let rendered = serde_json::to_string_pretty(self).map_err(|error| StateError::Parse {
            path: path.to_path_buf(),
            error,
        })?;
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, rendered).map_err(|error| StateError::Io {
            path: temporary.clone(),
            error,
        })?;
        std::fs::rename(&temporary, path).map_err(|error| StateError::Io {
            path: path.to_path_buf(),
            error,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("could not read or write {path}: {error}")]
    Io {
        path: PathBuf,
        #[source]
        error: std::io::Error,
    },
    #[error("{path} is not valid alert state: {error}")]
    Parse {
        path: PathBuf,
        #[source]
        error: serde_json::Error,
    },
}

/// What to mail this run.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Nothing crossed a threshold.
    Quiet,
    /// Statuses only improved; mail the recovery summary.
    Recovered { previous: Vec<Metric> },
    /// At least one metric is in a worse state than last time, or the quiet
    /// period has expired.
    Alert {
        reasons: Vec<Reason>,
        repeated: bool,
    },
}

impl Decision {
    /// Whether the caller has anything to send.
    pub fn needs_mail(&self) -> bool {
        !matches!(self, Self::Quiet)
    }

    /// Whether this run should rewrite the state file.
    pub fn touches_state(&self) -> bool {
        self.needs_mail()
    }
}

/// Why one metric is worth mentioning.
#[derive(Debug, Clone, PartialEq)]
pub struct Reason {
    pub metric: Metric,
    pub status: Status,
    /// `true` when the metric was already at `status` last time and is being
    /// repeated because the quiet period expired.
    pub repeated: bool,
    pub reading: MetricReading,
}

/// Decide what to send.
///
/// The rules, in order:
///
/// * a metric that changed to a worse status is always reported;
/// * a metric that stayed at the same non-ok status is reported again only
///   once the quiet period expired;
/// * a metric that improved is not itself a reason to mail, but it is
///   mentioned in the body so the operator sees the trend;
/// * if nothing is reported and something used to be, the recovery mail is
///   sent once.
pub fn decide(
    report: &UsageReport,
    previous: &AlertState,
    now: DateTime<Utc>,
    repeat: Repeat,
) -> Decision {
    let quiet = repeat.quiet_period();
    let mut reasons = Vec::new();
    let mut recovered = Vec::new();

    for reading in &report.readings {
        let key = key_of(reading.metric);
        let remembered = previous.metrics.get(key);
        match reading.status {
            Status::Ok => {
                if remembered.is_some() {
                    recovered.push(reading.metric);
                }
            }
            status => {
                let escalated = remembered.is_none_or(|old| status > old.status);
                let expired = remembered.is_none_or(|old| match quiet {
                    // No quiet period: every run is worth a mail.
                    None => true,
                    Some(period) => now - old.sent_at >= period,
                });
                if escalated || expired {
                    reasons.push(Reason {
                        metric: reading.metric,
                        status,
                        repeated: !escalated,
                        reading: reading.clone(),
                    });
                }
            }
        }
    }

    if !reasons.is_empty() {
        return Decision::Alert {
            repeated: reasons.iter().all(|reason| reason.repeated),
            reasons,
        };
    }

    if !recovered.is_empty() {
        return Decision::Recovered {
            previous: recovered,
        };
    }

    Decision::Quiet
}

fn key_of(metric: Metric) -> &'static str {
    match metric {
        Metric::Storage => "storage",
        Metric::ClassA => "class_a",
        Metric::ClassB => "class_b",
    }
}

/// Fold this run's outcome into the remembered state.
///
/// Only metrics that were actually mailed are stamped, so a metric that stayed
/// quiet keeps its old timestamp and will be repeated as soon as its period
/// expires rather than counting from a run that said nothing.
pub fn remember(
    previous: &mut AlertState,
    report: &UsageReport,
    decision: &Decision,
    now: DateTime<Utc>,
) {
    match decision {
        Decision::Quiet => {}
        Decision::Recovered { .. } => previous.metrics.clear(),
        Decision::Alert { reasons, .. } => {
            // A metric that fell back to ok while others are still alerting
            // has been dealt with; forget it so its recovery is not announced
            // a second time later.
            let alerting: Vec<&str> = reasons.iter().map(|r| key_of(r.metric)).collect();
            previous
                .metrics
                .retain(|key, _| alerting.contains(&key.as_str()));
            for reason in reasons {
                previous.metrics.insert(
                    key_of(reason.metric).to_string(),
                    Remembered {
                        status: reason.status,
                        sent_at: now,
                    },
                );
            }
            let _ = report;
        }
    }
}

/// Everything needed to render and send one message.
#[derive(Debug, Clone)]
pub struct Envelope {
    pub to: Vec<String>,
    /// Envelope sender, written as `From:`.
    ///
    /// Not optional: himalaya derives the SMTP envelope from the message's
    /// `From:` header and refuses to submit a message without one
    /// (`No \`From:\` header found in raw message`), so a message built
    /// without a sender could never be delivered.
    pub from: String,
    /// Hostname put in the `Message-ID` and the `X-Mailer` line.
    pub host: String,
    /// The report's period, shown in the subject.
    pub period: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error("no recipient was configured; pass at least one --alert-to")]
    NoRecipient,
    #[error("`{0}` is not a usable email address")]
    BadAddress(String),
    #[error("the message would contain a bare newline in a header value: `{0}`")]
    HeaderInjection(String),
}

/// Render an English message as raw RFC 5322 text.
pub fn compose(
    report: &UsageReport,
    decision: &Decision,
    envelope: &Envelope,
    now: DateTime<Utc>,
) -> Result<String, ComposeError> {
    compose_in_language(report, decision, envelope, now, Language::English)
}

/// Render the message as raw RFC 5322 text.
///
/// Line endings are `\n` on purpose: `himalaya` normalises them to CRLF
/// itself, and doing it here would only make the dry-run output harder to
/// read in a terminal.
pub fn compose_in_language(
    report: &UsageReport,
    decision: &Decision,
    envelope: &Envelope,
    now: DateTime<Utc>,
    language: Language,
) -> Result<String, ComposeError> {
    if envelope.to.is_empty() {
        return Err(ComposeError::NoRecipient);
    }
    for address in &envelope.to {
        check_address(address)?;
    }
    check_address(&envelope.from)?;

    let subject = match decision {
        Decision::Quiet => "Cloudflare R2 usage: quiet".to_string(),
        Decision::Recovered { .. } => {
            "[r2-monitor] recovered — R2 usage back under threshold".to_string()
        }
        Decision::Alert { reasons, .. } => {
            let worst = reasons
                .iter()
                .map(|reason| reason.status)
                .max()
                .unwrap_or(Status::Warn);
            let names = reasons
                .iter()
                .map(|reason| short(reason.metric))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[r2-monitor] {} — {names}", worst.label())
        }
    };

    let subject = match language {
        Language::English => subject,
        Language::SimplifiedChinese => chinese_subject(decision),
    };

    let mut message = String::new();
    push_header(&mut message, "From", Some(&envelope.from))?;
    push_header(&mut message, "To", Some(&envelope.to.join(", ")))?;
    if subject.contains(['\n', '\r']) {
        return Err(ComposeError::HeaderInjection("Subject".into()));
    }
    message.push_str("Subject: ");
    message.push_str(&encode_subject(&subject));
    message.push('\n');
    push_header(&mut message, "Date", Some(&now.to_rfc2822()))?;
    push_header(
        &mut message,
        "Message-ID",
        Some(&format!("<{}@{}>", unique_id(now), envelope.host)),
    )?;
    push_header(
        &mut message,
        "X-Mailer",
        Some(&format!("cf-r2-monitor/{}", env!("CARGO_PKG_VERSION"))),
    )?;
    push_header(&mut message, "MIME-Version", Some("1.0"))?;
    push_header(
        &mut message,
        "Content-Type",
        Some("text/plain; charset=utf-8"),
    )?;
    push_header(&mut message, "Content-Transfer-Encoding", Some("8bit"))?;
    message.push('\n');
    message.push_str(&match language {
        Language::English => body(report, decision, &envelope.period),
        Language::SimplifiedChinese => chinese_body(report, decision, &envelope.period),
    });
    Ok(message)
}

fn push_header(message: &mut String, name: &str, value: Option<&str>) -> Result<(), ComposeError> {
    // A header value with a newline would let a crafted bucket name inject
    // headers, so it is refused rather than stripped: silently rewriting data
    // hides the problem.
    let Some(value) = value else {
        return Ok(());
    };
    if value.contains('\n') || value.contains('\r') {
        return Err(ComposeError::HeaderInjection(name.to_string()));
    }
    message.push_str(name);
    message.push_str(": ");
    message.push_str(value);
    message.push('\n');
    Ok(())
}

fn body(report: &UsageReport, decision: &Decision, period: &str) -> String {
    let mut out = String::new();

    match decision {
        Decision::Recovered { previous } => {
            out.push_str("R2 usage is back under the configured thresholds.\n\n");
            let names = previous
                .iter()
                .map(|metric| short(*metric))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("Recovered: {names}\n\n"));
        }
        Decision::Alert { reasons, repeated } => {
            if *repeated {
                out.push_str("This is a repeat: the condition has not changed.\n\n");
            } else {
                out.push_str("At least one R2 metric crossed a configured threshold.\n\n");
            }
            for reason in reasons {
                let reading = &reason.reading;
                out.push_str(&format!(
                    "{} is at {:.1}% of the free tier ({})",
                    reading.metric.label(),
                    reading.percent(),
                    reason.status.label().to_uppercase(),
                ));
                if reading.overage() > 0 {
                    out.push_str(&format!(
                        "; past the limit by {}",
                        humanize_metric(reading.metric, reading.overage())
                    ));
                } else {
                    out.push_str(&format!(
                        "; {} left",
                        humanize_metric(reading.metric, reading.remaining())
                    ));
                }
                if reason.repeated {
                    out.push_str(" [repeat]");
                }
                out.push('\n');
            }
            out.push('\n');
        }
        Decision::Quiet => out.push_str("Nothing to report.\n\n"),
    }

    out.push_str(&format!("Free tier, window {period}\n"));
    for reading in &report.readings {
        out.push_str(&format!(
            "  {:<20} {:>12} / {:<12} {:>6.1}%  {}\n",
            reading.metric.label(),
            humanize_metric(reading.metric, reading.used),
            humanize_metric(reading.metric, reading.limit),
            reading.percent(),
            reading.status.label(),
        ));
    }

    let storage = &report.storage;
    out.push('\n');
    if storage.observed_days == 0 {
        out.push_str(&format!(
            "Storage: {} stored now; no dated readings, GB-month unknown.\n",
            humanize_bytes(storage.current_bytes)
        ));
    } else {
        out.push_str(&format!(
            "Storage: {} now, {} daily peak average, {} accrued over {} day(s).\n",
            humanize_bytes(storage.current_bytes),
            humanize_bytes(storage.average_daily_peak),
            humanize_bytes(storage.accrued_month_bytes),
            storage.observed_days,
        ));
    }
    out.push_str(&format!(
        "Thresholds: warn {:.0}%, critical {:.0}%.\n",
        report.thresholds.warn * 100.0,
        report.thresholds.critical * 100.0,
    ));
    out.push_str("\n-- \ncf-r2-monitor: Cloudflare R2 free-tier usage monitor\n");
    out
}

fn chinese_metric(metric: Metric) -> &'static str {
    match metric {
        Metric::Storage => "存储",
        Metric::ClassA => "A 类操作",
        Metric::ClassB => "B 类操作",
    }
}

fn chinese_status(status: Status) -> &'static str {
    match status {
        Status::Ok => "正常",
        Status::Warn => "警告",
        Status::Critical => "严重",
    }
}

fn chinese_subject(decision: &Decision) -> String {
    match decision {
        Decision::Quiet => "[r2-monitor] 用量正常".into(),
        Decision::Recovered { .. } => "[r2-monitor] 已恢复：R2 用量低于阈值".into(),
        Decision::Alert { reasons, .. } => {
            let worst = reasons
                .iter()
                .map(|reason| reason.status)
                .max()
                .unwrap_or(Status::Warn);
            let names = reasons
                .iter()
                .map(|reason| chinese_metric(reason.metric))
                .collect::<Vec<_>>()
                .join("、");
            format!("[r2-monitor] {}：{names}", chinese_status(worst))
        }
    }
}

// Split on UTF-8 character boundaries to keep each encoded word below 75 bytes.
fn encode_subject(subject: &str) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    let chars: Vec<char> = subject.chars().collect();
    chars
        .chunks(9)
        .map(|chunk| {
            let text: String = chunk.iter().collect();
            format!("=?UTF-8?B?{}?=", STANDARD.encode(text))
        })
        .collect::<Vec<_>>()
        .join("\n ")
}

fn chinese_body(report: &UsageReport, decision: &Decision, period: &str) -> String {
    let mut out = String::new();
    match decision {
        Decision::Quiet => out.push_str("没有需要报告的事项。\n\n"),
        Decision::Recovered { previous } => {
            let names = previous
                .iter()
                .map(|metric| chinese_metric(*metric))
                .collect::<Vec<_>>()
                .join("、");
            out.push_str(&format!(
                "R2 用量已回到配置的阈值以下。\n\n已恢复：{names}\n\n"
            ));
        }
        Decision::Alert { reasons, repeated } => {
            if *repeated {
                out.push_str("这是重复提醒：告警状态未变化。\n\n");
            } else {
                out.push_str("至少一项 R2 指标已达到配置的阈值。\n\n");
            }
            for reason in reasons {
                let reading = &reason.reading;
                out.push_str(&format!(
                    "{}已使用免费额度的 {:.1}%（{}）",
                    chinese_metric(reading.metric),
                    reading.percent(),
                    chinese_status(reason.status)
                ));
                if reading.overage() > 0 {
                    out.push_str(&format!(
                        "；超出额度 {}",
                        humanize_metric(reading.metric, reading.overage())
                    ));
                } else {
                    out.push_str(&format!(
                        "；剩余 {}",
                        humanize_metric(reading.metric, reading.remaining())
                    ));
                }
                if reason.repeated {
                    out.push_str("［重复］");
                }
                out.push('\n');
            }
            out.push('\n');
        }
    }
    out.push_str(&format!("免费额度，查询周期 {period}\n"));
    for reading in &report.readings {
        out.push_str(&format!(
            "  {}：{} / {}，{:.1}%（{}）\n",
            chinese_metric(reading.metric),
            humanize_metric(reading.metric, reading.used),
            humanize_metric(reading.metric, reading.limit),
            reading.percent(),
            chinese_status(reading.status)
        ));
    }
    let storage = &report.storage;
    if storage.observed_days == 0 {
        out.push_str(&format!(
            "\n存储：当前 {}；没有带日期的读数，GB-month 未知。\n",
            humanize_bytes(storage.current_bytes)
        ));
    } else {
        out.push_str(&format!(
            "\n存储：当前 {}，日峰值平均 {}，{} 天累计 {}。\n",
            humanize_bytes(storage.current_bytes),
            humanize_bytes(storage.average_daily_peak),
            storage.observed_days,
            humanize_bytes(storage.accrued_month_bytes)
        ));
    }
    out.push_str(&format!(
        "阈值：警告 {:.0}%，严重 {:.0}%。\n",
        report.thresholds.warn * 100.0,
        report.thresholds.critical * 100.0
    ));
    out.push_str("\n-- \ncf-r2-monitor：Cloudflare R2 免费额度监控\n");
    out
}

fn humanize_metric(metric: Metric, value: u64) -> String {
    match metric {
        Metric::Storage => humanize_bytes(value),
        Metric::ClassA | Metric::ClassB => crate::output::group_digits(value),
    }
}

/// Short metric name for a subject line.
fn short(metric: Metric) -> &'static str {
    match metric {
        Metric::Storage => "storage",
        Metric::ClassA => "class A ops",
        Metric::ClassB => "class B ops",
    }
}

/// A local-part that is very unlikely to collide, built without a random
/// number generator so the message stays reproducible in tests.
fn unique_id(now: DateTime<Utc>) -> String {
    format!(
        "{}.{}.{}",
        now.timestamp(),
        std::process::id(),
        now.timestamp_subsec_nanos()
    )
}

/// Reject addresses that would break the header layout.
///
/// This is not an RFC 5322 validator; it only catches the mistakes that
/// actually happen (a bare name, a stray comma, a newline).
fn check_address(address: &str) -> Result<(), ComposeError> {
    let trimmed = address.trim();
    let usable = !trimmed.is_empty()
        && !trimmed.contains(['\n', '\r', ',', '<', '>', ' '])
        && trimmed.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
        });
    if usable {
        Ok(())
    } else {
        Err(ComposeError::BadAddress(trimmed.to_string()))
    }
}

/// How the mailer is invoked.
#[derive(Debug, Clone)]
pub struct Mailer {
    /// Executable name or path; `himalaya` by default.
    pub command: String,
    /// `himalaya -a <account>`.
    pub account: Option<String>,
    /// `himalaya -c <path>`.
    pub config: Option<PathBuf>,
}

/// How long the mailer may run before it is killed.
///
/// himalaya v1 retries a rejected `RCPT TO` in a tight loop and never exits:
/// against a sink answering `550` it logged 1,042,739 retries in three minutes
/// and was still going. Without a deadline that turns a mailserver hiccup into
/// a hung monitoring run, and every later run would queue behind it. Ninety
/// seconds is comfortably more than a healthy SMTP conversation and short
/// enough that the timer cannot overlap itself.
pub const SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("could not run {command}: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{command} exited with {status}: {stderr}")]
    Failed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("could not write the message to {command}: {source}")]
    Write {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{command} did not finish within {seconds}s and was killed: {stderr}")]
    Timeout {
        command: String,
        seconds: u64,
        stderr: String,
    },
}

/// The argv for one send, kept separate so it can be asserted in a test.
///
/// The flags go *after* `message send`, not before it. himalaya v2 marks
/// `--account` and `--config` global, so both positions work there, but v1 (the
/// version `nixpkgs` 26.05 ships) only defines `-a/--account` on the `message
/// send` subcommand: passing it first fails with
/// `error: unexpected argument '-a' found` and nothing is sent. Putting the
/// flags last is the one ordering both versions accept.
pub fn mailer_args(mailer: &Mailer) -> Vec<String> {
    let mut args = vec!["message".to_string(), "send".to_string()];
    if let Some(account) = &mailer.account {
        args.push("-a".to_string());
        args.push(account.clone());
    }
    if let Some(config) = &mailer.config {
        args.push("-c".to_string());
        args.push(config.display().to_string());
    }
    args
}

/// Whether a failed run nevertheless delivered the message.
///
/// himalaya v1 (the version in `nixpkgs` 26.05) sends the message and *then*
/// saves a copy to the `Sent` folder. With the default
/// `message.send.save-copy = true` and no maildir backend configured, the send
/// succeeds and the copy fails, so himalaya exits 1 after the mail has already
/// left. Reporting that as a failure would leave the state file unwritten and
/// mail the operator again on every single run, so this specific outcome is
/// downgraded to a warning.
///
/// The match is deliberately narrow: `cannot add message` is the copy step and
/// only the copy step. A rejected recipient, a bad password or an unreachable
/// server all produce different text and stay failures.
fn delivered_despite_error(stderr: &str) -> bool {
    stderr.contains("cannot add message")
}

/// Hand one message to the mailer.
pub async fn send(mailer: &Mailer, message: &str) -> Result<(), SendError> {
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new(&mailer.command)
        .args(mailer_args(mailer))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| SendError::Spawn {
            command: mailer.command.clone(),
            source,
        })?;

    // The message has to be written while the child is still running, so the
    // write and the wait cannot both own the child; take stdin out first.
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let payload = message.as_bytes().to_vec();
    let writer = tokio::spawn(async move {
        let result = stdin.write_all(&payload).await;
        // Dropping stdin closes the pipe, which is what tells himalaya the
        // message ended.
        drop(stdin);
        result
    });

    let collected = tokio::time::timeout(SEND_TIMEOUT, child.wait_with_output()).await;

    let output = match collected {
        Err(_elapsed) => {
            // `wait_with_output` owned the child, so it is already gone; the
            // writer task is detached and its error, if any, no longer matters.
            writer.abort();
            return Err(SendError::Timeout {
                command: mailer.command.clone(),
                seconds: SEND_TIMEOUT.as_secs(),
                stderr: String::new(),
            });
        }
        Ok(Err(source)) => {
            return Err(SendError::Spawn {
                command: mailer.command.clone(),
                source,
            });
        }
        Ok(Ok(output)) => output,
    };

    // A broken pipe here means the mailer gave up before reading everything,
    // which the exit status below already explains; a real error is reported.
    match writer.await {
        Ok(Ok(())) => {}
        Ok(Err(source)) if source.kind() == std::io::ErrorKind::BrokenPipe => {}
        Ok(Err(source)) => {
            return Err(SendError::Write {
                command: mailer.command.clone(),
                source,
            });
        }
        Err(_join) => {}
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        return Ok(());
    }
    if delivered_despite_error(&stderr) {
        tracing::warn!(
            "alerting: {} sent the message but could not file a copy ({stderr}); \
             set `message.send.save-copy = false` in its config to silence this",
            mailer.command
        );
        return Ok(());
    }
    Err(SendError::Failed {
        command: mailer.command.clone(),
        status: output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "a signal".to_string()),
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{OperationClass, OperationStat, R2Snapshot, StoragePoint};
    use crate::quota::Thresholds;
    use chrono::TimeZone;

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, 3, 0, 0).unwrap()
    }

    fn report(class_a: u64, class_b: u64) -> UsageReport {
        let snapshot = R2Snapshot {
            storage: vec![
                StoragePoint {
                    bucket_name: "assets".into(),
                    payload_size: 3_420_000_000,
                    metadata_size: 0,
                    object_count: 10,
                    upload_count: 0,
                    at: Some(at(1)),
                },
                StoragePoint {
                    bucket_name: "backups".into(),
                    payload_size: 1_200_000_000,
                    metadata_size: 0,
                    object_count: 4,
                    upload_count: 0,
                    at: Some(at(1)),
                },
            ],
            operations: vec![
                OperationStat {
                    action_type: "PutObject".into(),
                    requests: class_a,
                    class: OperationClass::ClassA,
                },
                OperationStat {
                    action_type: "GetObject".into(),
                    requests: class_b,
                    class: OperationClass::ClassB,
                },
            ],
            observed_at: None,
        };
        UsageReport::evaluate(snapshot, Thresholds::default())
    }

    fn envelope() -> Envelope {
        Envelope {
            to: vec!["ops@example.org".into()],
            from: "monitor@example.org".into(),
            host: "monitor.example.org".into(),
            period: "30d".into(),
        }
    }

    #[test]
    fn repeat_parses_units_and_zero() {
        assert_eq!(Repeat::parse("24h").unwrap().minutes, 1440);
        assert_eq!(Repeat::parse("2d").unwrap().minutes, 2880);
        assert_eq!(Repeat::parse("90m").unwrap().minutes, 90);
        assert_eq!(Repeat::parse("0").unwrap().minutes, 0);
        assert_eq!(Repeat::parse("0").unwrap().quiet_period(), None);
        assert_eq!(Repeat::parse("24").unwrap().minutes, 1440);
    }

    #[test]
    fn repeat_rejects_junk() {
        assert!(Repeat::parse("").is_err());
        assert!(Repeat::parse("5w").is_err());
        assert!(Repeat::parse("soon").is_err());
    }

    #[test]
    fn repeat_describes_itself() {
        assert_eq!(Repeat::parse("24h").unwrap().describe(), "1d");
        assert_eq!(Repeat::parse("90m").unwrap().describe(), "90m");
        assert_eq!(Repeat::parse("0").unwrap().describe(), "0");
    }

    #[test]
    fn a_first_breach_is_always_reported() {
        let report = report(900_000, 100);
        assert_eq!(report.status(), Status::Warn);
        let decision = decide(
            &report,
            &AlertState::default(),
            at(25),
            Repeat::parse("24h").unwrap(),
        );
        match decision {
            Decision::Alert { reasons, repeated } => {
                assert_eq!(reasons.len(), 1);
                assert_eq!(reasons[0].metric, Metric::ClassA);
                assert!(!repeated);
            }
            other => panic!("expected an alert, got {other:?}"),
        }
    }

    #[test]
    fn an_unchanged_breach_stays_quiet_until_the_period_expires() {
        let report = report(900_000, 100);
        let mut state = AlertState::default();
        let first = decide(&report, &state, at(25), Repeat::parse("24h").unwrap());
        remember(&mut state, &report, &first, at(25));

        let early = decide(
            &report,
            &state,
            at(25).checked_add_signed(Duration::hours(6)).unwrap(),
            Repeat::parse("24h").unwrap(),
        );
        assert_eq!(early, Decision::Quiet);

        let late = decide(&report, &state, at(26), Repeat::parse("24h").unwrap());
        match late {
            Decision::Alert { reasons, repeated } => {
                assert!(repeated, "expected a repeat");
                assert_eq!(reasons[0].metric, Metric::ClassA);
            }
            other => panic!("expected a repeat, got {other:?}"),
        }
    }

    #[test]
    fn an_escalation_is_reported_immediately() {
        let warn = report(900_000, 100);
        let critical = report(980_000, 100);
        let mut state = AlertState::default();
        let first = decide(&warn, &state, at(25), Repeat::parse("24h").unwrap());
        remember(&mut state, &warn, &first, at(25));

        let escalated = decide(
            &critical,
            &state,
            at(25).checked_add_signed(Duration::hours(1)).unwrap(),
            Repeat::parse("24h").unwrap(),
        );
        match escalated {
            Decision::Alert { reasons, repeated } => {
                assert_eq!(reasons[0].status, Status::Critical);
                assert!(!repeated, "an escalation is not a repeat");
            }
            other => panic!("expected an escalation, got {other:?}"),
        }
    }

    #[test]
    fn a_recovery_is_announced_once() {
        let bad = report(900_000, 100);
        let good = report(10, 10);
        let mut state = AlertState::default();
        let first = decide(&bad, &state, at(25), Repeat::parse("24h").unwrap());
        remember(&mut state, &bad, &first, at(25));

        let recovered = decide(&good, &state, at(26), Repeat::parse("24h").unwrap());
        assert_eq!(
            recovered,
            Decision::Recovered {
                previous: vec![Metric::ClassA]
            }
        );
        remember(&mut state, &good, &recovered, at(26));

        let after = decide(&good, &state, at(27), Repeat::parse("24h").unwrap());
        assert_eq!(after, Decision::Quiet);
    }

    #[test]
    fn repeat_zero_reports_every_run() {
        let report = report(900_000, 100);
        let mut state = AlertState::default();
        let every = Repeat::parse("0").unwrap();
        let first = decide(&report, &state, at(25), every);
        remember(&mut state, &report, &first, at(25));
        let second = decide(&report, &state, at(25), every);
        assert!(matches!(second, Decision::Alert { .. }));
    }

    #[test]
    fn a_quiet_report_does_not_touch_state() {
        let good = report(10, 10);
        let decision = decide(
            &good,
            &AlertState::default(),
            at(25),
            Repeat::parse("24h").unwrap(),
        );
        assert_eq!(decision, Decision::Quiet);
        assert!(!decision.needs_mail());
        assert!(!decision.touches_state());
    }

    #[test]
    fn the_message_has_the_headers_a_mailer_needs() {
        let report = report(980_000, 100);
        let decision = decide(
            &report,
            &AlertState::default(),
            at(25),
            Repeat::parse("24h").unwrap(),
        );
        let message = compose(&report, &decision, &envelope(), at(25)).unwrap();
        assert!(message.starts_with("From: monitor@example.org\n"));
        assert!(message.contains("\nTo: ops@example.org\n"));
        assert_eq!(
            decoded_subject(&message),
            "[r2-monitor] critical — class A ops"
        );
        assert!(message.contains("\nMessage-ID: <"));
        assert!(message.contains("\nContent-Type: text/plain; charset=utf-8\n"));
        // Blank line separates headers from the body.
        assert!(message.contains("\n\nAt least one R2 metric"));
        assert!(message.contains("Class A operations is at 98.0%"));
    }

    fn decoded_subject(message: &str) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let headers = message.split("\n\n").next().unwrap();
        let lines: Vec<_> = headers
            .lines()
            .skip_while(|line| !line.starts_with("Subject: "))
            .take_while(|line| line.starts_with("Subject: ") || line.starts_with(' '))
            .collect();
        assert!(lines.iter().all(|line| line.len() <= 76));
        let encoded = lines.join(" ");
        let mut bytes = Vec::new();
        for word in encoded
            .strip_prefix("Subject: ")
            .unwrap()
            .split_whitespace()
        {
            assert!(word.len() <= 75);
            let payload = word
                .strip_prefix("=?UTF-8?B?")
                .unwrap()
                .strip_suffix("?=")
                .unwrap();
            bytes.extend(STANDARD.decode(payload).unwrap());
        }
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn chinese_alert_and_recovery_are_localized() {
        let report = report(980_000, 100);
        let decision = decide(
            &report,
            &AlertState::default(),
            at(25),
            Repeat::parse("24h").unwrap(),
        );
        let message = compose_in_language(
            &report,
            &decision,
            &envelope(),
            at(25),
            Language::SimplifiedChinese,
        )
        .unwrap();
        assert!(decoded_subject(&message).contains("严重：A 类操作"));
        assert!(message.contains("至少一项 R2 指标"));
        assert!(message.contains("A 类操作已使用免费额度的 98.0%"));
        assert!(message.contains("阈值：警告 80%，严重 95%"));
        let recovery = Decision::Recovered {
            previous: vec![Metric::Storage, Metric::ClassB],
        };
        let message = compose_in_language(
            &report,
            &recovery,
            &envelope(),
            at(26),
            Language::SimplifiedChinese,
        )
        .unwrap();
        assert!(decoded_subject(&message).contains("已恢复"));
        assert!(message.contains("已恢复：存储、B 类操作"));
    }

    #[test]
    fn encoded_subject_handles_four_byte_characters() {
        let subject = "😀".repeat(30);
        let message = format!("Subject: {}\n\n", encode_subject(&subject));
        assert_eq!(decoded_subject(&message), subject);
    }

    #[test]
    fn the_recovery_message_says_so() {
        let good = report(10, 10);
        let decision = Decision::Recovered {
            previous: vec![Metric::ClassA],
        };
        let message = compose(&good, &decision, &envelope(), at(26)).unwrap();
        assert!(decoded_subject(&message).contains("[r2-monitor] recovered"));
        assert!(message.contains("Recovered: class A ops"));
    }

    #[test]
    fn a_message_without_recipients_is_refused() {
        let report = report(10, 10);
        let decision = Decision::Quiet;
        let mut envelope = envelope();
        envelope.to.clear();
        assert!(matches!(
            compose(&report, &decision, &envelope, at(25)),
            Err(ComposeError::NoRecipient)
        ));
    }

    #[test]
    fn a_bare_name_is_not_an_address() {
        assert!(check_address("ops").is_err());
        assert!(check_address("ops@localhost").is_err());
        assert!(check_address("ops@example.org").is_ok());
        assert!(check_address("a.b+tag@sub.example.co.uk").is_ok());
        assert!(check_address("ops@example.org\nBcc: evil@x.y").is_err());
    }

    #[test]
    fn a_newline_in_a_header_is_refused() {
        let report = report(10, 10);
        let decision = Decision::Quiet;
        let mut envelope = envelope();
        envelope.host = "evil\nBcc: x@y.z".into();
        assert!(matches!(
            compose(&report, &decision, &envelope, at(25)),
            Err(ComposeError::HeaderInjection(_))
        ));
    }

    #[test]
    fn mailer_args_shapes_the_himalaya_command() {
        let plain = Mailer {
            command: "himalaya".into(),
            account: None,
            config: None,
        };
        assert_eq!(mailer_args(&plain), ["message", "send"]);

        let full = Mailer {
            command: "/run/current-system/sw/bin/himalaya".into(),
            account: Some("cf".into()),
            config: Some(PathBuf::from("/run/secrets/himalaya.toml")),
        };
        assert_eq!(
            mailer_args(&full),
            [
                "message",
                "send",
                "-a",
                "cf",
                "-c",
                "/run/secrets/himalaya.toml"
            ]
        );
    }

    #[test]
    fn a_failed_sent_copy_is_not_treated_as_an_undelivered_message() {
        // Observed verbatim from himalaya v1.2.0 after it had already handed
        // the message to the SMTP server.
        assert!(delivered_despite_error(
            "Error: \n   0: cannot add message: feature not available, or backend \
             configuration for this functionality is not set"
        ));
    }

    #[test]
    fn real_delivery_failures_stay_failures() {
        // A rejected recipient: nothing was sent.
        assert!(!delivered_despite_error(
            "Error: \n   0: cannot send message: server replied with code 550"
        ));
        // No sender: nothing was sent.
        assert!(!delivered_despite_error(
            "Error: \n   0: cannot send message without a sender"
        ));
        // Missing config: nothing was sent.
        assert!(!delivered_despite_error(
            "Cannot find configuration at /etc/himalaya.toml."
        ));
    }

    /// A scratch directory for one test, removed again when it drops.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("cf-r2-alert-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            // Best effort: a leftover directory is untidy, not a test failure.
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn state_round_trips_through_a_file() {
        let dir = Scratch::new("round-trip");
        let path = dir.join("state.json");

        let mut state = AlertState::default();
        state.metrics.insert(
            "class_a".into(),
            Remembered {
                status: Status::Warn,
                sent_at: at(25),
            },
        );
        state.save(&path).unwrap();
        assert_eq!(AlertState::load(&path).unwrap(), state);

        std::fs::remove_file(&path).unwrap();
        assert_eq!(AlertState::load(&path).unwrap(), AlertState::default());
    }

    #[test]
    fn a_corrupt_state_file_is_an_error_not_a_reset() {
        let dir = Scratch::new("corrupt");
        let path = dir.join("state.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            AlertState::load(&path),
            Err(StateError::Parse { .. })
        ));
    }
}

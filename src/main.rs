//! Entry point: parse arguments, fetch a snapshot, print a verdict.

use std::process::ExitCode;

use chrono::Utc;
use clap::FromArgMatches;
use cloudflare_r2_monitor::alert::{self, AlertState, Decision, Envelope, Mailer};
use cloudflare_r2_monitor::cloudflare::{Client, Credentials, Window};
use cloudflare_r2_monitor::{
    cli::{self, Args},
    i18n, output,
    quota::{Thresholds, UsageReport},
};
use tracing_subscriber::EnvFilter;

/// Exit code used when the check itself failed.
///
/// Kept apart from the status codes because clap would otherwise exit with 2,
/// making a malformed command line indistinguishable from a critical overage.
const EXIT_ERROR: u8 = 3;

/// Exit code used when the usage was read but the alert could not be
/// delivered.
///
/// Distinct from [`EXIT_ERROR`] because the reading itself is trustworthy: a
/// supervisor should page someone about a broken mail path, not about a quota
/// breach, and must not treat it as "everything is fine" either.
const EXIT_ALERT_FAILED: u8 = 4;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("CF_R2_MONITOR_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .without_time()
        .init();

    match run().await {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}

async fn run() -> Result<ExitCode, String> {
    // The help text follows the shell locale, so the command is built here
    // rather than parsed straight from the derive.
    let language = i18n::Language::from_env();

    // Parse by hand so a usage error exits 3 rather than clap's default 2,
    // which would collide with the "critical" status code.
    let matches = match cli::command(language).try_get_matches() {
        Ok(matches) => matches,
        Err(error) => {
            use clap::error::ErrorKind;
            let _ = error.print();
            return Ok(match error.kind() {
                // Help and version are successful invocations.
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => ExitCode::SUCCESS,
                _ => ExitCode::from(EXIT_ERROR),
            });
        }
    };
    let args = match Args::from_arg_matches(&matches) {
        Ok(args) => args,
        Err(error) => {
            let _ = error.print();
            return Ok(ExitCode::from(EXIT_ERROR));
        }
    };

    args.validate().map_err(str::to_string)?;

    let thresholds = Thresholds::new(args.warn, args.critical)
        .map_err(|e| format!("invalid thresholds: {e}"))?;

    let account_id = args
        .account_id
        .clone()
        .ok_or("missing --account-id (or CLOUDFLARE_ACCOUNT_ID)")?;
    let api_token = args
        .token()
        .map_err(|e| e.to_string())?
        .ok_or("missing --api-token-file, --api-token or CLOUDFLARE_API_TOKEN")?;

    let client = match &args.endpoint {
        Some(endpoint) => Client::with_endpoint(
            Credentials {
                api_token,
                account_id,
            },
            endpoint,
        ),
        None => Client::new(Credentials {
            api_token,
            account_id,
        }),
    }
    .map_err(|e| format!("could not build the HTTP client: {e}"))?;

    let window = Window::trailing_days(args.period.days());
    let snapshot = client
        .snapshot(window)
        .await
        .map_err(|e| format!("could not read R2 usage from Cloudflare: {e}"))?;

    let report = UsageReport::evaluate(snapshot, thresholds);

    if args.json {
        println!(
            "{}",
            output::json(&report).map_err(|e| format!("could not serialise the report: {e}"))?
        );
    } else {
        print!(
            "{}",
            output::table(&report, &args.period.describe(), language)
        );
    }

    tracing::debug!("{}", output::summary(&report));

    let status_code = output::exit_code(report.status()) as u8;
    let alert_result = notify(&args, &report, language).await;

    match alert_result {
        AlertOutcome::Skipped => Ok(ExitCode::from(status_code)),
        AlertOutcome::Delivered => Ok(ExitCode::from(status_code)),
        AlertOutcome::Failed(message) => {
            eprintln!("error: {message}");
            Ok(ExitCode::from(EXIT_ALERT_FAILED))
        }
    }
}

/// What happened on the alerting side.
enum AlertOutcome {
    /// No `--alert-to`, or nothing worth mailing.
    Skipped,
    /// The mailer accepted the message.
    Delivered,
    /// The message was built but not delivered.
    Failed(String),
}

/// Decide whether to mail and, if so, mail.
async fn notify(args: &Args, report: &UsageReport, language: i18n::Language) -> AlertOutcome {
    if args.alert_to.is_empty() {
        return AlertOutcome::Skipped;
    }

    let now = Utc::now();
    let mut state = match &args.alert_state {
        Some(path) => match AlertState::load(path) {
            Ok(state) => state,
            Err(error) => return AlertOutcome::Failed(error.to_string()),
        },
        None => AlertState::default(),
    };

    let decision = alert::decide(report, &state, now, args.alert_repeat);
    if !decision.needs_mail() {
        tracing::debug!("alerting: nothing to send");
        return AlertOutcome::Skipped;
    }
    // A recovery is a courtesy; the operator asked not to be told.
    if args.alert_no_recovery && matches!(decision, Decision::Recovered { .. }) {
        alert::remember(&mut state, report, &decision, now);
        return persist(args, &state);
    }

    // `Args::validate` already rejects --alert-to without --alert-from, so this
    // is only reachable if validation is bypassed; report it rather than panic.
    let Some(from) = args.alert_from.clone() else {
        return AlertOutcome::Failed(
            "alerting needs --alert-from: himalaya takes the envelope sender from the From header"
                .to_string(),
        );
    };

    let envelope = Envelope {
        to: args.alert_to.clone(),
        from,
        host: hostname(),
        period: args.period.describe(),
    };

    // The flag wins; without it the mail follows the shell locale, the same
    // choice the table already made.
    let mail_language = args.alert_language.unwrap_or(language);
    let message = match alert::compose_in_language(report, &decision, &envelope, now, mail_language)
    {
        Ok(message) => message,
        Err(error) => return AlertOutcome::Failed(error.to_string()),
    };

    if args.alert_dry_run {
        // A dry run must not consume the quiet period, otherwise the first
        // real run after a rehearsal would stay silent.
        print!("{message}");
        return AlertOutcome::Delivered;
    }

    let mailer = Mailer {
        command: args.alert_command.clone(),
        account: args.alert_account.clone(),
        config: args.alert_config.clone(),
    };

    if let Err(error) = alert::send(&mailer, &message).await {
        return AlertOutcome::Failed(error.to_string());
    }

    alert::remember(&mut state, report, &decision, now);
    persist(args, &state)
}

/// Write the state file, downgrading a failure to a note.
///
/// Failing to remember is not worth turning a delivered alert into an error:
/// the worst case is one repeat on the next run, and failing the whole run
/// would page someone about the ledger rather than about their quota.
fn persist(args: &Args, state: &AlertState) -> AlertOutcome {
    let Some(path) = &args.alert_state else {
        return AlertOutcome::Delivered;
    };
    if let Err(error) = state.save(path) {
        tracing::warn!("alerting: could not record the alert: {error}");
    }
    AlertOutcome::Delivered
}

/// The hostname used in `Message-ID`, or a placeholder when it cannot be read.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudflare_r2_monitor::quota::Status;

    #[test]
    fn error_exit_codes_are_distinct_from_every_status() {
        // 3 and 4 must never collide with ok/warn/critical, otherwise a failed
        // check would look like a quota breach.
        for status in [Status::Ok, Status::Warn, Status::Critical] {
            assert_ne!(output::exit_code(status) as u8, EXIT_ERROR);
            assert_ne!(output::exit_code(status) as u8, EXIT_ALERT_FAILED);
        }
        assert_ne!(EXIT_ERROR, EXIT_ALERT_FAILED);
    }
}

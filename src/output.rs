//! Rendering a [`UsageReport`] as text or JSON.

use crate::i18n::Language;
use crate::quota::{
    humanize_bytes, Metric, MetricReading, Status, UsageReport, FREE_STORAGE_BYTES,
};

/// Render the report as an aligned text table in `language`.
pub fn table(report: &UsageReport, period: &str, language: Language) -> String {
    let mut out = String::new();
    out.push_str(&match language {
        Language::English => format!("Cloudflare R2 free-tier usage — period {period}\n"),
        Language::SimplifiedChinese => format!("Cloudflare R2 免费额度用量（周期 {period}）\n"),
    });

    let headers = match language {
        Language::English => ["METRIC", "USED", "LIMIT", "USED%", "STATUS"],
        Language::SimplifiedChinese => ["指标", "已用量", "免费额度", "占比", "状态"],
    };
    out.push_str(&format!(
        "  {}{}{}{}   {}\n",
        pad_right(headers[0], 20),
        pad_left(headers[1], 14),
        pad_left(headers[2], 14),
        pad_left(headers[3], 9),
        headers[4]
    ));

    for reading in &report.readings {
        out.push_str(&format!(
            "  {}{:>14}{:>14}{:>8.1}%   {}\n",
            pad_right(metric_label(reading.metric, language), 20),
            humandize(reading.metric, reading.used),
            humandize(reading.metric, reading.limit),
            reading.percent(),
            status_label(reading.status, language),
        ));
    }

    out.push('\n');
    out.push_str(&storage_line(report, language));
    out.push_str(&verdict_line(report, language));

    out
}

/// The closing line: the worst reading, or confirmation that nothing crossed
/// the warning threshold.
fn verdict_line(report: &UsageReport, language: Language) -> String {
    let Some(worst) = report.worst() else {
        return match language {
            Language::English => format!(
                "\nOK: every metric is below the {:.0}% warning threshold\n",
                report.thresholds.warn * 100.0
            ),
            Language::SimplifiedChinese => format!(
                "\n正常：所有指标都低于 {:.0}% 的警告阈值\n",
                report.thresholds.warn * 100.0
            ),
        };
    };
    let mut out = match language {
        Language::English => format!(
            "\n{}: {} is at {:.1}% of the free tier",
            status_label(worst.status, language).to_uppercase(),
            metric_label(worst.metric, language),
            worst.percent()
        ),
        Language::SimplifiedChinese => format!(
            "\n{}：{} 已占用免费额度的 {:.1}%",
            status_label(worst.status, language),
            metric_label(worst.metric, language),
            worst.percent()
        ),
    };
    if worst.overage() > 0 {
        let amount = humandize(worst.metric, worst.overage());
        out.push_str(&match language {
            Language::English => format!(" — past the limit by {amount}"),
            Language::SimplifiedChinese => format!("，超出 {amount}"),
        });
    } else {
        let amount = humandize(worst.metric, worst.remaining());
        out.push_str(&match language {
            Language::English => format!(", {amount} left"),
            Language::SimplifiedChinese => format!("，还剩 {amount}"),
        });
    }
    out.push('\n');
    out
}

/// The one line that explains why the storage percentage is not the whole story.
fn storage_line(report: &UsageReport, language: Language) -> String {
    let detail = &report.storage;
    if detail.observed_days == 0 {
        return match language {
            Language::English => format!(
                "  storage: {} stored now; no dated readings, GB-month unknown\n",
                humanize_bytes(detail.current_bytes)
            ),
            Language::SimplifiedChinese => format!(
                "  存储：当前占用 {}；没有带日期的读数，GB-month 无法计算\n",
                humanize_bytes(detail.current_bytes)
            ),
        };
    }
    let mut out = match language {
        Language::English => format!(
            "  storage: {} now, {} daily peak avg, {} accrued over {} day(s)\n",
            humanize_bytes(detail.current_bytes),
            humanize_bytes(detail.average_daily_peak),
            humanize_bytes(detail.accrued_month_bytes),
            detail.observed_days,
        ),
        Language::SimplifiedChinese => format!(
            "  存储：当前 {}，日均峰值 {}，{} 天累计 {}\n",
            humanize_bytes(detail.current_bytes),
            humanize_bytes(detail.average_daily_peak),
            detail.observed_days,
            humanize_bytes(detail.accrued_month_bytes),
        ),
    };
    if detail.stale_buckets > 0 {
        // Say this out loud: a bucket vanishing from the total without a word
        // looks like a bug, and the reader needs to know the bill still counts
        // the days it existed.
        out.push_str(&match language {
            Language::English => format!(
                "           ({} bucket(s) deleted during the window: excluded from {} now, still counted in the average)\n",
                detail.stale_buckets,
                humanize_bytes(detail.current_bytes),
            ),
            Language::SimplifiedChinese => format!(
                "           （窗口内有 {} 个桶已删除：不计入当前 {}，仍计入日均峰值）\n",
                detail.stale_buckets,
                humanize_bytes(detail.current_bytes),
            ),
        });
    }
    out
}

/// Name of a metric in the table.
fn metric_label(metric: Metric, language: Language) -> &'static str {
    match (language, metric) {
        (Language::English, Metric::Storage) => "Storage",
        (Language::English, Metric::ClassA) => "Class A operations",
        (Language::English, Metric::ClassB) => "Class B operations",
        (Language::SimplifiedChinese, Metric::Storage) => "存储",
        (Language::SimplifiedChinese, Metric::ClassA) => "Class A 操作",
        (Language::SimplifiedChinese, Metric::ClassB) => "Class B 操作",
    }
}

/// Severity word for the table and the closing line.
fn status_label(status: Status, language: Language) -> &'static str {
    match (language, status) {
        (Language::English, Status::Ok) => "ok",
        (Language::English, Status::Warn) => "warn",
        (Language::English, Status::Critical) => "critical",
        (Language::SimplifiedChinese, Status::Ok) => "正常",
        (Language::SimplifiedChinese, Status::Warn) => "警告",
        (Language::SimplifiedChinese, Status::Critical) => "严重",
    }
}

/// Width of `text` in terminal columns.
///
/// `format!` counts characters, so a Chinese label would be padded as if it
/// were half as wide as it renders. East Asian wide characters count as two.
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| match ch as u32 {
            0x1100..=0x115F
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE6F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD => 2,
            _ => 1,
        })
        .sum()
}

/// Pad `text` on the right to `width` terminal columns.
fn pad_right(text: &str, width: usize) -> String {
    let mut out = String::from(text);
    out.push_str(&" ".repeat(width.saturating_sub(display_width(text))));
    out
}

/// Pad `text` on the left to `width` terminal columns.
fn pad_left(text: &str, width: usize) -> String {
    format!(
        "{}{text}",
        " ".repeat(width.saturating_sub(display_width(text)))
    )
}

/// Render the report as pretty-printed JSON.
pub fn json(report: &UsageReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

/// Format a value in the unit its metric is measured in.
///
/// Storage becomes a decimal GB figure because that is the unit Cloudflare
/// bills in; operations get thousands separators.
pub fn humandize(metric: Metric, value: u64) -> String {
    match metric {
        Metric::Storage => humanize_bytes(value),
        Metric::ClassA | Metric::ClassB => group_digits(value),
    }
}

/// Format a byte count using decimal units (`GB`, not `GiB`).
pub fn format_bytes(bytes: u64) -> String {
    humanize_bytes(bytes)
}

/// Insert `,` as a thousands separator.
pub fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// One-line summary for logs, suitable for a systemd journal.
pub fn summary(report: &UsageReport) -> String {
    let parts: Vec<String> = report
        .readings
        .iter()
        .map(|r| {
            format!(
                "{} {:.1}% {}",
                short(r.metric),
                r.percent(),
                r.status.label()
            )
        })
        .collect();
    format!("r2-usage {} {}", report.status().label(), parts.join(" "))
}

fn short(metric: Metric) -> &'static str {
    match metric {
        Metric::Storage => "storage",
        Metric::ClassA => "classA",
        Metric::ClassB => "classB",
    }
}

/// The process exit code that matches a status.
///
/// `0` ok, `1` warn, `2` critical. Runtime failures use `3` and are handled by
/// the caller; keeping them distinct lets a supervisor tell "over quota" apart
/// from "could not check".
pub fn exit_code(status: Status) -> i32 {
    match status {
        Status::Ok => 0,
        Status::Warn => 1,
        Status::Critical => 2,
    }
}

/// Whether a reading deserves a line in a CI or journal annotation.
pub fn needs_attention(reading: &MetricReading) -> bool {
    reading.status != Status::Ok
}

/// The free storage allowance in bytes, re-exported for callers that print it.
pub fn free_storage_bytes() -> u64 {
    FREE_STORAGE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{OperationClass, OperationStat, R2Snapshot, StoragePoint};
    use crate::quota::Thresholds;

    fn at(day: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 11, day, 3, 0, 0).unwrap()
    }

    /// Three buckets over a single day: a realistic "just ran it" report.
    fn report() -> UsageReport {
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
                    bucket_name: "assets".into(),
                    payload_size: 3_420_000_000,
                    metadata_size: 0,
                    object_count: 11,
                    upload_count: 0,
                    at: Some(at(2)),
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
                    requests: 412_880,
                    class: OperationClass::ClassA,
                },
                OperationStat {
                    action_type: "GetObject".into(),
                    requests: 1_204_551,
                    class: OperationClass::ClassB,
                },
            ],
            observed_at: None,
        };
        UsageReport::evaluate(snapshot, Thresholds::default())
    }

    #[test]
    fn bytes_use_decimal_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(999), "999 B");
        assert_eq!(format_bytes(1_000), "1.00 KB");
        assert_eq!(format_bytes(3_420_000_000), "3.42 GB");
        assert_eq!(format_bytes(10_000_000_000), "10.00 GB");
    }

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_000), "1,000");
        assert_eq!(group_digits(1_204_551), "1,204,551");
        assert_eq!(group_digits(10_000_000), "10,000,000");
    }

    #[test]
    fn table_lists_every_metric() {
        let text = table(&report(), "30d", Language::English);
        assert!(text.contains("Storage"));
        assert!(text.contains("Class A operations"));
        assert!(text.contains("Class B operations"));
        assert!(text.contains("4.62 GB"));
        assert!(text.contains("412,880"));
        assert!(text.contains("1,204,551"));
        assert!(text.contains("30d"));
    }

    #[test]
    fn table_shows_the_latest_storage_not_the_sum_of_history() {
        // The two `assets` rows both say 3.42 GB; the total must not be 6.84 GB.
        let text = table(&report(), "30d", Language::English);
        assert!(text.contains("4.62 GB"), "got:\n{text}");
        assert!(!text.contains("8.04 GB"), "got:\n{text}");
    }

    #[test]
    fn table_reports_an_ok_state() {
        let text = table(&report(), "30d", Language::English);
        assert!(text.contains("OK:"));
    }

    #[test]
    fn table_explains_both_storage_figures() {
        let text = table(&report(), "30d", Language::English);
        assert!(text.contains("now"), "got:\n{text}");
        assert!(text.contains("daily peak avg"), "got:\n{text}");
        assert!(text.contains("over 2 day(s)"), "got:\n{text}");
    }

    #[test]
    fn a_two_day_window_reports_a_small_accrual() {
        // Two days of a 4.62 GB average accrues 0.31 GB-month, not 4.62.
        let text = table(&report(), "30d", Language::English);
        assert!(text.contains("308.00 MB"), "got:\n{text}");
    }

    #[test]
    fn a_long_window_reports_the_full_average_as_accrued() {
        let snapshot = R2Snapshot {
            storage: (1..=30)
                .map(|day| StoragePoint {
                    bucket_name: "b".into(),
                    payload_size: 1_000_000_000,
                    metadata_size: 0,
                    object_count: 0,
                    upload_count: 0,
                    at: Some(at(day)),
                })
                .collect(),
            ..Default::default()
        };
        let report = UsageReport::evaluate(snapshot, Thresholds::default());
        assert_eq!(report.storage.accrued_month_bytes, 1_000_000_000);
        let text = table(&report, "30d", Language::English);
        assert!(
            text.contains("1.00 GB accrued over 30 day(s)"),
            "got:\n{text}"
        );
    }

    #[test]
    fn table_calls_out_the_worst_metric() {
        let mut snapshot = report().snapshot;
        snapshot.operations[0].requests = 990_000;
        let report = UsageReport::evaluate(snapshot, Thresholds::default());
        let text = table(&report, "30d", Language::English);
        assert!(text.contains("CRITICAL:"));
        assert!(text.contains("Class A operations"));
    }

    #[test]
    fn the_chinese_table_translates_every_label() {
        let text = table(&report(), "30d", Language::SimplifiedChinese);
        assert!(text.contains("存储"), "got:\n{text}");
        assert!(text.contains("Class A 操作"), "got:\n{text}");
        assert!(text.contains("日均峰值"), "got:\n{text}");
        assert!(text.contains("正常："), "got:\n{text}");
        assert!(!text.contains("OK:"), "got:\n{text}");
        assert!(!text.contains("daily peak avg"), "got:\n{text}");
    }

    #[test]
    fn the_chinese_verdict_names_the_worst_metric() {
        let mut snapshot = report().snapshot;
        snapshot.operations[0].requests = 990_000;
        let report = UsageReport::evaluate(snapshot, Thresholds::default());
        let text = table(&report, "30d", Language::SimplifiedChinese);
        assert!(text.contains("严重："), "got:\n{text}");
        assert!(text.contains("已占用免费额度的"), "got:\n{text}");
    }

    #[test]
    fn chinese_labels_are_padded_by_display_width() {
        // A Chinese character occupies two terminal columns; counting chars
        // would leave the numeric columns half as far right as intended.
        assert_eq!(display_width("存储"), 4);
        assert_eq!(display_width(&pad_right("存储", 20)), 20);
        assert_eq!(display_width(&pad_right("Class A 操作", 20)), 20);
        assert_eq!(display_width(&pad_left("已用量", 14)), 14);
        assert_eq!(pad_right("Class A operations", 20), "Class A operations  ");
    }

    #[test]
    fn the_chinese_table_mentions_a_deleted_bucket() {
        let mut storage: Vec<StoragePoint> = (1..=10)
            .map(|day| StoragePoint {
                bucket_name: "assets".into(),
                payload_size: 1_000_000_000,
                metadata_size: 0,
                object_count: 0,
                upload_count: 0,
                at: Some(at(day)),
            })
            .collect();
        storage.extend((1..=5).map(|day| StoragePoint {
            bucket_name: "backups".into(),
            payload_size: 2_000_000_000,
            metadata_size: 0,
            object_count: 0,
            upload_count: 0,
            at: Some(at(day)),
        }));
        let report = UsageReport::evaluate(
            R2Snapshot {
                storage,
                ..Default::default()
            },
            Thresholds::default(),
        );
        let text = table(&report, "30d", Language::SimplifiedChinese);
        assert!(text.contains("个桶已删除"), "got:\n{text}");
    }
    #[test]
    fn json_round_trips_and_uses_snake_case() {
        let text = json(&report()).unwrap();
        assert!(text.contains("\"class_a\""));
        assert!(text.contains("\"storage\""));
        assert!(text.contains("\"warn\""));
        assert!(text.contains("\"ratio\""));
        assert!(text.contains("\"average_daily_peak\""));
        assert!(text.contains("\"accrued_month_bytes\""));
        assert!(text.contains("\"payload_size\""));
        let back: UsageReport = serde_json::from_str(&text).unwrap();
        assert_eq!(back.status(), Status::Ok);
    }

    #[test]
    fn a_deleted_bucket_is_mentioned_rather_than_silently_dropped() {
        // assets lives all ten days; backups is deleted after day five.
        let mut storage: Vec<StoragePoint> = (1..=10)
            .map(|day| StoragePoint {
                bucket_name: "assets".into(),
                payload_size: 1_000_000_000,
                metadata_size: 0,
                object_count: 0,
                upload_count: 0,
                at: Some(at(day)),
            })
            .collect();
        storage.extend((1..=5).map(|day| StoragePoint {
            bucket_name: "backups".into(),
            payload_size: 2_000_000_000,
            metadata_size: 0,
            object_count: 0,
            upload_count: 0,
            at: Some(at(day)),
        }));
        let report = UsageReport::evaluate(
            R2Snapshot {
                storage,
                ..Default::default()
            },
            Thresholds::default(),
        );
        let text = table(&report, "30d", Language::English);
        assert!(
            text.contains("1 bucket(s) deleted during the window"),
            "got:\n{text}"
        );
        // Only `assets` counts as stored now.
        assert!(text.contains("1.00 GB now"), "got:\n{text}");
    }

    #[test]
    fn a_window_without_deletions_says_nothing_about_them() {
        let text = table(&report(), "30d", Language::English);
        assert!(!text.contains("deleted during the window"), "got:\n{text}");
    }

    #[test]
    fn exit_codes_match_severity() {
        assert_eq!(exit_code(Status::Ok), 0);
        assert_eq!(exit_code(Status::Warn), 1);
        assert_eq!(exit_code(Status::Critical), 2);
    }

    #[test]
    fn summary_is_one_line() {
        let text = summary(&report());
        assert!(!text.contains('\n'));
        assert!(text.starts_with("r2-usage ok"));
        assert!(text.contains("storage 46.2%"));
    }

    #[test]
    fn only_non_ok_readings_need_attention() {
        let report = report();
        for reading in &report.readings {
            assert_eq!(
                needs_attention(reading),
                report.status() != Status::Ok && reading.status != Status::Ok
            );
        }
    }
}

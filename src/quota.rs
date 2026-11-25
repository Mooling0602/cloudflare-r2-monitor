//! Free-tier limits, thresholds and the evaluation that turns a [`R2Snapshot`]
//! into a verdict.

use serde::{Deserialize, Serialize};

use crate::model::{Bytes, OperationClass, R2Snapshot};

/// Cloudflare's published R2 free tier.
///
/// Storage is stated as `10 GB-month`. GB here is the decimal gigabyte that
/// Cloudflare bills in, so the limit is `10 * 10^9` bytes and not `10 * 2^30`.
pub const FREE_STORAGE_BYTES: Bytes = 10_000_000_000;
/// Free Class A operations per month.
pub const FREE_CLASS_A_OPS: u64 = 1_000_000;
/// Free Class B operations per month.
pub const FREE_CLASS_B_OPS: u64 = 10_000_000;

/// Days in the billing month used for projections.
pub const BILLING_DAYS: u32 = 30;

/// Which metric a line of the report refers to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Storage,
    ClassA,
    ClassB,
}

impl Metric {
    pub fn label(self) -> &'static str {
        match self {
            Self::Storage => "Storage",
            Self::ClassA => "Class A operations",
            Self::ClassB => "Class B operations",
        }
    }

    /// The limit this metric is measured against, in its own unit.
    pub fn limit(self) -> u64 {
        match self {
            Self::Storage => FREE_STORAGE_BYTES,
            Self::ClassA => FREE_CLASS_A_OPS,
            Self::ClassB => FREE_CLASS_B_OPS,
        }
    }
}

/// Severity of a single metric.
///
/// Ordered so that `max` of several statuses is the worst one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Below the warn threshold.
    Ok,
    /// At or above the warn threshold, below the critical one.
    Warn,
    /// At or above the critical threshold.
    Critical,
}

impl Status {
    /// Map a usage ratio onto a status.
    ///
    /// Both thresholds are inclusive lower bounds: a ratio exactly equal to
    /// `warn` is a warning.
    pub fn from_ratio(ratio: f64, warn: f64, critical: f64) -> Self {
        if ratio >= critical {
            Self::Critical
        } else if ratio >= warn {
            Self::Warn
        } else {
            Self::Ok
        }
    }

    /// Short label for tables.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

/// Warn and critical thresholds expressed as fractions of the free tier.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Thresholds {
    pub warn: f64,
    pub critical: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            warn: 0.80,
            critical: 0.95,
        }
    }
}

impl Thresholds {
    /// Build a pair of thresholds, rejecting values that cannot order.
    pub fn new(warn: f64, critical: f64) -> Result<Self, ThresholdError> {
        if !(warn.is_finite() && critical.is_finite()) {
            return Err(ThresholdError::NotFinite);
        }
        if warn <= 0.0 || critical <= 0.0 {
            return Err(ThresholdError::NonPositive);
        }
        if warn > 1.0 || critical > 1.0 {
            return Err(ThresholdError::AboveOne);
        }
        if warn > critical {
            return Err(ThresholdError::WarnAboveCritical);
        }
        Ok(Self { warn, critical })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ThresholdError {
    #[error("thresholds must be finite")]
    NotFinite,
    #[error("thresholds must be greater than zero")]
    NonPositive,
    #[error("thresholds above 1.0 would never trigger")]
    AboveOne,
    #[error("warn threshold must not exceed the critical threshold")]
    WarnAboveCritical,
}

/// One evaluated metric.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricReading {
    pub metric: Metric,
    /// Observed value in the metric's own unit.
    pub used: u64,
    /// The free-tier limit in the same unit.
    pub limit: u64,
    /// `used / limit`, unclamped so an overage shows above 1.0.
    pub ratio: f64,
    pub status: Status,
}

impl MetricReading {
    pub fn new(metric: Metric, used: u64, thresholds: Thresholds) -> Self {
        let limit = metric.limit();
        let ratio = used as f64 / limit as f64;
        Self {
            metric,
            used,
            limit,
            ratio,
            status: Status::from_ratio(ratio, thresholds.warn, thresholds.critical),
        }
    }

    /// Percentage of the free tier in use.
    pub fn percent(&self) -> f64 {
        self.ratio * 100.0
    }

    /// Headroom left, saturating at zero once the limit is exceeded.
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    /// How far past the limit the usage is, saturating at zero.
    pub fn overage(&self) -> u64 {
        self.used.saturating_sub(self.limit)
    }
}

/// What the storage figures mean, beyond the single instantaneous number.
///
/// R2 does not bill the bytes you happen to be storing when the check runs. It
/// records each day's peak, averages those peaks over the billing period, and
/// bills that average in gigabyte-months. Three numbers are therefore needed to
/// describe one month honestly:
///
/// * `current_bytes` — what is on disk right now.
/// * `average_daily_peak` — the observed average of daily peaks. If usage holds
///   at this level for the rest of the month, this *is* the GB-month bill, so
///   comparing it against the free allowance is the check that predicts a
///   charge.
/// * `accrued_month_bytes` — the GB-month actually accrued so far, which is the
///   average prorated to the days elapsed. This is what has already been spent
///   against the allowance, and it is never larger than the monthly average.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StorageDetail {
    /// Bytes stored at the most recent reading, across every bucket.
    pub current_bytes: Bytes,
    /// Average of the daily peaks over the observed window.
    pub average_daily_peak: Bytes,
    /// `average_daily_peak` prorated to the days observed, in GB-month.
    pub accrued_month_bytes: Bytes,
    /// How many distinct days the window actually covered.
    pub observed_days: usize,
    /// Buckets that stopped being reported during the window, normally because
    /// they were deleted. They are excluded from `current_bytes` but still
    /// count towards `average_daily_peak`: the days they existed were billed.
    pub stale_buckets: usize,
}

impl StorageDetail {
    /// Derive the storage context from a snapshot.
    pub fn from_snapshot(snapshot: &R2Snapshot) -> Self {
        let current_bytes = snapshot.total_stored_bytes();
        let average_daily_peak = snapshot.average_daily_peak();
        let observed_days = snapshot.covered_days();
        let stale_buckets = snapshot.stale_buckets().len();

        // Prorate the average to the days observed. An average of X bytes held
        // for a third of a month accrues a third of an X GB-month bill; treating
        // the average itself as the accrual would make the first day of the
        // month look like a full month of usage.
        //
        // The day count is capped at BILLING_DAYS because a window can cover
        // more distinct dates than the billing period has: a 30-day window
        // starting at 21:45 still admits a reading bucket dated the previous
        // day, so it can span 31 dates. Without the cap the accrual would come
        // out larger than the monthly average it is a fraction of.
        let accrued_month_bytes = if observed_days == 0 {
            0
        } else {
            let days = observed_days.min(BILLING_DAYS as usize) as u128;
            (average_daily_peak as u128 * days / BILLING_DAYS as u128) as Bytes
        };

        Self {
            current_bytes,
            average_daily_peak,
            accrued_month_bytes,
            observed_days,
            stale_buckets,
        }
    }

    /// The figure to judge the free allowance against.
    ///
    /// Both halves matter. A bucket that is 12 GB right now is over the
    /// allowance even if the month's average is still low, and a bucket that
    /// shrank to 2 GB yesterday is still over it if the average across the
    /// month so far is 15 GB.
    pub fn billable_bytes(&self) -> Bytes {
        self.current_bytes.max(self.average_daily_peak)
    }

    /// Whether the window was too short for the average to mean much.
    pub fn is_window_weak(&self) -> bool {
        self.observed_days < 7
    }
}

/// The full result of one evaluation, together with what it was computed from.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageReport {
    pub readings: Vec<MetricReading>,
    pub thresholds: Thresholds,
    pub storage: StorageDetail,
    pub snapshot: R2Snapshot,
}

impl UsageReport {
    /// Evaluate every metric.
    ///
    /// The storage reading is judged against [`StorageDetail::billable_bytes`],
    /// which is the larger of the current footprint and the observed average of
    /// daily peaks. Using only the instantaneous figure would hide a bucket that
    /// spent most of the month over the allowance and was cleaned up this
    /// morning; using only the average would hide a spike that has not yet been
    /// averaged away. Over a steady month the two coincide.
    pub fn evaluate(snapshot: R2Snapshot, thresholds: Thresholds) -> Self {
        let storage = StorageDetail::from_snapshot(&snapshot);
        let readings = vec![
            MetricReading::new(Metric::Storage, storage.billable_bytes(), thresholds),
            MetricReading::new(
                Metric::ClassA,
                snapshot.operations_of(OperationClass::ClassA),
                thresholds,
            ),
            MetricReading::new(
                Metric::ClassB,
                snapshot.operations_of(OperationClass::ClassB),
                thresholds,
            ),
        ];
        Self {
            readings,
            thresholds,
            storage,
            snapshot,
        }
    }

    /// The worst status across all metrics.
    pub fn status(&self) -> Status {
        self.readings
            .iter()
            .map(|r| r.status)
            .max()
            .unwrap_or(Status::Ok)
    }

    /// The single worst metric, if any metric is at least a warning.
    pub fn worst(&self) -> Option<&MetricReading> {
        self.readings
            .iter()
            .filter(|r| r.status != Status::Ok)
            .max_by(|a, b| a.status.cmp(&b.status).then(a.ratio.total_cmp(&b.ratio)))
    }

    /// Look up one metric's reading.
    pub fn reading(&self, metric: Metric) -> Option<&MetricReading> {
        self.readings.iter().find(|r| r.metric == metric)
    }

    /// Plain-language caveat about the storage figure.
    pub fn storage_note(&self) -> String {
        let detail = &self.storage;
        if detail.observed_days == 0 {
            return format!(
                "Storage checks the {} on disk against the {:.0} GB allowance. No dated \
                 storage rows came back, so the GB-month figure that actually bills could \
                 not be computed.",
                humanize_bytes(detail.current_bytes),
                FREE_STORAGE_BYTES as f64 / 1e9,
            );
        }

        let mut note = format!(
            "Storage is judged on {} — the larger of the {} on disk now and the {} average \
             of daily peaks. R2 bills that average as GB-month, so {} sustained for a full \
             {BILLING_DAYS}-day month costs {} GB-month, of which {} has accrued over the \
             {} day(s) observed.",
            humanize_bytes(detail.billable_bytes()),
            humanize_bytes(detail.current_bytes),
            humanize_bytes(detail.average_daily_peak),
            humanize_bytes(detail.average_daily_peak),
            humanize_bytes(detail.average_daily_peak),
            humanize_bytes(detail.accrued_month_bytes),
            detail.observed_days,
        );
        if detail.is_window_weak() {
            note.push_str(
                " That window is shorter than a week, so the average is a rough indication \
                 rather than a forecast.",
            );
        }
        note
    }
}

/// Format bytes in decimal units, matching how Cloudflare quotes R2.
pub fn humanize_bytes(bytes: Bytes) -> String {
    const UNITS: [(&str, f64); 4] = [("TB", 1e12), ("GB", 1e9), ("MB", 1e6), ("KB", 1e3)];
    if bytes == 0 {
        return "0 B".to_string();
    }
    for (unit, scale) in UNITS {
        if bytes as f64 >= scale {
            return format!("{:.2} {unit}", bytes as f64 / scale);
        }
    }
    format!("{bytes} B")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{OperationStat, StoragePoint};

    fn thresholds() -> Thresholds {
        Thresholds::default()
    }

    fn at(offset: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 11, 1, 3, 0, 0).unwrap()
            + chrono::Duration::days(offset as i64)
    }

    fn dated(bucket: &str, payload: u64, offset: u32) -> StoragePoint {
        StoragePoint {
            bucket_name: bucket.into(),
            payload_size: payload,
            metadata_size: 0,
            object_count: 0,
            upload_count: 0,
            at: Some(at(offset)),
        }
    }

    /// A bucket whose size followed `schedule` (`(first_day, bytes)`, applied in
    /// order) across `days` days.
    fn series(schedule: &[(u32, u64)], days: u32) -> R2Snapshot {
        let storage = (0..days)
            .map(|day| {
                let size = schedule
                    .iter()
                    .rev()
                    .find(|(from, _)| *from <= day)
                    .map(|(_, bytes)| *bytes)
                    .unwrap_or(0);
                dated("b", size, day)
            })
            .collect();
        R2Snapshot {
            storage,
            ..Default::default()
        }
    }

    /// A single dated storage reading, i.e. a one-day observation.
    fn snapshot_with(storage: u64, class_a: u64, class_b: u64) -> R2Snapshot {
        R2Snapshot {
            storage: vec![StoragePoint {
                bucket_name: "b".into(),
                payload_size: storage,
                metadata_size: 0,
                object_count: 0,
                upload_count: 0,
                at: Some(chrono::Utc::now()),
            }],
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
        }
    }

    #[test]
    fn free_tier_limits_are_decimal_gigabytes() {
        assert_eq!(FREE_STORAGE_BYTES, 10_000_000_000);
        assert_eq!(FREE_CLASS_A_OPS, 1_000_000);
        assert_eq!(FREE_CLASS_B_OPS, 10_000_000);
    }

    #[test]
    fn low_usage_is_ok() {
        let report = UsageReport::evaluate(snapshot_with(1_000_000_000, 100, 100), thresholds());
        assert_eq!(report.status(), Status::Ok);
        assert!(report.worst().is_none());
    }

    #[test]
    fn eighty_percent_is_a_warning() {
        let report = UsageReport::evaluate(snapshot_with(0, 800_000, 0), thresholds());
        let reading = report.reading(Metric::ClassA).unwrap();
        assert_eq!(reading.status, Status::Warn);
        assert_eq!(reading.percent(), 80.0);
    }

    #[test]
    fn exactly_eighty_percent_warns_because_thresholds_are_inclusive() {
        let report = UsageReport::evaluate(snapshot_with(8_000_000_000, 0, 0), thresholds());
        assert_eq!(
            report.reading(Metric::Storage).unwrap().status,
            Status::Warn
        );
    }

    #[test]
    fn ninety_five_percent_is_critical() {
        let report = UsageReport::evaluate(snapshot_with(0, 950_000, 0), thresholds());
        assert_eq!(
            report.reading(Metric::ClassA).unwrap().status,
            Status::Critical
        );
        assert_eq!(report.status(), Status::Critical);
    }

    #[test]
    fn overage_reports_above_one_hundred_percent() {
        let report = UsageReport::evaluate(snapshot_with(0, 1_500_000, 0), thresholds());
        let reading = report.reading(Metric::ClassA).unwrap();
        assert_eq!(reading.percent(), 150.0);
        assert_eq!(reading.overage(), 500_000);
        assert_eq!(reading.remaining(), 0);
    }

    #[test]
    fn worst_metric_is_the_one_deepest_into_its_limit() {
        let report = UsageReport::evaluate(snapshot_with(9_000_000_000, 990_000, 0), thresholds());
        let worst = report.worst().unwrap();
        assert_eq!(worst.metric, Metric::ClassA);
    }

    #[test]
    fn storage_is_judged_on_the_larger_of_now_and_the_month_average() {
        // A steady 9 GB over ten days: both figures agree, so the verdict is
        // unambiguous.
        let report = UsageReport::evaluate(series(&[(0, 9_000_000_000)], 10), thresholds());
        let reading = report.reading(Metric::Storage).unwrap();
        assert_eq!(reading.used, 9_000_000_000);
        assert_eq!(reading.status, Status::Warn);
    }

    #[test]
    fn a_big_month_average_still_warns_after_a_cleanup() {
        // 15 GB for nine days, then a clean-up to 1 GB today: the footprint is
        // tiny but the month's billable average is ~13.6 GB, which is over.
        let mut points = Vec::new();
        for day in 0..9 {
            points.push(dated("b", 15_000_000_000, day));
        }
        points.push(dated("b", 1_000_000_000, 9));
        let snapshot = R2Snapshot {
            storage: points,
            ..Default::default()
        };
        let report = UsageReport::evaluate(snapshot, thresholds());
        let reading = report.reading(Metric::Storage).unwrap();
        assert_eq!(report.storage.current_bytes, 1_000_000_000);
        assert_eq!(reading.used, report.storage.average_daily_peak);
        assert!(reading.used > 13_000_000_000);
        assert_eq!(reading.status, Status::Critical);
    }

    #[test]
    fn a_sudden_spike_is_judged_even_before_it_moves_the_average() {
        // Nine quiet days at 1 GB and then 20 GB today: the average is still
        // low, but 20 GB is sitting on disk right now.
        let mut points = Vec::new();
        for day in 0..9 {
            points.push(dated("b", 1_000_000_000, day));
        }
        points.push(dated("b", 20_000_000_000, 9));
        let snapshot = R2Snapshot {
            storage: points,
            ..Default::default()
        };
        let report = UsageReport::evaluate(snapshot, thresholds());
        assert_eq!(
            report.reading(Metric::Storage).unwrap().used,
            20_000_000_000
        );
        assert_eq!(
            report.reading(Metric::Storage).unwrap().status,
            Status::Critical
        );
    }

    #[test]
    fn accrued_gb_month_never_exceeds_the_average() {
        let report = UsageReport::evaluate(series(&[(0, 3_000_000_000)], 10), thresholds());
        let detail = &report.storage;
        assert_eq!(detail.average_daily_peak, 3_000_000_000);
        // Ten days of a 3 GB average accrues 1 GB-month, not 3.
        assert_eq!(detail.accrued_month_bytes, 1_000_000_000);
        assert!(detail.accrued_month_bytes <= detail.average_daily_peak);
    }

    #[test]
    fn a_full_month_accrues_exactly_the_average() {
        let report = UsageReport::evaluate(series(&[(0, 1_000_000_000)], 30), thresholds());
        let detail = &report.storage;
        assert_eq!(detail.observed_days, 30);
        assert_eq!(detail.average_daily_peak, 1_000_000_000);
        assert_eq!(detail.accrued_month_bytes, 1_000_000_000);
        assert!(!detail.is_window_weak());
    }

    #[test]
    fn a_window_spanning_an_extra_date_does_not_over_accrue() {
        // A 30-day window does not align to midnight: starting at 21:45 still
        // admits a reading dated the previous day, so 31 distinct dates can be
        // seen. The accrual must stay a fraction of the monthly average.
        let report = UsageReport::evaluate(series(&[(0, 1_000_000_000)], 31), thresholds());
        let detail = &report.storage;
        assert_eq!(detail.observed_days, 31);
        assert_eq!(detail.average_daily_peak, 1_000_000_000);
        assert_eq!(detail.accrued_month_bytes, 1_000_000_000);
    }

    #[test]
    fn a_short_window_is_flagged_as_weak() {
        let report = UsageReport::evaluate(snapshot_with(1_000_000_000, 0, 0), thresholds());
        assert!(report.storage.is_window_weak());
        assert!(report.storage_note().contains("shorter than a week"));
    }

    #[test]
    fn a_long_window_is_not_flagged_as_weak() {
        let report = UsageReport::evaluate(series(&[(0, 1_000_000_000)], 10), thresholds());
        assert!(!report.storage.is_window_weak());
        assert!(!report.storage_note().contains("shorter than a week"));
    }

    #[test]
    fn the_note_explains_both_storage_figures() {
        let report = UsageReport::evaluate(series(&[(0, 3_000_000_000)], 10), thresholds());
        let note = report.storage_note();
        assert!(note.contains("3.00 GB"), "{note}");
        assert!(note.contains("daily peaks"), "{note}");
        assert!(note.contains("GB-month"), "{note}");
    }

    #[test]
    fn undated_storage_leaves_the_average_at_zero_and_says_so() {
        let snapshot = R2Snapshot {
            storage: vec![StoragePoint {
                bucket_name: "b".into(),
                payload_size: 5_000_000_000,
                metadata_size: 0,
                object_count: 0,
                upload_count: 0,
                at: None,
            }],
            ..Default::default()
        };
        let report = UsageReport::evaluate(snapshot, thresholds());
        assert_eq!(report.storage.observed_days, 0);
        assert_eq!(report.storage.average_daily_peak, 0);
        assert!(report.storage_note().contains("could not be computed"));
        // The verdict still comes from the current footprint.
        assert_eq!(report.reading(Metric::Storage).unwrap().used, 5_000_000_000);
    }

    #[test]
    fn thresholds_reject_bad_order_and_ranges() {
        assert_eq!(
            Thresholds::new(0.9, 0.8),
            Err(ThresholdError::WarnAboveCritical)
        );
        assert_eq!(Thresholds::new(0.0, 0.9), Err(ThresholdError::NonPositive));
        assert_eq!(Thresholds::new(0.8, 1.5), Err(ThresholdError::AboveOne));
        assert_eq!(
            Thresholds::new(f64::NAN, 0.9),
            Err(ThresholdError::NotFinite)
        );
        assert!(Thresholds::new(0.8, 0.95).is_ok());
    }

    #[test]
    fn report_serialises_to_json() {
        let report = UsageReport::evaluate(snapshot_with(1, 2, 3), thresholds());
        let json = serde_json::to_string(&report).unwrap();
        let back: UsageReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }

    #[test]
    fn bytes_are_formatted_decimally() {
        assert_eq!(humanize_bytes(0), "0 B");
        assert_eq!(humanize_bytes(999), "999 B");
        assert_eq!(humanize_bytes(3_420_000_000), "3.42 GB");
    }
}

//! Data model shared by the GraphQL client, the evaluator and the reporters.
//!
//! Everything here is pure data: no I/O, no network, no Cloudflare specifics
//! beyond the field names that the GraphQL dataset returns.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// A byte count. Used for storage figures so the unit is never ambiguous.
pub type Bytes = u64;

/// How far behind the account's newest storage reading a bucket may fall
/// before it is treated as deleted.
///
/// Cloudflare reports storage per day and samples every live bucket in the same
/// pass, so live buckets sit within minutes of each other while a deleted
/// bucket's last row is a full day older. Thirty-six hours is one sample period
/// plus generous slack: long enough that a day with a late or missing sample
/// does not drop the bucket — under-reporting storage is the expensive mistake
/// — and short enough that a deleted bucket stops inflating the total on the
/// next run.
pub const STALE_AFTER: Duration = Duration::hours(36);

/// The R2 datasets Cloudflare exposes, flattened into one snapshot.
///
/// Storage and operations come from different GraphQL datasets
/// (`r2StorageAdaptiveGroups` vs `r2OperationsAdaptiveGroups`), so they are
/// fetched separately and merged here.
///
/// Storage is kept as a time series rather than a single number because
/// Cloudflare bills storage as the average of the daily peaks over the billing
/// period. Collapsing the series to the latest reading would understate a month
/// that started large and shrank, and summing the rows would overstate almost
/// everything.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct R2Snapshot {
    /// Storage readings, one per bucket per time bucket Cloudflare returned.
    pub storage: Vec<StoragePoint>,
    /// Operations performed during the billing period, by action type.
    pub operations: Vec<OperationStat>,
    /// When Cloudflare produced the underlying data. `None` means unknown.
    pub observed_at: Option<DateTime<Utc>>,
}

impl R2Snapshot {
    /// Keep only the most recent reading for each bucket.
    ///
    /// This includes buckets that have since been deleted: their last reading
    /// stays in the window until the window itself slides past it. Use
    /// [`Self::active_buckets`] for the "what is stored right now" question and
    /// this one when you need every bucket the window ever mentioned.
    pub fn latest_per_bucket(&self) -> Vec<StoragePoint> {
        let mut latest: BTreeMap<&str, &StoragePoint> = BTreeMap::new();
        for point in &self.storage {
            latest
                .entry(point.bucket_name.as_str())
                .and_modify(|current| {
                    if point.sort_key() > current.sort_key() {
                        *current = point;
                    }
                })
                .or_insert(point);
        }
        latest.into_values().cloned().collect()
    }

    /// The newest reading timestamp across every bucket, when any is dated.
    fn newest_reading(&self) -> Option<DateTime<Utc>> {
        self.storage.iter().filter_map(|p| p.at).max()
    }

    /// Buckets that are still being reported, with their latest reading.
    ///
    /// A deleted bucket keeps producing rows until the query window slides past
    /// it, and its final row is a perfectly ordinary non-zero reading. Taking
    /// the newest row per bucket therefore reports deleted buckets as though
    /// they were still on disk, so the totals never fall after a delete.
    ///
    /// A bucket counts as gone when its latest reading is more than
    /// [`STALE_AFTER`] behind the newest reading in the account. Cloudflare
    /// samples storage roughly once a day and samples every live bucket at the
    /// same time, so a live bucket is normally zero to a few minutes behind
    /// while a deleted one is a whole sample period behind. The margin absorbs
    /// jitter without letting a deleted bucket linger.
    ///
    /// Readings without a timestamp cannot be judged, so they are treated as
    /// current: under-reporting storage is the one mistake that lets a free
    /// tier quietly turn into a bill.
    pub fn active_buckets(&self) -> Vec<StoragePoint> {
        let newest = self.newest_reading();
        let cutoff = newest.and_then(|newest| newest.checked_sub_signed(STALE_AFTER));
        self.latest_per_bucket()
            .into_iter()
            .filter(|point| match (point.at, cutoff) {
                (Some(at), Some(cutoff)) => at >= cutoff,
                _ => true,
            })
            .collect()
    }

    /// Buckets that stopped being reported and are no longer counted as stored.
    ///
    /// These are almost always deleted buckets. They still matter for billing,
    /// because the days they did exist were charged, and
    /// [`Self::average_daily_peak`] keeps them for exactly that reason.
    pub fn stale_buckets(&self) -> Vec<StoragePoint> {
        let newest = self.newest_reading();
        let cutoff = newest.and_then(|newest| newest.checked_sub_signed(STALE_AFTER));
        self.latest_per_bucket()
            .into_iter()
            .filter(|point| match (point.at, cutoff) {
                (Some(at), Some(cutoff)) => at < cutoff,
                _ => false,
            })
            .collect()
    }

    /// Payload bytes stored right now, across every bucket still on disk.
    pub fn total_payload_bytes(&self) -> Bytes {
        self.active_buckets().iter().map(|b| b.payload_size).sum()
    }

    /// Metadata bytes stored right now, across every bucket still on disk.
    pub fn total_metadata_bytes(&self) -> Bytes {
        self.active_buckets().iter().map(|b| b.metadata_size).sum()
    }

    /// Payload plus metadata currently on disk.
    pub fn total_stored_bytes(&self) -> Bytes {
        self.total_payload_bytes() + self.total_metadata_bytes()
    }

    /// Objects stored right now, across every bucket still on disk.
    pub fn total_objects(&self) -> u64 {
        self.active_buckets().iter().map(|b| b.object_count).sum()
    }

    /// The average of the daily peaks across every bucket.
    ///
    /// This is the number that decides the bill. Cloudflare meters R2 storage
    /// in gigabyte-months by recording each day's peak and averaging those
    /// peaks over the billing period, so an average of `X` bytes sustained for
    /// a whole month costs `X` GB-month. A bucket that held 1 GB for five days
    /// and 3 GB for twenty-five days averages
    /// `(1*5 + 3*25) / 30 = 2.67 GB`, which is exactly the 2.67 GB-month the
    /// pricing page quotes for that pattern.
    ///
    /// The average is taken over the days actually observed, so it answers
    /// "what would the month bill if usage stayed at this level". Use
    /// [`R2Snapshot::covered_days`] to judge how much that extrapolation is
    /// worth.
    pub fn average_daily_peak(&self) -> Bytes {
        let by_bucket = self.daily_peaks();
        if by_bucket.is_empty() {
            return 0;
        }
        // Average each bucket over its own observed days, then add the buckets
        // up. Summing first and dividing by a shared day count would let a
        // bucket that only appeared yesterday drag every other bucket's average
        // down toward zero.
        let total: f64 = by_bucket
            .values()
            .map(|peaks| {
                let days = peaks.len() as f64;
                let sum: u64 = peaks.values().map(|p| p.total_bytes()).sum();
                sum as f64 / days
            })
            .sum();
        total.round() as Bytes
    }

    /// Peak stored bytes per bucket per calendar day (UTC).
    pub fn daily_peaks(&self) -> BTreeMap<String, BTreeMap<NaiveDate, StoragePoint>> {
        let mut out: BTreeMap<String, BTreeMap<NaiveDate, StoragePoint>> = BTreeMap::new();
        for point in &self.storage {
            let Some(date) = point.date() else { continue };
            let entry = out
                .entry(point.bucket_name.clone())
                .or_default()
                .entry(date)
                .or_insert_with(|| point.clone());
            if point.total_bytes() > entry.total_bytes() {
                *entry = point.clone();
            }
        }
        out
    }

    /// How many distinct days the storage series covers.
    pub fn covered_days(&self) -> usize {
        self.storage
            .iter()
            .filter_map(|p| p.date())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// Request count for one operation class during the billing period.
    pub fn operations_of(&self, class: OperationClass) -> u64 {
        self.operations
            .iter()
            .filter(|op| op.class == class)
            .map(|op| op.requests)
            .sum()
    }
}

/// One bucket's storage at one point in time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoragePoint {
    /// Bucket name as Cloudflare reports it. Buckets with a jurisdiction carry
    /// a `<jurisdiction>_` prefix, for example `eu_my-bucket`.
    pub bucket_name: String,
    pub payload_size: Bytes,
    pub metadata_size: Bytes,
    pub object_count: u64,
    /// Pending multipart uploads. These are billed but hold no complete object.
    pub upload_count: u64,
    /// The time bucket this reading represents, when Cloudflare reported one.
    pub at: Option<DateTime<Utc>>,
}

impl StoragePoint {
    pub fn total_bytes(&self) -> Bytes {
        self.payload_size + self.metadata_size
    }

    /// The UTC calendar day, used to find daily peaks.
    pub fn date(&self) -> Option<NaiveDate> {
        self.at.map(|t| t.date_naive())
    }

    /// Ordering key that keeps undated readings at the front.
    fn sort_key(&self) -> (bool, Option<DateTime<Utc>>) {
        (self.at.is_some(), self.at)
    }
}

/// One row of the operations dataset, reduced to what billing cares about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationStat {
    /// Cloudflare's `actionType`, for example `PutObject` or `GetObject`.
    pub action_type: String,
    pub requests: u64,
    pub class: OperationClass,
}

/// The two billable operation classes.
///
/// Deletes and `AbortMultipartUpload` belong to neither class and are free, so
/// they are represented by [`OperationClass::Free`] rather than being dropped:
/// keeping them makes the totals auditable against the dashboard.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum OperationClass {
    ClassA,
    ClassB,
    Free,
}

impl OperationClass {
    /// Classify a Cloudflare `actionType`.
    ///
    /// The two published lists are authoritative and finite, so anything not
    /// named there is treated as free. Cloudflare's own guidance is the
    /// opposite (unknown operations are billed as Class A); callers that want
    /// the conservative behaviour should use [`Self::classify_strict`].
    pub fn classify(action_type: &str) -> Self {
        match action_type {
            // Class A: mutation and listing.
            "ListBuckets"
            | "PutBucket"
            | "ListObjects"
            | "PutObject"
            | "CopyObject"
            | "CompleteMultipartUpload"
            | "CreateMultipartUpload"
            | "LifecycleStorageTierTransition"
            | "ListMultipartUploads"
            | "UploadPart"
            | "UploadPartCopy"
            | "ListParts"
            | "PutBucketEncryption"
            | "PutBucketCors"
            | "PutBucketLifecycleConfiguration" => Self::ClassA,

            // Class B: reads and metadata lookups.
            "HeadBucket"
            | "HeadObject"
            | "GetObject"
            | "UsageSummary"
            | "GetBucketEncryption"
            | "GetBucketLocation"
            | "GetBucketCors"
            | "GetBucketLifecycleConfiguration" => Self::ClassB,

            // Explicitly free operations.
            "DeleteObject"
            | "DeleteBucket"
            | "AbortMultipartUpload"
            | "DeleteBucketCors"
            | "DeleteBucketEncryption"
            | "DeleteBucketLifecycleConfiguration" => Self::Free,

            _ => Self::Free,
        }
    }

    /// Classify, but bill unrecognised operations as Class A.
    ///
    /// This mirrors the convention used by existing R2 budget tooling: an
    /// unknown operation is assumed to be the expensive kind, so a monitor
    /// built on it never under-reports.
    pub fn classify_strict(action_type: &str) -> Self {
        match Self::classify(action_type) {
            Self::Free if !Self::is_known_free(action_type) => Self::ClassA,
            other => other,
        }
    }

    /// Whether the action type appears in the published free list.
    pub fn is_known_free(action_type: &str) -> bool {
        matches!(
            action_type,
            "DeleteObject"
                | "DeleteBucket"
                | "AbortMultipartUpload"
                | "DeleteBucketCors"
                | "DeleteBucketEncryption"
                | "DeleteBucketLifecycleConfiguration"
        )
    }

    /// Human-readable label used in reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::ClassA => "Class A",
            Self::ClassB => "Class B",
            Self::Free => "Free",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(day: u32) -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 11, day, 3, 0, 0).unwrap()
    }

    fn point(bucket: &str, payload: u64, day: u32) -> StoragePoint {
        StoragePoint {
            bucket_name: bucket.into(),
            payload_size: payload,
            metadata_size: 0,
            object_count: payload / 1000,
            upload_count: 0,
            at: Some(at(day)),
        }
    }

    /// A reading on day `offset` of November 2026, counting from the 1st.
    fn dated(bucket: &str, payload: u64, offset: u32) -> StoragePoint {
        point(bucket, payload, offset + 1)
    }

    fn snapshot() -> R2Snapshot {
        R2Snapshot {
            storage: vec![
                // bucket `assets` grows: 1 GB then 2 GB, over two days.
                point("assets", 1_000_000_000, 1),
                point("assets", 2_000_000_000, 2),
                // bucket `eu_backups` is flat at 500 MB for two days.
                point("eu_backups", 500_000_000, 1),
                point("eu_backups", 500_000_000, 2),
            ],
            operations: vec![
                OperationStat {
                    action_type: "PutObject".into(),
                    requests: 400_000,
                    class: OperationClass::ClassA,
                },
                OperationStat {
                    action_type: "GetObject".into(),
                    requests: 3_000_000,
                    class: OperationClass::ClassB,
                },
                OperationStat {
                    action_type: "DeleteObject".into(),
                    requests: 50_000,
                    class: OperationClass::Free,
                },
            ],
            observed_at: None,
        }
    }

    #[test]
    fn latest_reading_per_bucket_is_the_current_state() {
        let s = snapshot();
        let latest = s.latest_per_bucket();
        assert_eq!(latest.len(), 2);
        let assets = latest.iter().find(|p| p.bucket_name == "assets").unwrap();
        assert_eq!(assets.payload_size, 2_000_000_000);
    }

    #[test]
    fn totals_use_the_latest_reading_not_the_sum_of_history() {
        let s = snapshot();
        // 2 GB + 500 MB, not the 4 GB that summing every row would give.
        assert_eq!(s.total_payload_bytes(), 2_500_000_000);
        assert_eq!(s.total_stored_bytes(), 2_500_000_000);
    }

    #[test]
    fn billed_storage_is_the_average_of_daily_peaks() {
        let s = snapshot();
        // assets: (1 GB + 2 GB) / 2 days = 1.5 GB
        // eu_backups: (500 MB + 500 MB) / 2 days = 500 MB
        assert_eq!(s.average_daily_peak(), 2_000_000_000);
    }

    #[test]
    fn the_pricing_page_example_is_reproduced() {
        // 1 GB held for 5 days then 3 GB held for 25 days bills 2.66 GB-month
        // per Cloudflare's own pricing page, computed as (1*5 + 3*25) / 30.
        let mut storage = Vec::new();
        for day in 0..5 {
            storage.push(dated("b", 1_000_000_000, day));
        }
        for day in 5..30 {
            storage.push(dated("b", 3_000_000_000, day));
        }
        let s = R2Snapshot {
            storage,
            ..Default::default()
        };
        assert_eq!(s.covered_days(), 30);
        // (1*5 + 3*25) / 30 = 2.6667 GB
        assert_eq!(s.average_daily_peak(), 2_666_666_667);
    }

    #[test]
    fn a_bucket_that_only_appeared_today_does_not_dilute_the_others() {
        // `old` has held 9 GB for ten days; `new` appeared today with 1 GB.
        // Averaging everything over a shared 10-day window would report 9.1 GB
        // instead of 10 GB and quietly hide an overage.
        let mut storage = Vec::new();
        for day in 0..10 {
            storage.push(dated("old", 9_000_000_000, day));
        }
        storage.push(dated("new", 1_000_000_000, 9));
        let s = R2Snapshot {
            storage,
            ..Default::default()
        };
        assert_eq!(s.average_daily_peak(), 10_000_000_000);
    }

    #[test]
    fn a_deleted_bucket_stops_counting_towards_storage_now() {
        // `assets` is still there on day 25; `backups` was deleted after day 10.
        // Its final row is a normal 1 GB reading, so keeping the newest row per
        // bucket made the total stay at 2 GB forever — the bug where deleting a
        // bucket never lowered the reported storage.
        let mut storage = Vec::new();
        for offset in 0..25 {
            storage.push(dated("assets", 1_000_000_000, offset));
            if offset <= 9 {
                storage.push(dated("backups", 1_000_000_000, offset));
            }
        }
        let s = R2Snapshot {
            storage,
            ..Default::default()
        };
        assert_eq!(s.total_stored_bytes(), 1_000_000_000);
        assert_eq!(s.active_buckets().len(), 1);
        assert_eq!(s.stale_buckets().len(), 1);
        assert_eq!(s.stale_buckets()[0].bucket_name, "backups");
        // Billing still counts the ten days `backups` existed:
        // assets 1 GB + backups (1 GB * 10/10 observed days) = 2 GB.
        assert_eq!(s.average_daily_peak(), 2_000_000_000);
    }

    #[test]
    fn a_bucket_that_missed_one_sample_is_not_treated_as_deleted() {
        // Under-reporting storage is the expensive mistake, so a single late or
        // missing sample must not drop a bucket out of the current total.
        let s = R2Snapshot {
            storage: vec![
                point("assets", 1_000_000_000, 1),
                point("assets", 1_000_000_000, 2),
                point("slow", 700_000_000, 1),
            ],
            ..Default::default()
        };
        assert_eq!(s.active_buckets().len(), 2);
        assert_eq!(s.total_stored_bytes(), 1_700_000_000);
    }

    #[test]
    fn same_day_readings_collapse_to_the_peak() {
        let s = R2Snapshot {
            storage: vec![point("b", 100, 1), point("b", 900, 1), point("b", 500, 1)],
            ..Default::default()
        };
        assert_eq!(s.average_daily_peak(), 900);
        assert_eq!(s.latest_per_bucket().len(), 1);
        assert_eq!(s.covered_days(), 1);
    }

    #[test]
    fn undated_readings_do_not_crash_the_maths() {
        let s = R2Snapshot {
            storage: vec![StoragePoint {
                bucket_name: "b".into(),
                payload_size: 42,
                metadata_size: 0,
                object_count: 1,
                upload_count: 0,
                at: None,
            }],
            ..Default::default()
        };
        // Undated rows cannot be averaged into a day, so billing is zero…
        assert_eq!(s.average_daily_peak(), 0);
        // …but the current reading is still usable.
        assert_eq!(s.total_stored_bytes(), 42);
    }

    #[test]
    fn empty_snapshot_is_all_zero() {
        let s = R2Snapshot::default();
        assert_eq!(s.total_stored_bytes(), 0);
        assert_eq!(s.average_daily_peak(), 0);
        assert_eq!(s.operations_of(OperationClass::ClassA), 0);
        assert!(s.latest_per_bucket().is_empty());
    }

    #[test]
    fn operation_classes_are_summed_separately() {
        let s = snapshot();
        assert_eq!(s.operations_of(OperationClass::ClassA), 400_000);
        assert_eq!(s.operations_of(OperationClass::ClassB), 3_000_000);
        assert_eq!(s.operations_of(OperationClass::Free), 50_000);
    }

    #[test]
    fn published_operations_classify_to_the_right_class() {
        assert_eq!(
            OperationClass::classify("PutObject"),
            OperationClass::ClassA
        );
        assert_eq!(
            OperationClass::classify("GetObject"),
            OperationClass::ClassB
        );
        assert_eq!(
            OperationClass::classify("DeleteObject"),
            OperationClass::Free
        );
    }

    #[test]
    fn unknown_operation_is_class_a_when_strict() {
        assert_eq!(
            OperationClass::classify("SomeFutureOp"),
            OperationClass::Free
        );
        assert_eq!(
            OperationClass::classify_strict("SomeFutureOp"),
            OperationClass::ClassA
        );
        // A known-free operation must not be promoted.
        assert_eq!(
            OperationClass::classify_strict("DeleteObject"),
            OperationClass::Free
        );
    }
}

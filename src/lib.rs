//! Cloudflare R2 usage monitoring against the free tier.

pub mod alert;
pub mod cli;
pub mod cloudflare;
pub mod model;
pub mod output;
pub mod quota;

pub use alert::{AlertState, Decision, Envelope, Mailer, Reason, Repeat};
pub use model::{Bytes, OperationClass, OperationStat, R2Snapshot, StoragePoint};
pub use quota::{
    Metric, MetricReading, Status, ThresholdError, Thresholds, UsageReport, FREE_CLASS_A_OPS,
    FREE_CLASS_B_OPS, FREE_STORAGE_BYTES,
};

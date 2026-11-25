//! Cloudflare GraphQL Analytics client.
//!
//! Two datasets are involved, and Cloudflare keeps them separate:
//!
//! * `r2StorageAdaptiveGroups` — how much is stored, per bucket.
//! * `r2OperationsAdaptiveGroups` — how many requests were served, per action.
//!
//! Both are queried through `https://api.cloudflare.com/client/v4/graphql` with
//! an account-scoped API token. The token needs exactly one permission:
//! *Account → Account Analytics → Read*. Nothing here writes to Cloudflare.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{OperationClass, OperationStat, R2Snapshot, StoragePoint};

/// Default GraphQL endpoint. Tests inject another one.
pub const DEFAULT_ENDPOINT: &str = "https://api.cloudflare.com/client/v4/graphql";

/// Cloudflare rejects queries spanning more than 31 days.
pub const MAX_WINDOW_DAYS: i64 = 31;

/// Storage rows are grouped per bucket and per time bucket, so the caller can
/// reconstruct the daily peaks that R2 bills against. Asking for the `datetime`
/// dimension is what makes the billing average computable at all.
const STORAGE_QUERY: &str = r#"
query R2Storage($accountTag: string, $start: Time, $end: Time) {
  viewer {
    accounts(filter: { accountTag: $accountTag }) {
      r2StorageAdaptiveGroups(
        limit: 10000
        filter: { datetime_geq: $start, datetime_leq: $end }
        orderBy: [datetime_DESC]
      ) {
        max { payloadSize metadataSize objectCount uploadCount }
        dimensions { bucketName datetime }
      }
    }
  }
}
"#;

/// Operations are grouped by action type so the caller can classify them.
const OPERATIONS_QUERY: &str = r#"
query R2Operations($accountTag: string, $start: Time, $end: Time) {
  viewer {
    accounts(filter: { accountTag: $accountTag }) {
      r2OperationsAdaptiveGroups(
        limit: 10000
        filter: { datetime_geq: $start, datetime_leq: $end }
      ) {
        sum { requests }
        dimensions { actionType }
      }
    }
  }
}
"#;

/// A window of time to report on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Window {
    /// A window ending now and reaching back `days` days.
    ///
    /// Clamped to [`MAX_WINDOW_DAYS`]; Cloudflare would reject anything wider.
    pub fn trailing_days(days: u32) -> Self {
        let days = days.clamp(1, MAX_WINDOW_DAYS as u32);
        let end = Utc::now();
        Self {
            start: end - chrono::Duration::days(i64::from(days)),
            end,
        }
    }

    /// Length of the window in whole days, for display.
    pub fn days(&self) -> i64 {
        (self.end - self.start).num_days()
    }
}

/// Everything needed to talk to Cloudflare.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub api_token: String,
    pub account_id: String,
}

/// Failures the client can report.
#[derive(Debug, thiserror::Error)]
pub enum CloudflareError {
    #[error("cloudflare returned HTTP {status}: {body}")]
    Http { status: u16, body: String },

    #[error("cloudflare rejected the query: {0}")]
    GraphQl(String),

    #[error("unexpected response shape: {0}")]
    Shape(String),

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
}

impl CloudflareError {
    /// Whether retrying the same request could plausibly succeed.
    ///
    /// Rate limits and server-side failures are worth another attempt; a bad
    /// token or a malformed query is not.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { status, .. } => *status == 429 || *status >= 500,
            Self::Network(_) => true,
            Self::GraphQl(_) | Self::Shape(_) => false,
        }
    }
}

/// The GraphQL client. Cheap to clone; `reqwest::Client` pools connections.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
    credentials: Credentials,
}

impl Client {
    /// Build a client against the production endpoint.
    pub fn new(credentials: Credentials) -> Result<Self, CloudflareError> {
        Self::with_endpoint(credentials, DEFAULT_ENDPOINT)
    }

    /// Build a client against a custom endpoint. Used by tests.
    pub fn with_endpoint(
        credentials: Credentials,
        endpoint: impl Into<String>,
    ) -> Result<Self, CloudflareError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("cf-r2-monitor/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            endpoint: endpoint.into(),
            credentials,
        })
    }

    /// Fetch both datasets and merge them into one snapshot.
    pub async fn snapshot(&self, window: Window) -> Result<R2Snapshot, CloudflareError> {
        let storage = self.storage(window).await?;
        let operations = self.operations(window).await?;
        Ok(R2Snapshot {
            storage,
            operations,
            observed_at: Some(window.end),
        })
    }

    /// Fetch the storage series: one row per bucket per time bucket.
    pub async fn storage(&self, window: Window) -> Result<Vec<StoragePoint>, CloudflareError> {
        let variables = self.variables(window);
        let body = self.post(STORAGE_QUERY, variables).await?;
        parse_storage(&body)
    }

    /// Fetch per-action request counts.
    pub async fn operations(&self, window: Window) -> Result<Vec<OperationStat>, CloudflareError> {
        let variables = self.variables(window);
        let body = self.post(OPERATIONS_QUERY, variables).await?;
        parse_operations(&body)
    }

    fn variables(&self, window: Window) -> serde_json::Value {
        serde_json::json!({
            "accountTag": self.credentials.account_id,
            "start": window.start.to_rfc3339(),
            "end": window.end.to_rfc3339(),
        })
    }

    async fn post(
        &self,
        query: &str,
        variables: serde_json::Value,
    ) -> Result<serde_json::Value, CloudflareError> {
        let payload = serde_json::json!({ "query": query, "variables": variables });
        let response = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.credentials.api_token)
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await?;
        if !status.is_success() {
            return Err(CloudflareError::Http {
                status: status.as_u16(),
                body: truncate(&text, 600),
            });
        }

        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| CloudflareError::Shape(format!("response was not JSON: {e}")))?;

        // Cloudflare reports query-level failures in a top-level `errors` array
        // while still returning HTTP 200.
        if let Some(errors) = parsed.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                let joined = errors
                    .iter()
                    .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                    .collect::<Vec<_>>()
                    .join("; ");
                let shown = if joined.is_empty() {
                    truncate(
                        &serde_json::to_string(errors).unwrap_or_else(|_| "unknown".into()),
                        600,
                    )
                } else {
                    joined
                };
                return Err(CloudflareError::GraphQl(shown));
            }
        }

        Ok(parsed)
    }
}

/// Pull `accounts[0].r2StorageAdaptiveGroups` out of a storage response.
///
/// Every returned row is kept: the same bucket appears once per time bucket,
/// and it is those rows that make the billed GB-month average computable.
///
/// Kept separate from [`Client`] so it can be unit tested without a server.
pub fn parse_storage(body: &serde_json::Value) -> Result<Vec<StoragePoint>, CloudflareError> {
    let groups = groups(body, "r2StorageAdaptiveGroups")?;
    let mut points = Vec::with_capacity(groups.len());
    for group in groups {
        let bucket_name = group
            .get("dimensions")
            .and_then(|d| d.get("bucketName"))
            .and_then(|n| n.as_str())
            .ok_or_else(|| CloudflareError::Shape("storage row without bucketName".into()))?
            .to_string();
        let at = group
            .get("dimensions")
            .and_then(|d| d.get("datetime"))
            .and_then(|d| d.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc));
        let max = group
            .get("max")
            .ok_or_else(|| CloudflareError::Shape("storage row without max".into()))?;
        points.push(StoragePoint {
            bucket_name,
            payload_size: number(max, "payloadSize"),
            metadata_size: number(max, "metadataSize"),
            object_count: number(max, "objectCount"),
            upload_count: number(max, "uploadCount"),
            at,
        });
    }
    Ok(points)
}

/// Pull `accounts[0].r2OperationsAdaptiveGroups` out of an operations response.
pub fn parse_operations(body: &serde_json::Value) -> Result<Vec<OperationStat>, CloudflareError> {
    let groups = groups(body, "r2OperationsAdaptiveGroups")?;
    let mut operations: Vec<OperationStat> = Vec::with_capacity(groups.len());
    for group in groups {
        let action_type = group
            .get("dimensions")
            .and_then(|d| d.get("actionType"))
            .and_then(|n| n.as_str())
            .ok_or_else(|| CloudflareError::Shape("operation row without actionType".into()))?
            .to_string();
        let requests = group.get("sum").map(|s| number(s, "requests")).unwrap_or(0);
        let class = OperationClass::classify(&action_type);

        // The same action type may appear in more than one adaptive group when
        // Cloudflare splits it by time bucket; fold them together.
        if let Some(existing) = operations
            .iter_mut()
            .find(|op| op.action_type == action_type)
        {
            existing.requests += requests;
        } else {
            operations.push(OperationStat {
                action_type,
                requests,
                class,
            });
        }
    }
    Ok(operations)
}

/// Navigate to the dataset's group array, tolerating a missing account.
fn groups<'a>(
    body: &'a serde_json::Value,
    dataset: &str,
) -> Result<&'a Vec<serde_json::Value>, CloudflareError> {
    let accounts = body
        .pointer("/data/viewer/accounts")
        .and_then(|a| a.as_array())
        .ok_or_else(|| {
            CloudflareError::Shape(
                "response had no data.viewer.accounts — check that the token can read \
                 Account Analytics for this account ID"
                    .into(),
            )
        })?;

    // An account the token cannot see simply does not appear.
    let account = accounts.first().ok_or_else(|| {
        CloudflareError::Shape(
            "the token has no access to any account matching the given account ID".into(),
        )
    })?;

    account
        .get(dataset)
        .and_then(|g| g.as_array())
        .ok_or_else(|| CloudflareError::Shape(format!("response had no `{dataset}` array")))
}

/// Read a numeric field, treating `null` and absence as zero.
fn number(object: &serde_json::Value, key: &str) -> u64 {
    object
        .get(key)
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_f64().map(|f| f.max(0.0) as u64))
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0)
}

/// Clip long bodies so an error message stays readable in a journal.
fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &text[..cut])
}

/// Response of one GraphQL call, kept for reference in tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphQlResponse {
    pub data: Option<serde_json::Value>,
    pub errors: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_response_becomes_buckets() {
        let body = serde_json::json!({
            "data": { "viewer": { "accounts": [{
                "r2StorageAdaptiveGroups": [
                    {
                        "max": {
                            "payloadSize": 2_000_000_000u64,
                            "metadataSize": 20_000_000u64,
                            "objectCount": 900u64,
                            "uploadCount": 1u64
                        },
                        "dimensions": { "bucketName": "assets", "datetime": "2026-11-25T00:00:00Z" }
                    },
                    {
                        "max": {
                            "payloadSize": 1_000_000_000u64,
                            "metadataSize": 10_000_000u64,
                            "objectCount": 100u64,
                            "uploadCount": 0u64
                        },
                        "dimensions": { "bucketName": "eu_backups", "datetime": "2026-11-25T00:00:00Z" }
                    }
                ]
            }]}}
        });
        let buckets = parse_storage(&body).unwrap();
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].bucket_name, "assets");
        assert_eq!(buckets[0].payload_size, 2_000_000_000);
        assert_eq!(buckets[1].bucket_name, "eu_backups");
    }

    #[test]
    fn operations_response_classifies_actions() {
        let body = serde_json::json!({
            "data": { "viewer": { "accounts": [{
                "r2OperationsAdaptiveGroups": [
                    { "sum": { "requests": 400_000u64 }, "dimensions": { "actionType": "PutObject" } },
                    { "sum": { "requests": 3_000_000u64 }, "dimensions": { "actionType": "GetObject" } },
                    { "sum": { "requests": 50_000u64 }, "dimensions": { "actionType": "DeleteObject" } }
                ]
            }]}}
        });
        let ops = parse_operations(&body).unwrap();
        assert_eq!(ops.len(), 3);
        let put = ops.iter().find(|o| o.action_type == "PutObject").unwrap();
        assert_eq!(put.requests, 400_000);
        assert_eq!(put.class, OperationClass::ClassA);
        let get = ops.iter().find(|o| o.action_type == "GetObject").unwrap();
        assert_eq!(get.class, OperationClass::ClassB);
    }

    #[test]
    fn repeated_action_types_are_folded_together() {
        let body = serde_json::json!({
            "data": { "viewer": { "accounts": [{
                "r2OperationsAdaptiveGroups": [
                    { "sum": { "requests": 100u64 }, "dimensions": { "actionType": "PutObject" } },
                    { "sum": { "requests": 250u64 }, "dimensions": { "actionType": "PutObject" } }
                ]
            }]}}
        });
        let ops = parse_operations(&body).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].requests, 350);
    }

    #[test]
    fn missing_account_is_reported_clearly() {
        let body = serde_json::json!({ "data": { "viewer": { "accounts": [] } } });
        let err = parse_storage(&body).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("no access"), "got: {message}");
        assert!(!err.is_retryable());
    }

    #[test]
    fn null_numbers_are_zero() {
        let body = serde_json::json!({
            "data": { "viewer": { "accounts": [{
                "r2StorageAdaptiveGroups": [
                    {
                        "max": { "payloadSize": null, "metadataSize": null, "objectCount": null, "uploadCount": null },
                        "dimensions": { "bucketName": "empty" }
                    }
                ]
            }]}}
        });
        let buckets = parse_storage(&body).unwrap();
        assert_eq!(buckets[0].payload_size, 0);
        assert_eq!(buckets[0].object_count, 0);
    }

    #[test]
    fn window_is_clamped_to_cloudflares_limit() {
        assert_eq!(Window::trailing_days(7).days(), 7);
        // 90 days would be rejected by Cloudflare, so it clamps to 31.
        assert_eq!(Window::trailing_days(90).days(), MAX_WINDOW_DAYS);
        assert_eq!(Window::trailing_days(0).days(), 1);
    }

    #[test]
    fn rate_limits_and_server_errors_are_retryable() {
        assert!(CloudflareError::Http {
            status: 429,
            body: String::new()
        }
        .is_retryable());
        assert!(CloudflareError::Http {
            status: 503,
            body: String::new()
        }
        .is_retryable());
        assert!(!CloudflareError::Http {
            status: 403,
            body: String::new()
        }
        .is_retryable());
        assert!(!CloudflareError::GraphQl("bad query".into()).is_retryable());
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let text = "日本語のエラーメッセージ";
        let cut = truncate(text, 7);
        assert!(cut.ends_with('…'));
        assert!(text.starts_with(cut.trim_end_matches('…')));
    }
}

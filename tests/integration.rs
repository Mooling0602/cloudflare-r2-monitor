//! End-to-end tests against a throwaway HTTP server.
//!
//! The server is a few dozen lines of `std::net::TcpListener` rather than a
//! mocking crate: the client only ever does `POST` with a JSON body, so a real
//! mock framework would add a dependency without adding coverage.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use cloudflare_r2_monitor::cloudflare::{
    parse_operations, parse_storage, Client, Credentials, Window,
};
use cloudflare_r2_monitor::model::{OperationClass, R2Snapshot};
use cloudflare_r2_monitor::quota::{Metric, Status, Thresholds, UsageReport};

/// What the fake server should answer with.
#[derive(Clone, Copy)]
enum Reply {
    /// Both datasets, in the shape Cloudflare returns them.
    Success,
    /// A GraphQL-level error carried in a 200 response.
    GraphQlError,
    /// HTTP 403, as an under-scoped token produces.
    Forbidden,
    /// A 200 with no usable data.
    EmptyAccounts,
}

/// Serve exactly `requests` POSTs, then stop.
struct FakeCloudflare {
    endpoint: String,
    #[allow(dead_code)]
    hits: Arc<AtomicUsize>,
}

impl FakeCloudflare {
    fn start(reply: Reply, requests: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);

        std::thread::spawn(move || {
            for stream in listener.incoming().take(requests) {
                let Ok(mut stream) = stream else { continue };
                counter.fetch_add(1, Ordering::SeqCst);

                // Read the request head, then the body, and ignore both: the
                // reply depends only on which dataset was asked for, which we
                // do not need to inspect to keep this test honest about the
                // client's ability to cope with a single canned answer.
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if line == "\r\n" || line == "\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(rest) = lower.strip_prefix("content-length:") {
                        length = rest.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; length];
                use std::io::Read;
                let _ = reader.read_exact(&mut body);
                let query = String::from_utf8_lossy(&body);

                let (code, payload) = match reply {
                    Reply::Success if query.contains("r2StorageAdaptiveGroups") => {
                        (200, STORAGE.to_string())
                    }
                    Reply::Success => (200, OPERATIONS.to_string()),
                    Reply::GraphQlError => (
                        200,
                        r#"{"errors":[{"message":"authentication error"}]}"#.to_string(),
                    ),
                    Reply::Forbidden => (403, r#"{"success":false}"#.to_string()),
                    Reply::EmptyAccounts => {
                        (200, r#"{"data":{"viewer":{"accounts":[]}}}"#.to_string())
                    }
                };

                let response = format!(
                    "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        Self {
            endpoint: format!("http://{addr}/graphql"),
            hits,
        }
    }

    fn client(&self) -> Client {
        Client::with_endpoint(
            Credentials {
                api_token: "test-token".into(),
                account_id: "testaccount".into(),
            },
            &self.endpoint,
        )
        .expect("client")
    }
}

const STORAGE: &str = r#"{"data":{"viewer":{"accounts":[{"r2StorageAdaptiveGroups":[
  {"max":{"payloadSize":3420000000,"metadataSize":0,"objectCount":10,"uploadCount":0},
   "dimensions":{"bucketName":"assets","datetime":"2026-11-24T00:00:00Z"}},
  {"max":{"payloadSize":3420000000,"metadataSize":0,"objectCount":10,"uploadCount":0},
   "dimensions":{"bucketName":"assets","datetime":"2026-11-25T00:00:00Z"}},
  {"max":{"payloadSize":900000000,"metadataSize":0,"objectCount":5,"uploadCount":0},
   "dimensions":{"bucketName":"backups","datetime":"2026-11-24T00:00:00Z"}},
  {"max":{"payloadSize":1200000000,"metadataSize":0,"objectCount":6,"uploadCount":0},
   "dimensions":{"bucketName":"backups","datetime":"2026-11-25T00:00:00Z"}}
]}]}}}"#;

const OPERATIONS: &str = r#"{"data":{"viewer":{"accounts":[{"r2OperationsAdaptiveGroups":[
  {"sum":{"requests":412880},"dimensions":{"actionType":"PutObject"}},
  {"sum":{"requests":1204551},"dimensions":{"actionType":"GetObject"}},
  {"sum":{"requests":99000},"dimensions":{"actionType":"DeleteObject"}}
]}]}}}"#;

#[tokio::test]
async fn fetches_both_datasets_and_merges_them() {
    let server = FakeCloudflare::start(Reply::Success, 2);
    let window = Window::trailing_days(30);
    let snapshot = server.client().snapshot(window).await.expect("snapshot");

    // Four dated rows, but only two distinct buckets: the rows are a time
    // series, so the totals must come from the latest row per bucket.
    assert_eq!(snapshot.storage.len(), 4);
    assert_eq!(snapshot.latest_per_bucket().len(), 2);
    assert_eq!(snapshot.total_payload_bytes(), 4_620_000_000);
    assert_eq!(snapshot.operations_of(OperationClass::ClassA), 412_880);
    assert_eq!(snapshot.operations_of(OperationClass::ClassB), 1_204_551);
    assert_eq!(snapshot.operations_of(OperationClass::Free), 99_000);
    assert!(snapshot.observed_at.is_some());

    let report = UsageReport::evaluate(snapshot, Thresholds::default());
    assert_eq!(report.status(), Status::Ok);
    assert!((report.reading(Metric::Storage).unwrap().percent() - 46.2).abs() < 1e-9);
}

#[tokio::test]
async fn graphql_errors_are_surfaced_not_swallowed() {
    let server = FakeCloudflare::start(Reply::GraphQlError, 1);
    let error = server
        .client()
        .snapshot(Window::trailing_days(7))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("authentication error"),
        "{error}"
    );
    // A rejected query will not fix itself; retrying would just burn quota.
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn http_403_reports_the_status() {
    let server = FakeCloudflare::start(Reply::Forbidden, 1);
    let error = server
        .client()
        .storage(Window::trailing_days(7))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("403"), "{error}");
    assert!(!error.is_retryable());
}

#[tokio::test]
async fn an_account_the_token_cannot_see_is_reported() {
    let server = FakeCloudflare::start(Reply::EmptyAccounts, 1);
    let error = server
        .client()
        .storage(Window::trailing_days(7))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no access"), "{error}");
}

#[test]
fn parsing_is_independent_of_the_network() {
    let storage: serde_json::Value = serde_json::from_str(STORAGE).unwrap();
    let buckets = parse_storage(&storage).unwrap();
    assert_eq!(buckets[0].bucket_name, "assets");

    let operations: serde_json::Value = serde_json::from_str(OPERATIONS).unwrap();
    let ops = parse_operations(&operations).unwrap();
    assert_eq!(ops.len(), 3);
}

#[test]
fn a_snapshot_round_trips_through_json() {
    let snapshot = R2Snapshot {
        storage: parse_storage(&serde_json::from_str(STORAGE).unwrap()).unwrap(),
        operations: parse_operations(&serde_json::from_str(OPERATIONS).unwrap()).unwrap(),
        observed_at: None,
    };
    let json = serde_json::to_string(&snapshot).unwrap();
    let back: R2Snapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back, snapshot);
}

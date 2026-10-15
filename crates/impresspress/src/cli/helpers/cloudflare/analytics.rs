//! The memory a deploy candidate reached in its concurrency smoke, read back
//! from Cloudflare's Workers analytics after the smoke.
//!
//! A Worker isolate is capped at 128 MB, and the binding cost on this target
//! is Wasm linear memory: one built runtime holds 13–20 MB of it. The smoke's
//! pass/fail verdict does not show how close a candidate came, so the deploy
//! reads `workersInvocationsAdaptive` for the candidate's version and prints
//! the peak per-isolate `wasmMemoryBytes` (and the isolate's whole
//! `memoryUsageBytes`) by request status. A run that kills an isolate shows up
//! as `exceededResources` rows at that peak.
//!
//! Informational only: an unreadable token, an analytics error or data that
//! has not arrived yet is printed as a note and never fails the deploy. The
//! token comes from `wrangler auth token --json`, the credential every other
//! deploy step already uses.

use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

const GRAPHQL_ENDPOINT: &str = "https://api.cloudflare.com/client/v4/graphql";
/// Cloudflare's per-isolate memory limit, on every plan.
pub const ISOLATE_MEMORY_LIMIT_BYTES: u64 = 128 * 1024 * 1024;
/// Analytics arrive with a lag; poll this often, for at most this long.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);
const POLL_ATTEMPTS: u32 = 12;

const QUERY: &str =
    "query($account: String!, $filter: AccountWorkersInvocationsAdaptiveFilter_InputObject) { \
viewer { accounts(filter: {accountTag: $account}) { \
workersInvocationsAdaptive(limit: 100, filter: $filter, orderBy: [max_wasmMemoryBytes_DESC]) { \
sum { requests } max { cpuTime memoryUsageBytes wasmMemoryBytes } dimensions { status } \
} } } }";

/// Which invocations to read: one Worker version, from `since` (RFC 3339)
/// until the moment of the query.
pub struct CandidateWindow<'a> {
    pub account_id: &'a str,
    pub script_name: &'a str,
    pub version_id: &'a str,
    pub since: &'a str,
}

/// The GraphQL request body for `window`, read until `until` (RFC 3339).
pub fn query_body(window: &CandidateWindow<'_>, until: &str) -> Value {
    json!({
        "query": QUERY,
        "variables": {
            "account": window.account_id,
            "filter": {
                "scriptName": window.script_name,
                "scriptVersion": window.version_id,
                "datetime_geq": window.since,
                "datetime_leq": until,
            },
        },
    })
}

/// One request status's invocations: how many, and the peaks they reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusPeak {
    pub status: String,
    pub requests: u64,
    pub max_wasm_memory_bytes: u64,
    pub max_memory_usage_bytes: u64,
    pub max_cpu_time_us: u64,
}

/// Parse a `workersInvocationsAdaptive` answer into one row per status.
pub fn parse_response(response: &Value) -> Result<Vec<StatusPeak>> {
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            bail!("analytics query refused: {}", Value::Array(errors.clone()));
        }
    }
    let rows = response
        .pointer("/data/viewer/accounts/0/workersInvocationsAdaptive")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("analytics answer has no workersInvocationsAdaptive rows"))?;
    rows.iter()
        .map(|row| {
            let number = |pointer: &str| {
                row.pointer(pointer)
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("analytics row is missing {pointer}"))
            };
            Ok(StatusPeak {
                status: row
                    .pointer("/dimensions/status")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("analytics row is missing /dimensions/status"))?
                    .to_string(),
                requests: number("/sum/requests")?,
                max_wasm_memory_bytes: number("/max/wasmMemoryBytes")?,
                max_memory_usage_bytes: number("/max/memoryUsageBytes")?,
                max_cpu_time_us: number("/max/cpuTime")?,
            })
        })
        .collect()
}

/// Binary megabytes, the unit Cloudflare's 128 MB limit is stated in.
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// The lines the deploy prints for `rows`.
pub fn render(rows: &[StatusPeak]) -> Vec<String> {
    let requests: u64 = rows.iter().map(|row| row.requests).sum();
    let peak_wasm = rows
        .iter()
        .map(|row| row.max_wasm_memory_bytes)
        .max()
        .unwrap_or(0);
    let peak_isolate = rows
        .iter()
        .map(|row| row.max_memory_usage_bytes)
        .max()
        .unwrap_or(0);
    let mut lines = vec![format!(
        "-> candidate memory per isolate (Cloudflare analytics, {requests} requests): \
         peak Wasm {}, peak isolate {} of the {} limit",
        megabytes(peak_wasm),
        megabytes(peak_isolate),
        megabytes(ISOLATE_MEMORY_LIMIT_BYTES),
    )];
    for row in rows {
        lines.push(format!(
            "   {}: {} requests, Wasm {}, isolate {}, CPU {} ms",
            row.status,
            row.requests,
            megabytes(row.max_wasm_memory_bytes),
            megabytes(row.max_memory_usage_bytes),
            row.max_cpu_time_us / 1_000,
        ));
    }
    if rows.iter().any(|row| row.status == "exceededResources") {
        lines.push(
            "   exceededResources: an isolate hit a Cloudflare limit; at a peak near the \
             limit above that limit is memory"
                .to_string(),
        );
    }
    lines
}

/// The token `wrangler` deploys with, from `wrangler auth token --json`.
fn wrangler_token() -> Result<String> {
    let output = Command::new("wrangler")
        .args(["auth", "token", "--json"])
        .output()
        .context("run `wrangler auth token --json`")?;
    if !output.status.success() {
        bail!("`wrangler auth token --json` exited with {}", output.status);
    }
    let answer: Value = serde_json::from_slice(&output.stdout)
        .context("parse `wrangler auth token --json` output")?;
    answer
        .get("token")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow!("`wrangler auth token --json` returned no bearer token (an API key login?)")
        })
}

async fn fetch_rows(
    client: &reqwest::Client,
    token: &str,
    window: &CandidateWindow<'_>,
) -> Result<Vec<StatusPeak>> {
    let response: Value = client
        .post(GRAPHQL_ENDPOINT)
        .bearer_auth(token)
        .json(&query_body(window, &impresspress_core::util::now_rfc3339()))
        .send()
        .await
        .context("POST the Cloudflare GraphQL analytics API")?
        .error_for_status()
        .context("Cloudflare GraphQL analytics API")?
        .json()
        .await
        .context("parse the Cloudflare GraphQL analytics answer")?;
    parse_response(&response)
}

async fn read_peaks(
    window: &CandidateWindow<'_>,
    expected_requests: u64,
) -> Result<Vec<StatusPeak>> {
    let token = wrangler_token()?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build analytics http client")?;
    let mut rows = Vec::new();
    for attempt in 1..=POLL_ATTEMPTS {
        rows = fetch_rows(&client, &token, window).await?;
        let seen: u64 = rows.iter().map(|row| row.requests).sum();
        if seen >= expected_requests || attempt == POLL_ATTEMPTS {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Ok(rows)
}

/// Print the candidate's per-isolate memory peaks. Waits for analytics to
/// account for `expected_requests` (bounded), and never fails the deploy.
pub async fn report_candidate_memory(window: &CandidateWindow<'_>, expected_requests: u64) {
    match read_peaks(window, expected_requests).await {
        Ok(rows) if rows.is_empty() => println!(
            "-> candidate memory: no analytics for version {} yet",
            window.version_id
        ),
        Ok(rows) => {
            for line in render(&rows) {
                println!("{line}");
            }
        }
        Err(error) => println!("-> candidate memory not read (informational): {error:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> CandidateWindow<'static> {
        CandidateWindow {
            account_id: "acct",
            script_name: "site",
            version_id: "1f4620eb-bc9e-4be6-86f9-34c6a87b69c9",
            since: "2026-10-14T21:38:00+00:00",
        }
    }

    #[test]
    fn the_query_reads_one_version_in_the_smoke_window() {
        let body = query_body(&window(), "2026-10-14T21:40:00+00:00");
        let filter = &body["variables"]["filter"];
        assert_eq!(body["variables"]["account"], "acct");
        assert_eq!(filter["scriptName"], "site");
        assert_eq!(
            filter["scriptVersion"],
            "1f4620eb-bc9e-4be6-86f9-34c6a87b69c9"
        );
        assert_eq!(filter["datetime_geq"], "2026-10-14T21:38:00+00:00");
        assert_eq!(filter["datetime_leq"], "2026-10-14T21:40:00+00:00");
        let query = body["query"].as_str().unwrap();
        assert!(query.contains("wasmMemoryBytes") && query.contains("memoryUsageBytes"));
    }

    /// The shape Cloudflare answered for the 2026-10-14 smoke that killed an
    /// isolate (values from that read).
    fn smoke_answer() -> Value {
        json!({
            "data": {"viewer": {"accounts": [{"workersInvocationsAdaptive": [
                {"dimensions": {"status": "exceededResources"},
                 "max": {"cpuTime": 330_335, "memoryUsageBytes": 127_802_476_u64, "wasmMemoryBytes": 120_389_632_u64},
                 "sum": {"requests": 9}},
                {"dimensions": {"status": "success"},
                 "max": {"cpuTime": 668_792, "memoryUsageBytes": 71_179_733_u64, "wasmMemoryBytes": 63_045_632_u64},
                 "sum": {"requests": 153}}
            ]}]}},
            "errors": null
        })
    }

    #[test]
    fn a_killed_isolate_reads_as_its_peak_and_is_named() {
        let rows = parse_response(&smoke_answer()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, "exceededResources");
        assert_eq!(rows[0].max_wasm_memory_bytes, 120_389_632);

        let lines = render(&rows);
        assert!(
            lines[0].contains("162 requests")
                && lines[0].contains("peak Wasm 114.8 MiB")
                && lines[0].contains("peak isolate 121.9 MiB")
                && lines[0].contains("128.0 MiB limit"),
            "{lines:?}"
        );
        assert!(lines
            .iter()
            .any(|line| line.contains("success: 153 requests")));
        assert!(lines.last().unwrap().contains("exceededResources"));
    }

    #[test]
    fn a_healthy_smoke_names_no_limit() {
        let mut answer = smoke_answer();
        answer["data"]["viewer"]["accounts"][0]["workersInvocationsAdaptive"]
            .as_array_mut()
            .unwrap()
            .remove(0);
        let lines = render(&parse_response(&answer).unwrap());
        assert!(lines[0].contains("peak Wasm 60.1 MiB"), "{lines:?}");
        assert!(!lines.iter().any(|line| line.contains("exceededResources")));
    }

    #[test]
    fn a_refused_query_is_an_error_not_an_empty_report() {
        let refused = json!({"data": null, "errors": [{"message": "not authorized"}]});
        let error = parse_response(&refused).unwrap_err().to_string();
        assert!(error.contains("not authorized"), "{error}");
        assert!(parse_response(&json!({"data": {}})).is_err());
    }
}

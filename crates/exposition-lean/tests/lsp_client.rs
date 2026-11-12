//! The LSP client against a real `lean --server`: request correlation,
//! cancellation, timeouts, and orderly shutdown.

mod common;

use std::time::Duration;

use common::*;
use exposition_core::job::{EnvironmentSpec, ExecutionPolicy, JobOutcome, JobSpec};
use exposition_execution::InteractiveOptions;
use exposition_lean::lsp::{LspClient, LspError};
use exposition_lean::toolchain::{self, Resolution};
use exposition_lean::uri::path_to_uri;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn requests_are_correlated_cancelled_and_timed_out() {
    if !lean_available() {
        return;
    }
    let h = harness("lean-basic", Options::default()).await;
    let home = toolchain::elan_home(|n| std::env::var(n).ok());
    let Resolution::Installed(t) = toolchain::resolve(home.as_deref(), TOOLCHAIN) else { unreachable!() };
    let spec = JobSpec {
        label: "lean --server (test)".into(),
        program: t.lean.display().to_string(),
        args: vec!["--server".into()],
        environment: EnvironmentSpec::default(),
        working_directory: h.root.display().to_string(),
        policy: ExecutionPolicy::TrustedNative,
        timeout_ms: None,
        log_limit_bytes: None,
        stdin: None,
    };
    let job = h.exec.launch_interactive(spec, InteractiveOptions::default()).await.expect("server starts");
    let job_id = job.id.clone();
    let (client, _incoming) = LspClient::start(job.input, job.output);
    let init = client
        .call(
            "initialize",
            json!({ "processId": null, "rootUri": path_to_uri(&h.root), "capabilities": {} }),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert!(init.pointer("/capabilities/hoverProvider").is_some(), "{init}");
    client.notify("initialized", json!({})).unwrap();
    let file = h.root.join("Scratch.lean");
    let uri = path_to_uri(&file);
    let text = "theorem t (p : Prop) (hp : p) : p ∧ p := by\n  constructor\n  exact hp\n  exact hp\n";
    client
        .notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "lean4", "version": 1, "text": text } }),
        )
        .unwrap();

    // A request Lean cannot answer yet (a future version): cancelling it
    // resolves the waiter immediately and tells the server to stop.
    let pending = client.request("textDocument/waitForDiagnostics", json!({ "uri": uri, "version": 99 }));
    let id = pending.id();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(client.pending_count(), 1);
    client.cancel(id);
    assert_eq!(pending.response(Duration::from_secs(5)).await, Err(LspError::Cancelled));
    assert_eq!(client.pending_count(), 0);

    // A timeout cancels too.
    let slow = client.request("textDocument/waitForDiagnostics", json!({ "uri": uri, "version": 100 }));
    assert_eq!(slow.response(Duration::from_millis(300)).await, Err(LspError::Timeout));
    assert_eq!(client.pending_count(), 0);

    // Dropping a pending request cancels it.
    drop(client.request("textDocument/waitForDiagnostics", json!({ "uri": uri, "version": 101 })));
    assert_eq!(client.pending_count(), 0);

    // The session is still healthy: responses go to the right callers even
    // when several requests are in flight.
    client
        .call("textDocument/waitForDiagnostics", json!({ "uri": uri, "version": 1 }), Duration::from_secs(120))
        .await
        .unwrap();
    let a = client.request(
        "$/lean/plainGoal",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 1, "character": 2 } }),
    );
    let b = client.request(
        "$/lean/plainGoal",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 3, "character": 2 } }),
    );
    let (a, b) =
        (a.response(Duration::from_secs(30)).await.unwrap(), b.response(Duration::from_secs(30)).await.unwrap());
    assert_eq!(a["goals"].as_array().unwrap().len(), 1, "{a}");
    assert!(a["goals"][0].as_str().unwrap().contains("⊢ p ∧ p"), "{a}");
    assert_eq!(b["goals"].as_array().unwrap().len(), 1, "{b}");
    let second = b["goals"][0].as_str().unwrap();
    assert!(second.contains("⊢ p") && !second.contains('∧'), "{b}");
    // Unknown methods come back as errors, not hangs.
    let err = client.call("$/exposition/unknown", json!({}), Duration::from_secs(30)).await.unwrap_err();
    assert!(matches!(err, LspError::Response { .. }), "{err:?}");

    // Orderly shutdown: the process exits by itself.
    client.call("shutdown", serde_json::Value::Null, Duration::from_secs(10)).await.unwrap();
    client.notify("exit", serde_json::Value::Null).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(20), h.exec.wait(&job_id)).await.unwrap().unwrap();
    assert_eq!(result.outcome, JobOutcome::Exited { code: 0 });
    // After the stream closed, requests fail fast.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(client.is_closed());
    assert_eq!(client.call("shutdown", serde_json::Value::Null, Duration::from_secs(5)).await, Err(LspError::Closed));
}

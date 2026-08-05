//! Reshape rust-analyzer's raw `experimental/runnables` output into a compact,
//! LLM-friendly form, and run a runnable as a cargo subprocess.
//!
//! Raw runnables look like
//! ```ignore
//! {
//!   "label": "test foo::bar::tests::test_x",
//!   "kind": "cargo",
//!   "location": { "targetUri": "...", "targetRange": {...}, ... },
//!   "args": {
//!     "workspaceRoot": "...",
//!     "cargoArgs": ["test", "--package", "...", "--bin", "..."],
//!     "cargoExtraArgs": [],
//!     "executableArgs": ["foo::bar::tests::test_x", "--exact", "--nocapture"],
//!     ...
//!   }
//! }
//! ```
//!
//! Reshape keeps `location` intact so the snippet walker can still enrich it,
//! but flattens cargo args, classifies kind from the label, and stamps the
//! `can_run_via_mcp` flag based on the env-gate.

use serde_json::{json, Value};

/// Env var that opts the host into in-MCP execution of cargo runnables.
/// Set to `1` to enable; any other value (including unset) disables.
pub const RUN_GATE_ENV: &str = "RUST_ANALYZER_MCP_ALLOW_RUN";

/// Cargo subcommands the `run_runnable` tool will accept. Anything else is
/// rejected before spawning, so a runnable cannot be coerced into running an
/// arbitrary command.
pub const ALLOWED_CARGO_SUBCOMMANDS: &[&str] = &[
    "test", "bench", "run", "build", "check", "clippy", "doc", "nextest",
];

/// Default subprocess timeout for `run_runnable`.
pub const DEFAULT_RUN_TIMEOUT_SECS: u64 = 60;

/// Hard upper bound on the timeout — no runnable should ever block the LLM
/// for more than ten minutes.
pub const MAX_RUN_TIMEOUT_SECS: u64 = 600;

/// Per-stream cap for captured stdout/stderr (5 KiB).
pub const RUN_OUTPUT_BYTE_CAP: usize = 5 * 1024;

/// Reshape the raw `experimental/runnables` array. Anything that isn't an
/// array (null, garbage) round-trips unchanged so error/loading states stay
/// recognisable to the caller.
pub fn reshape_runnables(raw: Value, can_run: bool) -> Value {
    let Value::Array(items) = raw else {
        return raw;
    };
    let mut runnables: Vec<Value> = items.into_iter().map(|r| reshape_one(r, can_run)).collect();
    runnables.sort_by(|a, b| {
        let ka = a.get("kind").and_then(Value::as_str).unwrap_or("");
        let kb = b.get("kind").and_then(Value::as_str).unwrap_or("");
        ka.cmp(kb).then_with(|| {
            let la = a.get("label").and_then(Value::as_str).unwrap_or("");
            let lb = b.get("label").and_then(Value::as_str).unwrap_or("");
            la.cmp(lb)
        })
    });
    let total = runnables.len();
    json!({
        "runnables": runnables,
        "total": total,
        "can_run_via_mcp": can_run,
    })
}

fn reshape_one(raw: Value, can_run: bool) -> Value {
    let label = raw
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let kind = classify_kind(&label);

    let args = raw.get("args");
    let cargo_args = args.map(flatten_cargo_args).unwrap_or_default();
    let executable_args: Vec<Value> = args
        .and_then(|a| a.get("executableArgs"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let fq_name = derive_fq_name(&label, &executable_args);

    let location = raw.get("location").cloned().unwrap_or(Value::Null);

    let mut out = serde_json::Map::new();
    out.insert("kind".to_string(), json!(kind));
    out.insert("label".to_string(), json!(label));
    if let Some(name) = fq_name {
        out.insert("fq_name".to_string(), json!(name));
    }
    out.insert("cargo_args".to_string(), json!(cargo_args));
    out.insert("location".to_string(), location);
    out.insert("can_run_via_mcp".to_string(), json!(can_run));
    Value::Object(out)
}

/// First whitespace-separated token of `label` is the runnable's kind in
/// rust-analyzer (e.g. `"test foo::bar"` → `"test"`, `"test-mod tests"` →
/// `"test-mod"`, `"run"` for binaries). Empty label → `"other"`.
fn classify_kind(label: &str) -> String {
    label
        .split_whitespace()
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("other")
        .to_string()
}

/// Combine `cargoArgs + cargoExtraArgs + ["--"] + executableArgs` into a
/// single ready-to-pass argv. The `--` separator is only inserted when there
/// are executable args, matching how cargo expects the invocation.
fn flatten_cargo_args(args: &Value) -> Vec<String> {
    let mut out = Vec::new();
    out.extend(string_array(args.get("cargoArgs")));
    out.extend(string_array(args.get("cargoExtraArgs")));
    let exec = string_array(args.get("executableArgs"));
    if !exec.is_empty() {
        out.push("--".to_string());
        out.extend(exec);
    }
    out
}

fn string_array(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Best-effort fully-qualified name extraction. Tests usually carry the
/// path as the first executable arg (e.g. `["foo::bar::test_x", "--exact"]`);
/// otherwise we strip the leading kind word from the label and use the rest.
fn derive_fq_name(label: &str, executable_args: &[Value]) -> Option<String> {
    if let Some(first) = executable_args.first().and_then(Value::as_str) {
        if !first.is_empty() && !first.starts_with('-') {
            return Some(first.to_string());
        }
    }
    let rest = label.split_once(' ').map(|(_, r)| r.trim()).unwrap_or("");
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

/// Validate the `cargo_args` payload of `run_runnable`.
///
/// Rules: non-empty; first arg must be in [`ALLOWED_CARGO_SUBCOMMANDS`].
/// Cargo *flags* before the subcommand (e.g. `+nightly`, `-Z something`) are
/// rejected — the runnable reshape never emits them and accepting them would
/// open arbitrary command surface.
pub fn validate_cargo_args(args: &[String]) -> Result<(), String> {
    let Some(first) = args.first() else {
        return Err("cargo_args must be non-empty".into());
    };
    if !ALLOWED_CARGO_SUBCOMMANDS.contains(&first.as_str()) {
        return Err(format!(
            "cargo_args[0] must be one of {ALLOWED_CARGO_SUBCOMMANDS:?}; got {first:?}",
        ));
    }
    Ok(())
}

/// Read [`RUN_GATE_ENV`] and decide if `run_runnable` is permitted in this
/// process. Anything other than `"1"` keeps it disabled — opting into
/// running user code should be deliberate.
pub fn run_gate_enabled() -> bool {
    std::env::var(RUN_GATE_ENV).ok().as_deref() == Some("1")
}

/// Truncate captured output to `RUN_OUTPUT_BYTE_CAP`, snapping back to a
/// char boundary so we never split a UTF-8 sequence. Returns `(text, true)`
/// when truncation actually happened.
pub fn cap_output(s: String) -> (String, bool) {
    if s.len() <= RUN_OUTPUT_BYTE_CAP {
        return (s, false);
    }
    let mut cut = RUN_OUTPUT_BYTE_CAP;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut truncated = s;
    truncated.truncate(cut);
    (truncated, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reshape_empty_array_returns_empty_envelope() {
        let out = reshape_runnables(json!([]), false);
        assert_eq!(out["runnables"], json!([]));
        assert_eq!(out["total"], 0);
        assert_eq!(out["can_run_via_mcp"], false);
    }

    #[test]
    fn reshape_passes_null_through() {
        let out = reshape_runnables(Value::Null, true);
        assert!(out.is_null());
    }

    #[test]
    fn reshape_classifies_kinds_and_flattens_args() {
        let raw = json!([{
            "label": "test foo::bar::tests::it_works",
            "kind": "cargo",
            "location": { "targetUri": "file:///tmp/a.rs", "targetRange": {} },
            "args": {
                "workspaceRoot": "/tmp",
                "cargoArgs": ["test", "--package", "demo", "--lib"],
                "cargoExtraArgs": [],
                "executableArgs": ["foo::bar::tests::it_works", "--exact", "--nocapture"]
            }
        }]);
        let out = reshape_runnables(raw, true);
        let r = &out["runnables"][0];
        assert_eq!(r["kind"], "test");
        assert_eq!(r["label"], "test foo::bar::tests::it_works");
        assert_eq!(r["fq_name"], "foo::bar::tests::it_works");
        assert_eq!(
            r["cargo_args"],
            json!([
                "test",
                "--package",
                "demo",
                "--lib",
                "--",
                "foo::bar::tests::it_works",
                "--exact",
                "--nocapture"
            ])
        );
        assert_eq!(r["location"]["targetUri"], "file:///tmp/a.rs");
        assert_eq!(r["can_run_via_mcp"], true);
    }

    #[test]
    fn reshape_omits_separator_when_no_executable_args() {
        let raw = json!([{
            "label": "run main",
            "args": {
                "cargoArgs": ["run", "--package", "demo", "--bin", "demo"],
                "cargoExtraArgs": [],
                "executableArgs": []
            }
        }]);
        let out = reshape_runnables(raw, false);
        assert_eq!(
            out["runnables"][0]["cargo_args"],
            json!(["run", "--package", "demo", "--bin", "demo"])
        );
        assert_eq!(out["runnables"][0]["fq_name"], "main");
    }

    #[test]
    fn reshape_sorts_by_kind_then_label() {
        let raw = json!([
            { "label": "test b" },
            { "label": "bench a" },
            { "label": "test a" }
        ]);
        let out = reshape_runnables(raw, false);
        let labels: Vec<&str> = out["runnables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, vec!["bench a", "test a", "test b"]);
    }

    #[test]
    fn reshape_skips_fq_name_when_first_executable_arg_is_a_flag() {
        let raw = json!([{
            "label": "test-mod tests",
            "args": {
                "cargoArgs": ["test"],
                "executableArgs": ["--ignored"]
            }
        }]);
        let out = reshape_runnables(raw, false);
        let r = &out["runnables"][0];
        assert_eq!(r["kind"], "test-mod");
        assert_eq!(r["fq_name"], "tests");
    }

    #[test]
    fn validate_cargo_args_rejects_empty() {
        assert!(validate_cargo_args(&[]).is_err());
    }

    #[test]
    fn validate_cargo_args_rejects_unknown_subcommand() {
        let v = vec!["uninstall".to_string(), "--all".to_string()];
        assert!(validate_cargo_args(&v).is_err());
    }

    #[test]
    fn validate_cargo_args_accepts_test() {
        let v = vec!["test".to_string(), "--lib".to_string()];
        assert!(validate_cargo_args(&v).is_ok());
    }

    #[test]
    fn cap_output_no_truncation_under_cap() {
        let (s, t) = cap_output("hello".to_string());
        assert_eq!(s, "hello");
        assert!(!t);
    }

    #[test]
    fn cap_output_truncates_oversized() {
        let big = "a".repeat(RUN_OUTPUT_BYTE_CAP + 100);
        let (s, t) = cap_output(big);
        assert_eq!(s.len(), RUN_OUTPUT_BYTE_CAP);
        assert!(t);
    }

    #[test]
    fn cap_output_snaps_to_char_boundary() {
        let mut s = "x".repeat(RUN_OUTPUT_BYTE_CAP - 1);
        s.push('ä'); // 2 bytes; boundary lands mid-codepoint
        s.push_str("xxxxx");
        let (out, truncated) = cap_output(s);
        assert!(truncated);
        // No panic, and the result is valid UTF-8 (which String guarantees);
        // length is at most the cap.
        assert!(out.len() <= RUN_OUTPUT_BYTE_CAP);
    }
}

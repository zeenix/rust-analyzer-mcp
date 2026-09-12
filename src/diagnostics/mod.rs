use serde_json::{json, Value};
use std::collections::HashSet;

/// What a diagnostic is *about*, as opposed to who said it: the code, and where it starts.
///
/// rust-analyzer reports each fault twice -- once from its own analysis (`"source":
/// "rust-analyzer"`) and once from `cargo check` proxied through flycheck (`"source": "rustc"`)
/// -- with different wording each time. Counted naively that is two errors where the compiler
/// sees one, which is exactly the number anything gating on the count reads.
fn fault(diag: &Value) -> (u64, String, u64, u64) {
    let start = diag.pointer("/range/start");
    (
        diag.get("severity").and_then(Value::as_u64).unwrap_or(0),
        diag.get("code")
            .map(ToString::to_string)
            .unwrap_or_default(),
        start
            .and_then(|s| s.get("line"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        start
            .and_then(|s| s.get("character"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    )
}

fn message(diag: &Value) -> &str {
    diag.get("message").and_then(Value::as_str).unwrap_or("")
}

fn source(diag: &Value) -> &str {
    diag.get("source")
        .and_then(Value::as_str)
        .unwrap_or("rust-analyzer")
}

/// Every message carried in a diagnostic's `relatedInformation`.
fn related_messages(diag: &Value) -> impl Iterator<Item = &str> {
    diag.get("relatedInformation")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(message)
}

pub fn format_diagnostics(file_path: &str, result: &Value) -> Value {
    let Some(diag_array) = result.as_array() else {
        return json!({
            "file": file_path,
            "diagnostics": [],
            "summary": {
                "errors": 0,
                "warnings": 0,
                "information": 0,
                "hints": 0
            }
        });
    };

    // One entry per fault, in the order the faults first appeared. The rustc wording wins where
    // both sources reported the same fault -- it is the wording the compiler prints, and it names
    // the item ("cannot find value `undefined_var` in this scope") where rust-analyzer's does not
    // ("no such value in this scope").
    let mut faults: Vec<(u64, String, u64, u64)> = Vec::new();
    let mut kept: Vec<(Value, Vec<String>)> = Vec::new();

    for diag in diag_array {
        let key = fault(diag);
        match faults.iter().position(|seen| *seen == key) {
            Some(index) => {
                let (held, sources) = &mut kept[index];
                if source(diag) == "rustc" && source(held) != "rustc" {
                    *held = diag.clone();
                }
                let from = source(diag).to_string();
                if !sources.contains(&from) {
                    sources.push(from);
                }
            }
            None => {
                faults.push(key);
                kept.push((diag.clone(), vec![source(diag).to_string()]));
            }
        }
    }

    // rustc's notes and suggestions arrive twice as well: once inside the diagnostic they belong
    // to, as `relatedInformation`, and once more as free-standing hints. The hint carries no
    // context of its own -- "value moved here" says nothing about what was moved -- so the copy
    // attached to its diagnostic is the useful one.
    let attached: HashSet<&str> = kept
        .iter()
        .flat_map(|(diag, _)| related_messages(diag).chain(std::iter::once(message(diag))))
        .collect();

    let mut diagnostics = Vec::with_capacity(kept.len());
    let (mut errors, mut warnings, mut information, mut hints) = (0, 0, 0, 0);

    for (diag, sources) in &kept {
        let severity = diag.get("severity").and_then(Value::as_u64);
        if severity == Some(4) && attached.contains(message(diag)) {
            continue;
        }

        match severity {
            Some(1) => errors += 1,
            Some(2) => warnings += 1,
            Some(3) => information += 1,
            Some(4) => hints += 1,
            _ => {}
        }

        diagnostics.push(json!({
            "severity": match severity {
                Some(1) => "error",
                Some(2) => "warning",
                Some(3) => "information",
                Some(4) => "hint",
                _ => "unknown"
            },
            "range": diag.get("range").cloned().unwrap_or(json!(null)),
            "message": message(diag),
            "code": diag.get("code").cloned().unwrap_or(json!(null)),
            "source": source(diag),
            // Both, when both reported it: a fault rust-analyzer sees and rustc does not is a
            // different thing from one the compiler will fail the build over.
            "sources": sources,
            "relatedInformation": diag.get("relatedInformation").cloned().unwrap_or(json!(null))
        }));
    }

    json!({
        "file": file_path,
        "diagnostics": diagnostics,
        "summary": {
            "errors": errors,
            "warnings": warnings,
            "information": information,
            "hints": hints
        }
    })
}

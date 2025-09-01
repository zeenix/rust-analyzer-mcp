use serde_json::{json, Value};

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

    let mut output = json!({
        "file": file_path,
        "diagnostics": [],
        "summary": {
            "errors": 0,
            "warnings": 0,
            "information": 0,
            "hints": 0
        }
    });

    let mut errors = 0;
    let mut warnings = 0;
    let mut information = 0;
    let mut hints = 0;

    for diag in diag_array {
        // Count by severity.
        if let Some(severity) = diag.get("severity").and_then(|s| s.as_u64()) {
            match severity {
                1 => errors += 1,
                2 => warnings += 1,
                3 => information += 1,
                4 => hints += 1,
                _ => {}
            }
        }

        // Add formatted diagnostic.
        let Some(diag_list) = output["diagnostics"].as_array_mut() else {
            continue;
        };

        diag_list.push(json!({
            "severity": match diag.get("severity").and_then(|s| s.as_u64()) {
                Some(1) => "error",
                Some(2) => "warning",
                Some(3) => "information",
                Some(4) => "hint",
                _ => "unknown"
            },
            "range": diag.get("range").cloned().unwrap_or(json!(null)),
            "message": diag.get("message").and_then(|m| m.as_str()).unwrap_or(""),
            "code": diag.get("code").cloned().unwrap_or(json!(null)),
            "source": diag.get("source").and_then(|s| s.as_str()).unwrap_or("rust-analyzer"),
            "relatedInformation": diag.get("relatedInformation").cloned().unwrap_or(json!(null))
        }));
    }

    output["summary"]["errors"] = json!(errors);
    output["summary"]["warnings"] = json!(warnings);
    output["summary"]["information"] = json!(information);
    output["summary"]["hints"] = json!(hints);

    output
}

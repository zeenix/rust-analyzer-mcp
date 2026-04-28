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
            "codeDescription": diag.get("codeDescription").cloned().unwrap_or(json!(null)),
            "source": diag.get("source").and_then(|s| s.as_str()).unwrap_or("rust-analyzer"),
            "tags": diag.get("tags").cloned().unwrap_or(json!(null)),
            "relatedInformation": diag.get("relatedInformation").cloned().unwrap_or(json!(null)),
            "data": diag.get("data").cloned().unwrap_or(json!(null))
        }));
    }

    output["summary"]["errors"] = json!(errors);
    output["summary"]["warnings"] = json!(warnings);
    output["summary"]["information"] = json!(information);
    output["summary"]["hints"] = json!(hints);

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_diag(out: &Value) -> &Value {
        &out["diagnostics"][0]
    }

    #[test]
    fn passes_through_data_code_description_and_tags() {
        let raw = json!([{
            "severity": 1,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "message": "boom",
            "code": "E0382",
            "codeDescription": { "href": "https://doc.rust-lang.org/error-index.html#E0382" },
            "source": "rustc",
            "tags": [1],
            "data": { "rendered": "error[E0382]: ...\n  --> src/x.rs:1:1\n" }
        }]);

        let out = format_diagnostics("src/x.rs", &raw);
        let d = first_diag(&out);

        assert_eq!(d["severity"], "error");
        assert_eq!(d["code"], "E0382");
        assert_eq!(
            d["codeDescription"]["href"],
            "https://doc.rust-lang.org/error-index.html#E0382"
        );
        assert_eq!(d["tags"], json!([1]));
        assert!(d["data"]["rendered"]
            .as_str()
            .unwrap()
            .contains("error[E0382]"));
    }

    #[test]
    fn missing_optional_fields_become_null_not_absent() {
        let raw = json!([{
            "severity": 2,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } },
            "message": "warn",
            "source": "rustc"
        }]);

        let out = format_diagnostics("src/x.rs", &raw);
        let d = first_diag(&out);

        // Stable shape: keys exist, values are null when absent upstream.
        assert!(d.get("data").is_some());
        assert!(d["data"].is_null());
        assert!(d["codeDescription"].is_null());
        assert!(d["tags"].is_null());
        assert!(d["code"].is_null());
        assert!(d["relatedInformation"].is_null());
    }
}

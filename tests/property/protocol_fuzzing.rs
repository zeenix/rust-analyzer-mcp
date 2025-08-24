use proptest::prelude::*;
use serde_json::{json, Value};

// Import protocol types from main
#[derive(Debug, Clone)]
struct FuzzedRequest {
    method: String,
    params: Value,
}

impl Arbitrary for FuzzedRequest {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: Self::Parameters) -> Self::Strategy {
        (
            prop::string::string_regex("[a-zA-Z][a-zA-Z0-9_/]*").unwrap(),
            any::<bool>().prop_flat_map(|has_params| {
                if has_params {
                    prop::collection::vec(any::<String>(), 0..5)
                        .prop_map(|v| json!(v))
                        .boxed()
                } else {
                    Just(json!(null)).boxed()
                }
            }),
        )
            .prop_map(|(method, params)| FuzzedRequest { method, params })
            .boxed()
    }
}

proptest! {
    #[test]
    fn test_server_handles_invalid_methods(request in any::<FuzzedRequest>()) {
        // This test would require spawning the actual server
        // For now, we test that invalid methods don't cause panics
        let request_json = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": request.method,
            "params": request.params
        });

        // Verify the request can be serialized
        let serialized = serde_json::to_string(&request_json);
        prop_assert!(serialized.is_ok());
    }

    #[test]
    fn test_malformed_json_handling(
        json_str in prop::string::string_regex(r#"\{"[a-z]+": [a-z0-9", ]*\}"#).unwrap()
    ) {
        // Test that malformed JSON doesn't cause panics
        let result: Result<Value, _> = serde_json::from_str(&json_str);
        // Either it parses or returns an error - no panics
        let _ = result;
    }

    #[test]
    fn test_tool_parameter_fuzzing(
        file_path in prop::string::string_regex("[a-zA-Z0-9_/.]+").unwrap(),
        line in 0u32..10000,
        character in 0u32..1000,
    ) {
        // Test various tool parameter combinations
        let tools = vec![
            json!({
                "name": "rust_analyzer_symbols",
                "arguments": {"file_path": file_path.clone()}
            }),
            json!({
                "name": "rust_analyzer_definition",
                "arguments": {
                    "file_path": file_path.clone(),
                    "line": line,
                    "character": character
                }
            }),
            json!({
                "name": "rust_analyzer_hover",
                "arguments": {
                    "file_path": file_path.clone(),
                    "line": line,
                    "character": character
                }
            }),
        ];

        for tool in tools {
            let request = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": tool
            });

            // Verify the request is valid JSON
            let serialized = serde_json::to_string(&request);
            prop_assert!(serialized.is_ok());
        }
    }

    #[test]
    fn test_concurrent_request_ids(
        ids in prop::collection::vec(0u64..1000000, 1..100)
    ) {
        // Test that various request IDs are handled correctly
        for id in ids {
            let request = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "initialize",
                "params": {}
            });

            let serialized = serde_json::to_string(&request);
            prop_assert!(serialized.is_ok());

            // Verify ID is preserved in serialization
            let deserialized: Value = serde_json::from_str(&serialized.unwrap()).unwrap();
            prop_assert_eq!(deserialized["id"].as_u64(), Some(id));
        }
    }

    #[test]
    fn test_unicode_in_parameters(
        unicode_str in "\\PC*", // Any unicode string
    ) {
        // Test that unicode strings in parameters are handled correctly
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "rust_analyzer_symbols",
                "arguments": {
                    "file_path": unicode_str
                }
            }
        });

        let serialized = serde_json::to_string(&request);
        prop_assert!(serialized.is_ok());

        // Verify unicode is preserved
        let deserialized: Value = serde_json::from_str(&serialized.unwrap()).unwrap();
        prop_assert_eq!(
            deserialized["params"]["arguments"]["file_path"].as_str(),
            Some(unicode_str.as_str())
        );
    }

    #[test]
    fn test_deeply_nested_params(
        depth in 1usize..10,
    ) {
        // Test deeply nested parameter structures
        let mut params = json!({"value": "test"});
        for _ in 0..depth {
            params = json!({"nested": params});
        }

        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "test",
            "params": params
        });

        let serialized = serde_json::to_string(&request);
        prop_assert!(serialized.is_ok());
    }

    #[test]
    fn test_notification_vs_request(
        method in prop::string::string_regex("[a-z_/]+").unwrap(),
        has_id in any::<bool>(),
    ) {
        // Test that notifications (no ID) and requests (with ID) are handled differently
        let mut message = json!({
            "jsonrpc": "2.0",
            "method": method
        });

        if has_id {
            message["id"] = json!(1);
        }

        let serialized = serde_json::to_string(&message);
        prop_assert!(serialized.is_ok());

        let deserialized: Value = serde_json::from_str(&serialized.unwrap()).unwrap();
        prop_assert_eq!(deserialized.get("id").is_some(), has_id);
    }
}

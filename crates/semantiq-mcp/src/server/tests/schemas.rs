//! `tools/list` stays small and every `outputSchema` describes what the tool
//! really returns: one call per tool is validated against its schema.

use super::{create_test_server, index_test_file};
use crate::server::*;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use serde_json::{Map, Value};

/// Keywords the compact schemas may use. Anything else fails the test, so the
/// validator below never silently ignores a constraint.
const KNOWN_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "items",
    "$ref",
    "$defs",
    "description",
];

/// Validate `value` against `schema` (the subset of JSON Schema 2020-12 the
/// tools use). Objects must not carry properties the schema does not declare.
fn validate(value: &Value, schema: &Value, root: &Value, path: &str) -> Result<(), String> {
    let schema = schema
        .as_object()
        .ok_or_else(|| format!("{path}: schema is not an object"))?;
    for keyword in schema.keys() {
        if !KNOWN_KEYWORDS.contains(&keyword.as_str()) {
            return Err(format!("{path}: unexpected schema keyword {keyword}"));
        }
    }

    if let Some(reference) = schema.get("$ref") {
        let name = reference
            .as_str()
            .and_then(|r| r.strip_prefix("#/$defs/"))
            .ok_or_else(|| format!("{path}: unsupported $ref {reference}"))?;
        let target = root
            .get("$defs")
            .and_then(|defs| defs.get(name))
            .ok_or_else(|| format!("{path}: missing definition {name}"))?;
        return validate(value, target, root, path);
    }

    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
        _ => return Err(format!("{path}: schema without type")),
    };
    let matches = types.iter().any(|t| match *t {
        "string" => value.is_string(),
        "integer" => value.is_u64() || value.is_i64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => false,
    });
    if !matches {
        return Err(format!("{path}: {value} is not of type {types:?}"));
    }

    if let Value::Array(items) = value {
        let item_schema = schema
            .get("items")
            .ok_or_else(|| format!("{path}: array without items schema"))?;
        for (i, item) in items.iter().enumerate() {
            validate(item, item_schema, root, &format!("{path}[{i}]"))?;
        }
    }

    if let Value::Object(object) = value {
        let empty = Map::new();
        let properties = schema
            .get("properties")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        for required in schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = required.as_str().unwrap_or_default();
            if !object.contains_key(name) {
                return Err(format!("{path}: missing required property {name}"));
            }
        }
        for (key, field) in object {
            let field_schema = properties
                .get(key)
                .ok_or_else(|| format!("{path}: property {key} not in the schema"))?;
            validate(field, field_schema, root, &format!("{path}.{key}"))?;
        }
    }

    Ok(())
}

fn structured(result: Result<CallToolResult, String>) -> Value {
    let result = result.expect("tool call failed");
    assert_eq!(result.is_error, Some(false));
    result
        .structured_content
        .expect("tool result must carry structuredContent")
}

#[tokio::test]
async fn test_output_schemas_validate_real_outputs() {
    let (server, temp) = create_test_server();
    let files = [
        (
            "src/shapes.rs",
            "use crate::util::helper;\n\npub trait Shape {\n    fn area(&self) -> f64;\n}\n\npub struct Circle {\n    pub r: f64,\n}\n\nimpl Shape for Circle {\n    fn area(&self) -> f64 {\n        helper(self.r) * 3.14\n    }\n}\n\nfn orphan() -> u32 {\n    7\n}\n",
        ),
        (
            "src/util.rs",
            "/// Squares a number.\npub fn helper(x: f64) -> f64 {\n    x * x\n}\n\npub fn total(shapes: &[Circle]) -> f64 {\n    shapes.iter().map(|s| s.area()).sum::<f64>() + helper(1.0)\n}\n",
        ),
    ];
    for (path, content) in files {
        let full = temp.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
        index_test_file(&server.store, path, content, "rust");
    }

    let outputs = [
        (
            "semantiq_search",
            structured(
                server
                    .semantiq_search(Parameters(SearchParams {
                        query: "helper".to_string(),
                        snippets: Some(true),
                        ..Default::default()
                    }))
                    .await,
            ),
        ),
        (
            "semantiq_repo_map",
            structured(
                server
                    .semantiq_repo_map(Parameters(RepoMapParams {
                        focus: Some(vec!["src/util.rs".to_string(), "Nope".to_string()]),
                        ..Default::default()
                    }))
                    .await,
            ),
        ),
        (
            "semantiq_find_refs",
            structured(
                server
                    .semantiq_find_refs(Parameters(FindRefsParams {
                        symbol: "helper".to_string(),
                        limit: None,
                    }))
                    .await,
            ),
        ),
        (
            "semantiq_deps",
            structured(
                server
                    .semantiq_deps(Parameters(DepsParams {
                        file_path: "src/shapes.rs".to_string(),
                    }))
                    .await,
            ),
        ),
        (
            "semantiq_explain",
            structured(
                server
                    .semantiq_explain(Parameters(ExplainParams {
                        symbol: "helper".to_string(),
                    }))
                    .await,
            ),
        ),
        (
            "semantiq_impact",
            structured(
                server
                    .semantiq_impact(Parameters(ImpactParams {
                        symbol: "helper".to_string(),
                        ..Default::default()
                    }))
                    .await,
            ),
        ),
        (
            "semantiq_calls",
            structured(
                server
                    .semantiq_calls(Parameters(CallsParams {
                        symbol: "area".to_string(),
                        ..Default::default()
                    }))
                    .await,
            ),
        ),
        (
            "semantiq_hierarchy",
            structured(
                server
                    .semantiq_hierarchy(Parameters(HierarchyParams {
                        symbol: "Shape".to_string(),
                        ..Default::default()
                    }))
                    .await,
            ),
        ),
        (
            "semantiq_dead_code",
            structured(
                server
                    .semantiq_dead_code(Parameters(DeadCodeParams {
                        include_public: Some(true),
                        ..Default::default()
                    }))
                    .await,
            ),
        ),
    ];

    // Serialize the tool list as `tools/list` sends it.
    let tools = serde_json::to_value(SemantiqServer::tool_router().list_all()).unwrap();
    let tools = tools.as_array().unwrap();
    assert_eq!(tools.len(), outputs.len());

    for (name, output) in &outputs {
        let tool = tools
            .iter()
            .find(|t| t["name"] == *name)
            .unwrap_or_else(|| panic!("{name} not listed"));
        let schema = &tool["outputSchema"];
        assert_eq!(schema["type"], "object", "{name}");
        validate(output, schema, schema, name)
            .unwrap_or_else(|e| panic!("{name}: {e}\n{output:#}"));
    }

    // The fixture exercises the optional fields, not only empty lists.
    let search = &outputs[0].1;
    assert!(search["results"][0]["content"].is_string(), "{search:#}");
    let calls = &outputs[6].1;
    assert!(
        !calls["callers"].as_array().unwrap().is_empty(),
        "{calls:#}"
    );
    let hierarchy = &outputs[7].1;
    assert!(
        !hierarchy["subtypes"].as_array().unwrap().is_empty(),
        "{hierarchy:#}"
    );
    let dead = &outputs[8].1;
    assert!(!dead["symbols"].as_array().unwrap().is_empty(), "{dead:#}");
}

#[test]
fn test_tools_list_is_compact() {
    let tools = serde_json::to_value(SemantiqServer::tool_router().list_all()).unwrap();

    for tool in tools.as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        let description = tool["description"].as_str().unwrap();
        assert!(description.len() <= 250, "{name}: description too long");

        // Output fields are described by the tool, not one by one; generated
        // noise (formats, bounds, titles, `null` unions) is stripped.
        let output = tool["outputSchema"].to_string();
        assert!(!output.contains("\"description\""), "{name}: {output}");
        for noise in ["\"format\"", "\"minimum\"", "\"$schema\"", "\"null\""] {
            assert!(!output.contains(noise), "{name}: {noise} in {output}");
            let input = tool["inputSchema"].to_string();
            assert!(!input.contains(noise), "{name}: {noise} in {input}");
        }
    }

    // Measured at 11.9k characters for the 9 tools; 20.5k before compaction.
    let size = tools.to_string().len();
    assert!(size < 13_000, "tools/list grew to {size} characters");
}

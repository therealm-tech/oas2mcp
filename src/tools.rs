//! Mapping of OpenAPI operations to MCP tools, and execution of a tool call as
//! a proxied HTTP request to the upstream API.

use std::collections::HashSet;
use std::sync::Arc;

use openapiv3::{
    OpenAPI, Operation, Parameter, ParameterSchemaOrContent, PathItem, ReferenceOr, RequestBody,
    Schema,
};
use reqwest::Method;
use serde_json::{Map, Value, json};

#[cfg(test)]
use crate::filter::FilterConfig;
use crate::filter::OperationFilter;

/// Where an OpenAPI parameter is carried in the HTTP request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamLocation {
    Path,
    Query,
    Header,
}

/// A single OpenAPI parameter relevant to building/executing the request.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub location: ParamLocation,
}

/// A fully resolved OpenAPI operation, ready to be advertised as an MCP tool
/// and executed as an HTTP request.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: Option<String>,
    pub method: Method,
    /// Path template relative to the base URL, e.g. `/pets/{petId}`.
    pub path_template: String,
    pub params: Vec<Param>,
    /// Whether the operation accepts a JSON request body (the `body` argument).
    pub has_body: bool,
    /// The JSON Schema advertised to MCP clients as the tool input schema.
    pub input_schema: Arc<Map<String, Value>>,
}

/// Build one [`ToolSpec`] per operation defined in the document, keeping only
/// the operations the [`OperationFilter`] selects.
pub fn build_tools(spec: &OpenAPI, filter: &OperationFilter) -> Vec<ToolSpec> {
    let mut tools = Vec::new();
    let mut seen_names = HashSet::new();
    let mut filtered = 0usize;

    for (path, item) in &spec.paths.paths {
        let ReferenceOr::Item(item) = item else {
            tracing::warn!(%path, "skipping path defined by a $ref (unsupported)");
            continue;
        };

        for (method, operation) in operations(item) {
            if !filter.keeps(&operation_name(path, &method, operation), &operation.tags) {
                filtered += 1;
                continue;
            }

            let mut tool = match build_tool(spec, item, path, method.clone(), operation) {
                Ok(tool) => tool,
                Err(err) => {
                    tracing::warn!(%path, %method, error = %err, "skipping operation");
                    continue;
                }
            };

            // MCP tool names must be unique; disambiguate collisions.
            let mut name = tool.name.clone();
            let mut suffix = 2;
            while !seen_names.insert(name.clone()) {
                name = format!("{}_{suffix}", tool.name);
                suffix += 1;
            }
            tool.name = name;

            tracing::debug!(tool = %tool.name, %method, %path, "registered tool");
            tools.push(tool);
        }
    }

    if filtered > 0 {
        tracing::info!(
            kept = tools.len(),
            filtered,
            "filtered operations by the configured include/exclude rules"
        );
    }

    tools
}

/// Iterate the HTTP operations present on a path item.
fn operations(item: &PathItem) -> Vec<(Method, &Operation)> {
    [
        (Method::GET, &item.get),
        (Method::PUT, &item.put),
        (Method::POST, &item.post),
        (Method::DELETE, &item.delete),
        (Method::OPTIONS, &item.options),
        (Method::HEAD, &item.head),
        (Method::PATCH, &item.patch),
        (Method::TRACE, &item.trace),
    ]
    .into_iter()
    .filter_map(|(method, slot)| slot.as_ref().map(|op| (method, op)))
    .collect()
}

/// The MCP tool name for an operation: its `operationId`, or a synthesised
/// `<method>_<path>` fallback, sanitised to the allowed character set.
fn operation_name(path: &str, method: &Method, operation: &Operation) -> String {
    operation
        .operation_id
        .clone()
        .map(|id| sanitize_name(&id))
        .unwrap_or_else(|| sanitize_name(&format!("{}_{path}", method.as_str().to_lowercase())))
}

fn build_tool(
    spec: &OpenAPI,
    item: &PathItem,
    path: &str,
    method: Method,
    operation: &Operation,
) -> anyhow::Result<ToolSpec> {
    let name = operation_name(path, &method, operation);

    // Use the summary as a headline and the description as detail. Many specs
    // (e.g. GitLab) put the one-line "what it does" in `summary` and reserve
    // `description` for version/deprecation notes, so favouring one over the
    // other loses information; combine them when both are present.
    let description = match (&operation.summary, &operation.description) {
        (Some(summary), Some(detail)) => Some(format!("{summary}\n\n{detail}")),
        (Some(summary), None) => Some(summary.clone()),
        (None, detail) => detail.clone(),
    };

    // Path-item parameters apply to every operation; operation parameters win.
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut params = Vec::new();

    for param_ref in item.parameters.iter().chain(operation.parameters.iter()) {
        let ReferenceOr::Item(parameter) = param_ref else {
            // Resolve a referenced parameter from components.
            let Some(parameter) = resolve_parameter(spec, param_ref) else {
                continue;
            };
            push_param(spec, parameter, &mut properties, &mut required, &mut params);
            continue;
        };
        push_param(spec, parameter, &mut properties, &mut required, &mut params);
    }

    let has_body = add_request_body(spec, operation, &mut properties, &mut required);

    let mut input_schema = Map::new();
    input_schema.insert("type".into(), json!("object"));
    input_schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        input_schema.insert("required".into(), json!(required));
    }

    Ok(ToolSpec {
        name,
        description,
        method,
        path_template: path.to_string(),
        params,
        has_body,
        input_schema: Arc::new(input_schema),
    })
}

fn push_param(
    spec: &OpenAPI,
    parameter: &Parameter,
    properties: &mut Map<String, Value>,
    required: &mut Vec<String>,
    params: &mut Vec<Param>,
) {
    let (data, location) = match parameter {
        Parameter::Query { parameter_data, .. } => (parameter_data, ParamLocation::Query),
        Parameter::Path { parameter_data, .. } => (parameter_data, ParamLocation::Path),
        Parameter::Header { parameter_data, .. } => (parameter_data, ParamLocation::Header),
        // Cookie parameters are not proxied.
        Parameter::Cookie { parameter_data, .. } => {
            tracing::debug!(name = %parameter_data.name, "ignoring cookie parameter");
            return;
        }
    };

    // Path parameters are always required regardless of the document.
    let is_required = data.required || location == ParamLocation::Path;

    let mut schema = match &data.format {
        ParameterSchemaOrContent::Schema(schema) => {
            schema_to_json(spec, schema, &mut HashSet::new())
        }
        ParameterSchemaOrContent::Content(_) => json!({ "type": "string" }),
    };
    if let (Some(obj), Some(desc)) = (schema.as_object_mut(), data.description.as_ref())
        && !obj.contains_key("description")
    {
        obj.insert("description".into(), json!(desc));
    }

    properties.insert(data.name.clone(), schema);
    if is_required {
        required.push(data.name.clone());
    }
    params.push(Param {
        name: data.name.clone(),
        location,
    });
}

/// Add the JSON request body (if any) as a `body` property. Returns whether a
/// body is accepted.
fn add_request_body(
    spec: &OpenAPI,
    operation: &Operation,
    properties: &mut Map<String, Value>,
    required: &mut Vec<String>,
) -> bool {
    let Some(body_ref) = &operation.request_body else {
        return false;
    };
    let body: &RequestBody = match body_ref {
        ReferenceOr::Item(body) => body,
        ReferenceOr::Reference { .. } => {
            let Some(body) = resolve_request_body(spec, body_ref) else {
                return false;
            };
            body
        }
    };

    // We proxy JSON bodies only.
    let Some(media) = body
        .content
        .get("application/json")
        .or_else(|| body.content.first().map(|(_, m)| m))
    else {
        return false;
    };

    let schema = media
        .schema
        .as_ref()
        .map(|s| schema_to_json(spec, s, &mut HashSet::new()))
        .unwrap_or_else(|| json!({ "type": "object" }));

    properties.insert("body".into(), schema);
    if body.required {
        required.push("body".into());
    }
    true
}

/// Serialize an OpenAPI schema to a JSON Schema value, inlining local
/// `#/components/schemas` references so MCP clients need no extra context.
fn schema_to_json(
    spec: &OpenAPI,
    schema: &ReferenceOr<Schema>,
    seen: &mut HashSet<String>,
) -> Value {
    match schema {
        ReferenceOr::Reference { reference } => resolve_schema_ref(spec, reference, seen),
        ReferenceOr::Item(schema) => {
            let mut value = serde_json::to_value(schema).unwrap_or_else(|_| json!({}));
            inline_refs(spec, &mut value, seen);
            value
        }
    }
}

fn resolve_schema_ref(spec: &OpenAPI, reference: &str, seen: &mut HashSet<String>) -> Value {
    // Guard against recursive schemas: a second visit collapses to `object`.
    if !seen.insert(reference.to_string()) {
        return json!({ "type": "object" });
    }
    let resolved = ref_name(reference, "schemas")
        .and_then(|name| spec.components.as_ref()?.schemas.get(name))
        .map(|schema| schema_to_json(spec, schema, seen))
        .unwrap_or_else(|| json!({ "type": "object" }));
    seen.remove(reference);
    resolved
}

/// Recursively replace `{ "$ref": "#/components/schemas/X" }` nodes in a
/// serialized schema with the inlined target schema.
fn inline_refs(spec: &OpenAPI, value: &mut Value, seen: &mut HashSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref")
                && map.len() == 1
            {
                *value = resolve_schema_ref(spec, &reference.clone(), seen);
                return;
            }
            for child in map.values_mut() {
                inline_refs(spec, child, seen);
            }
        }
        Value::Array(items) => {
            for item in items {
                inline_refs(spec, item, seen);
            }
        }
        _ => {}
    }
}

fn resolve_parameter<'a>(
    spec: &'a OpenAPI,
    param: &ReferenceOr<Parameter>,
) -> Option<&'a Parameter> {
    let ReferenceOr::Reference { reference } = param else {
        return None;
    };
    let name = ref_name(reference, "parameters")?;
    match spec.components.as_ref()?.parameters.get(name)? {
        ReferenceOr::Item(parameter) => Some(parameter),
        ReferenceOr::Reference { .. } => None,
    }
}

fn resolve_request_body<'a>(
    spec: &'a OpenAPI,
    body: &ReferenceOr<RequestBody>,
) -> Option<&'a RequestBody> {
    let ReferenceOr::Reference { reference } = body else {
        return None;
    };
    let name = ref_name(reference, "requestBodies")?;
    match spec.components.as_ref()?.request_bodies.get(name)? {
        ReferenceOr::Item(body) => Some(body),
        ReferenceOr::Reference { .. } => None,
    }
}

/// Extract `X` from `#/components/<kind>/X`.
fn ref_name<'a>(reference: &'a str, kind: &str) -> Option<&'a str> {
    reference.strip_prefix(&format!("#/components/{kind}/"))
}

/// Turn an arbitrary string into a valid MCP tool name (`[A-Za-z0-9_-]+`).
fn sanitize_name(raw: &str) -> String {
    let mut name: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    name = name.trim_matches('_').to_string();
    if name.is_empty() {
        name = "operation".to_string();
    }
    name.truncate(64);
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_from(yaml: &str) -> OpenAPI {
        serde_yaml_ng::from_str(yaml).expect("valid spec")
    }

    const PETSTORE: &str = r##"
openapi: 3.0.0
info:
  title: Pets
  version: "1.0"
paths:
  /pets/{petId}:
    get:
      operationId: getPet
      parameters:
        - name: petId
          in: path
          required: true
          schema:
            type: string
        - name: verbose
          in: query
          schema:
            type: boolean
      responses:
        "200":
          description: ok
  /pets:
    post:
      operationId: createPet
      summary: Create a pet
      description: Introduced in 1.0.
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Pet"
      responses:
        "201":
          description: created
components:
  schemas:
    Pet:
      type: object
      required: [name]
      properties:
        name:
          type: string
        tag:
          type: string
"##;

    #[test]
    fn builds_one_tool_per_operation() {
        let tools = build_tools(&spec_from(PETSTORE), &OperationFilter::default());
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"getPet"));
        assert!(names.contains(&"createPet"));
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn path_param_is_required_and_present() {
        let tools = build_tools(&spec_from(PETSTORE), &OperationFilter::default());
        let get_pet = tools.iter().find(|t| t.name == "getPet").unwrap();
        let pet_id = get_pet.params.iter().find(|p| p.name == "petId").unwrap();
        assert_eq!(pet_id.location, ParamLocation::Path);
        let required = get_pet.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "petId"));
    }

    #[test]
    fn request_body_ref_is_inlined() {
        let tools = build_tools(&spec_from(PETSTORE), &OperationFilter::default());
        let create = tools.iter().find(|t| t.name == "createPet").unwrap();
        assert!(create.has_body);
        let body = &create.input_schema["properties"]["body"];
        // The $ref to Pet must be inlined, not left as a bare reference.
        assert!(body.get("$ref").is_none());
        assert_eq!(body["properties"]["name"]["type"], "string");
    }

    #[test]
    fn description_combines_summary_and_detail() {
        let tools = build_tools(&spec_from(PETSTORE), &OperationFilter::default());
        // Both present: summary headlines, description follows.
        let create = tools.iter().find(|t| t.name == "createPet").unwrap();
        assert_eq!(
            create.description.as_deref(),
            Some("Create a pet\n\nIntroduced in 1.0.")
        );
        // Neither summary nor description: stays None.
        let get_pet = tools.iter().find(|t| t.name == "getPet").unwrap();
        assert_eq!(get_pet.description, None);
    }

    #[test]
    fn description_falls_back_to_each_field() {
        const SPEC: &str = r##"
openapi: 3.0.0
info: { title: T, version: "1" }
paths:
  /a:
    get:
      operationId: onlySummary
      summary: Just a summary
      responses: { "200": { description: ok } }
  /b:
    get:
      operationId: onlyDescription
      description: Just a description
      responses: { "200": { description: ok } }
"##;
        let tools = build_tools(&spec_from(SPEC), &OperationFilter::default());
        let only_summary = tools.iter().find(|t| t.name == "onlySummary").unwrap();
        assert_eq!(only_summary.description.as_deref(), Some("Just a summary"));
        let only_desc = tools.iter().find(|t| t.name == "onlyDescription").unwrap();
        assert_eq!(only_desc.description.as_deref(), Some("Just a description"));
    }

    #[test]
    fn filter_restricts_the_built_tool_set() {
        let only_get = OperationFilter::new(FilterConfig {
            include_globs: vec!["getPet".into()],
            ..Default::default()
        });
        let tools = build_tools(&spec_from(PETSTORE), &only_get);
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["getPet"]);
    }

    #[test]
    fn sanitizes_operation_names() {
        assert_eq!(
            sanitize_name("get /pets/{petId}"),
            "get__pets__petId_".trim_matches('_')
        );
        assert_eq!(sanitize_name("//"), "operation");
    }
}

//! Mapping of OpenAPI operations to MCP tools, and execution of a tool call as
//! a proxied HTTP request to the upstream API.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;
use reqwest::Method;
use serde_json::{Map, Value, json};

#[cfg(test)]
use crate::filter::FilterConfig;
use crate::filter::OperationFilter;
use crate::openapi::Spec;
use crate::openapi::spec::{MediaType, Operation, Parameter, PathItem};
use crate::rename::{ToolRenamer, sanitize_name};

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
/// the operations the [`OperationFilter`] selects and naming each one through
/// the [`ToolRenamer`].
///
/// Order of operations, which the tests pin down: the raw name is derived from
/// the `operationId` (or the `<method>_<path>` fallback), **the filter matches
/// that raw name**, and only then is the name rewritten. Filtering before
/// renaming is deliberate — a deployment's curated allowlist of `operationId`s
/// keeps working unchanged when rename rules are added or edited.
pub fn build_tools(spec: &Spec, filter: &OperationFilter, renamer: &ToolRenamer) -> Vec<ToolSpec> {
    let mut tools = Vec::new();
    // Final name → the raw operation name that claimed it, so a collision can
    // name both sides rather than being resolved silently.
    let mut seen_names: HashMap<String, String> = HashMap::new();
    let mut filtered = 0usize;

    for (path, item) in spec.path_items() {
        for (method, operation) in operations(&item) {
            let raw = operation_name(path, &method, operation);
            if !filter.keeps(&raw, &operation.tags) {
                filtered += 1;
                continue;
            }

            let mut tool = build_tool(spec, &item, path, method.clone(), operation, raw.clone());
            rename(&mut tool, &raw, operation, renamer);

            // MCP tool names must be unique; disambiguate collisions.
            let mut name = tool.name.clone();
            let mut suffix = 2;
            while let Some(owner) = seen_names.get(&name) {
                tracing::warn!(
                    name = %name,
                    held_by = %owner,
                    operation = %raw,
                    "two operations claim the same tool name; disambiguating with a suffix"
                );
                name = format!("{}_{suffix}", tool.name);
                suffix += 1;
            }
            seen_names.insert(name.clone(), raw);
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

/// The raw MCP tool name for an operation: its `operationId`, or a synthesised
/// `<method>_<path>` fallback, sanitised to the allowed character set. This is
/// the name the [`OperationFilter`] matches, before any renaming.
fn operation_name(path: &str, method: &Method, operation: &Operation) -> String {
    operation
        .operation_id
        .clone()
        .map(|id| sanitize_name(&id))
        .unwrap_or_else(|| sanitize_name(&format!("{}_{path}", method.as_str().to_lowercase())))
}

/// Apply the rename rules to a built tool. A rewritten name keeps its origin in
/// the description: the model gets the short name, whoever reads a trace keeps
/// the mapping back to the `operationId`.
fn rename(tool: &mut ToolSpec, raw: &str, operation: &Operation, renamer: &ToolRenamer) {
    let renamed = renamer.rename(raw);
    if renamed == raw {
        return;
    }
    tracing::debug!(old = %raw, new = %renamed, "rewrote the tool name");

    if let Some(id) = &operation.operation_id {
        let origin = format!("OpenAPI operationId: {id}");
        tool.description = Some(match tool.description.take() {
            Some(description) => format!("{description}\n\n{origin}"),
            None => origin,
        });
    }
    tool.name = renamed;
}

fn build_tool(
    spec: &Spec,
    item: &PathItem,
    path: &str,
    method: Method,
    operation: &Operation,
    name: String,
) -> ToolSpec {
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
        let Some(parameter) = spec.resolve(param_ref) else {
            continue;
        };
        push_param(
            spec,
            &parameter,
            &mut properties,
            &mut required,
            &mut params,
        );
    }

    let has_body = add_request_body(spec, operation, &mut properties, &mut required);

    let mut input_schema = Map::new();
    input_schema.insert("type".into(), json!("object"));
    input_schema.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        input_schema.insert("required".into(), json!(required));
    }

    ToolSpec {
        name,
        description,
        method,
        path_template: path.to_string(),
        params,
        has_body,
        input_schema: Arc::new(input_schema),
    }
}

fn push_param(
    spec: &Spec,
    parameter: &Parameter,
    properties: &mut Map<String, Value>,
    required: &mut Vec<String>,
    params: &mut Vec<Param>,
) {
    let location = match parameter.location.as_str() {
        "path" => ParamLocation::Path,
        "query" => ParamLocation::Query,
        "header" => ParamLocation::Header,
        // Cookie parameters are not proxied.
        other => {
            tracing::debug!(name = %parameter.name, location = other, "ignoring parameter");
            return;
        }
    };

    // Path parameters are always required regardless of the document.
    let is_required = parameter.required || location == ParamLocation::Path;

    // A `content`-typed parameter carries a serialised media type rather than
    // a plain value; its schema still describes what the caller must supply.
    let raw_schema = parameter.schema.as_ref().or_else(|| {
        parameter
            .content
            .first()
            .and_then(|(_, m)| m.schema.as_ref())
    });
    let mut schema = match raw_schema {
        Some(schema) => inlined(spec, schema),
        None => json!({ "type": "string" }),
    };
    if let (Some(obj), Some(desc)) = (schema.as_object_mut(), parameter.description.as_ref())
        && !obj.contains_key("description")
    {
        obj.insert("description".into(), json!(desc));
    }

    properties.insert(parameter.name.clone(), schema);
    if is_required {
        required.push(parameter.name.clone());
    }
    params.push(Param {
        name: parameter.name.clone(),
        location,
    });
}

/// Add the JSON request body (if any) as a `body` property. Returns whether a
/// body is accepted.
fn add_request_body(
    spec: &Spec,
    operation: &Operation,
    properties: &mut Map<String, Value>,
    required: &mut Vec<String>,
) -> bool {
    let Some(body_ref) = &operation.request_body else {
        return false;
    };
    let Some(body) = spec.resolve(body_ref) else {
        return false;
    };
    let Some((media_type, media)) = json_content(&body.content) else {
        return false;
    };

    let schema = media
        .schema
        .as_ref()
        .map(|schema| inlined(spec, schema))
        .unwrap_or_else(|| json!({ "type": "object" }));

    properties.insert("body".into(), schema);
    if body.required {
        required.push("body".into());
    }
    tracing::debug!(media_type, "operation accepts a request body");
    true
}

/// Pick the `content` entry describing the request body: an exact
/// `application/json`, else any type with the `+json` structured suffix
/// (`application/merge-patch+json`, `application/vnd.api+json`, …), else
/// whatever comes first — the body is sent as JSON either way.
fn json_content(content: &IndexMap<String, MediaType>) -> Option<(&str, &MediaType)> {
    // Media types may carry parameters (`application/json; charset=utf-8`) and
    // are case-insensitive.
    let essence = |raw: &str| {
        raw.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
    };

    let exact = content
        .iter()
        .find(|(name, _)| essence(name) == "application/json");
    let suffixed = || {
        content
            .iter()
            .find(|(name, _)| essence(name).ends_with("+json"))
    };
    exact
        .or_else(suffixed)
        .or_else(|| content.first())
        .map(|(name, media)| (name.as_str(), media))
}

/// Copy a schema out of the document with its local `$ref`s inlined, so MCP
/// clients need no extra context. The schema is otherwise untouched: whatever
/// dialect the document uses — 3.0's modified draft-04 or 3.1's JSON Schema
/// 2020-12 — reaches the client as written.
fn inlined(spec: &Spec, schema: &Value) -> Value {
    let mut value = schema.clone();
    inline_refs(spec, &mut value, &mut HashSet::new());
    value
}

/// Recursively replace `{ "$ref": "#/components/schemas/X" }` nodes with the
/// schema they point at.
fn inline_refs(spec: &Spec, value: &mut Value, seen: &mut HashSet<String>) {
    match value {
        Value::Object(map) => {
            let reference = match map.get("$ref") {
                Some(Value::String(reference)) => reference.clone(),
                _ => {
                    for child in map.values_mut() {
                        inline_refs(spec, child, seen);
                    }
                    return;
                }
            };

            let siblings = std::mem::take(map);
            let mut resolved = resolve_schema_ref(spec, &reference, seen);

            // OpenAPI 3.1 — and JSON Schema 2020-12 generally — allow keywords
            // beside `$ref`, where they refine the referenced schema. OpenAPI
            // 3.0 forbade them, so this only ever fires on a 3.1 document.
            if siblings.len() > 1
                && let Some(target) = resolved.as_object_mut()
            {
                for (key, mut sibling) in siblings {
                    if key == "$ref" {
                        continue;
                    }
                    inline_refs(spec, &mut sibling, seen);
                    target.insert(key, sibling);
                }
            }

            *value = resolved;
        }
        Value::Array(items) => {
            for item in items {
                inline_refs(spec, item, seen);
            }
        }
        _ => {}
    }
}

fn resolve_schema_ref(spec: &Spec, reference: &str, seen: &mut HashSet<String>) -> Value {
    // Guard against recursive schemas: a second visit collapses to `object`.
    if !seen.insert(reference.to_string()) {
        tracing::debug!(reference, "collapsing a recursive schema reference");
        return json!({ "type": "object" });
    }
    let resolved = match spec.resolve_value(reference) {
        Some(target) => {
            let mut value = target.clone();
            inline_refs(spec, &mut value, seen);
            value
        }
        None => {
            tracing::debug!(
                reference,
                "substituting a bare object for an unresolved schema"
            );
            json!({ "type": "object" })
        }
    };
    seen.remove(reference);
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rename::{RenameConfig, RenameRule};

    fn spec_from(yaml: &str) -> Spec {
        let raw: Value = serde_yaml_ng::from_str(yaml).expect("valid YAML");
        Spec::from_value(raw).expect("supported document")
    }

    fn tools_from(yaml: &str) -> Vec<ToolSpec> {
        build_tools(
            &spec_from(yaml),
            &OperationFilter::default(),
            &ToolRenamer::default(),
        )
    }

    /// A renamer over the given `<regex>=<replacement>` rules.
    fn renamer(rules: &[&str]) -> ToolRenamer {
        ToolRenamer::new(RenameConfig {
            rules: rules
                .iter()
                .map(|raw| RenameRule::parse(raw).expect("valid rule"))
                .collect(),
            ..Default::default()
        })
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

    /// The same API as [`PETSTORE`], written as OpenAPI 3.1: a JSON Schema
    /// 2020-12 `type` array instead of `nullable`, and a `$ref` carrying a
    /// sibling `description`.
    const PETSTORE_31: &str = r##"
openapi: 3.1.0
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
      responses:
        "200":
          description: ok
  /pets:
    post:
      operationId: createPet
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: "#/components/schemas/Pet"
              description: The pet to create.
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
          type: [string, "null"]
        status:
          const: available
        legs:
          type: integer
          exclusiveMinimum: 0
        coords:
          type: array
          prefixItems:
            - type: number
            - type: number
        extras:
          additionalProperties: false
"##;

    #[test]
    fn builds_one_tool_per_operation() {
        let tools = tools_from(PETSTORE);
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"getPet"));
        assert!(names.contains(&"createPet"));
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn path_param_is_required_and_present() {
        let tools = tools_from(PETSTORE);
        let get_pet = tools.iter().find(|t| t.name == "getPet").unwrap();
        let pet_id = get_pet.params.iter().find(|p| p.name == "petId").unwrap();
        assert_eq!(pet_id.location, ParamLocation::Path);
        let required = get_pet.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "petId"));
    }

    #[test]
    fn request_body_ref_is_inlined() {
        let tools = tools_from(PETSTORE);
        let create = tools.iter().find(|t| t.name == "createPet").unwrap();
        assert!(create.has_body);
        let body = &create.input_schema["properties"]["body"];
        // The $ref to Pet must be inlined, not left as a bare reference.
        assert!(body.get("$ref").is_none());
        assert_eq!(body["properties"]["name"]["type"], "string");
    }

    #[test]
    fn description_combines_summary_and_detail() {
        let tools = tools_from(PETSTORE);
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
        let tools = tools_from(SPEC);
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
        let tools = build_tools(&spec_from(PETSTORE), &only_get, &ToolRenamer::default());
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["getPet"]);
    }

    #[test]
    fn sanitizes_operation_names() {
        assert_eq!(
            operation_name("/pets/{petId}", &Method::GET, &Operation::default()),
            "get__pets__petId"
        );
    }

    #[test]
    fn filters_match_the_name_before_renaming() {
        // The guarantee a curated allowlist of `operationId`s depends on: rename
        // rules never move the target the filter is aiming at.
        let only_get = OperationFilter::new(FilterConfig {
            include_globs: vec!["getPet".into()],
            ..Default::default()
        });
        let tools = build_tools(
            &spec_from(PETSTORE),
            &only_get,
            &renamer(&["^get=fetch_", "Pet=animal"]),
        );
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["fetch_animal"]);

        // And the reverse: a filter written against the *renamed* name matches
        // nothing, because renaming happens afterwards.
        let renamed_only = OperationFilter::new(FilterConfig {
            include_globs: vec!["fetch_animal".into()],
            ..Default::default()
        });
        let tools = build_tools(
            &spec_from(PETSTORE),
            &renamed_only,
            &renamer(&["^get=fetch_", "Pet=animal"]),
        );
        assert!(tools.is_empty());
    }

    #[test]
    fn a_renamed_tool_keeps_its_operation_id_in_the_description() {
        let tools = build_tools(
            &spec_from(PETSTORE),
            &OperationFilter::default(),
            &renamer(&["^create=new_"]),
        );
        let create = tools.iter().find(|t| t.name == "new_Pet").unwrap();
        assert_eq!(
            create.description.as_deref(),
            Some("Create a pet\n\nIntroduced in 1.0.\n\nOpenAPI operationId: createPet")
        );

        // A tool whose name survived the rules keeps its description untouched.
        let get_pet = tools.iter().find(|t| t.name == "getPet").unwrap();
        assert_eq!(get_pet.description, None);
    }

    #[test]
    fn colliding_renamed_names_are_disambiguated() {
        // Two distinct operations abbreviated onto the same name: both are still
        // exposed, and the second one carries a numeric suffix.
        const SPEC: &str = r##"
openapi: 3.1.0
info: { title: T, version: "1" }
paths:
  /a: { get: { operationId: getApiV4ProjectsIdIssues } }
  /b: { get: { operationId: getApiV4GroupsIdIssues } }
"##;
        let tools = build_tools(
            &spec_from(SPEC),
            &OperationFilter::default(),
            &renamer(&["^getApiV4(Projects|Groups)Id=get_"]),
        );
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["get_Issues", "get_Issues_2"]);
    }

    #[test]
    fn builds_the_same_tools_from_a_3_1_document() {
        let tools = tools_from(PETSTORE_31);
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"getPet"));
        assert!(names.contains(&"createPet"));
    }

    #[test]
    fn keeps_json_schema_2020_12_keywords_verbatim() {
        let tools = tools_from(PETSTORE_31);
        let create = tools.iter().find(|t| t.name == "createPet").unwrap();
        let properties = &create.input_schema["properties"]["body"]["properties"];

        // A nullable union type, which cannot be expressed in the 3.0 model.
        assert_eq!(properties["tag"]["type"], json!(["string", "null"]));
        // Keywords 3.0 has no place for at all.
        assert_eq!(properties["status"]["const"], "available");
        assert_eq!(properties["coords"]["prefixItems"][1]["type"], "number");
        // `exclusiveMinimum` is a number in 2020-12, a boolean in 3.0.
        assert_eq!(properties["legs"]["exclusiveMinimum"], json!(0));
        // A boolean schema is a schema in 2020-12.
        assert_eq!(properties["extras"]["additionalProperties"], json!(false));
    }

    #[test]
    fn keeps_keywords_written_beside_a_ref() {
        // 3.1 allows `$ref` siblings, which refine the referenced schema.
        let tools = tools_from(PETSTORE_31);
        let create = tools.iter().find(|t| t.name == "createPet").unwrap();
        let body = &create.input_schema["properties"]["body"];
        assert!(body.get("$ref").is_none());
        assert_eq!(body["description"], "The pet to create.");
        assert_eq!(body["type"], "object");
    }

    #[test]
    fn keeps_3_0_schemas_verbatim_too() {
        // `nullable` is 3.0's spelling and must survive untouched, rather than
        // being normalised away or dropped.
        const SPEC: &str = r##"
openapi: 3.0.3
info: { title: T, version: "1" }
paths:
  /a:
    get:
      operationId: getA
      parameters:
        - name: tag
          in: query
          schema: { type: string, nullable: true, minLength: 2 }
"##;
        let tools = tools_from(SPEC);
        let schema = &tools[0].input_schema["properties"]["tag"];
        assert_eq!(schema["nullable"], json!(true));
        assert_eq!(schema["minLength"], json!(2));
    }

    #[test]
    fn resolves_a_pointer_into_defs() {
        // 2020-12 keeps reusable subschemas in `$defs`; the pointer is resolved
        // against the document root, like any other local reference.
        const SPEC: &str = r##"
openapi: 3.1.0
info: { title: T, version: "1" }
paths:
  /a:
    post:
      operationId: postA
      requestBody:
        content:
          application/json:
            schema: { $ref: "#/$defs/Body" }
$defs:
  Body: { type: object, properties: { id: { type: string } } }
"##;
        let tools = tools_from(SPEC);
        let body = &tools[0].input_schema["properties"]["body"];
        assert_eq!(body["properties"]["id"]["type"], "string");
    }

    #[test]
    fn prefers_a_json_media_type_over_the_first_entry() {
        const SPEC: &str = r##"
openapi: 3.1.0
info: { title: T, version: "1" }
paths:
  /a:
    post:
      operationId: postA
      requestBody:
        content:
          text/plain:
            schema: { type: string }
          application/vnd.api+json:
            schema: { type: object, properties: { data: { type: object } } }
"##;
        let tools = tools_from(SPEC);
        let body = &tools[0].input_schema["properties"]["body"];
        assert_eq!(body["type"], "object");
        assert!(body["properties"].get("data").is_some());
    }

    #[test]
    fn shared_path_item_parameters_apply_to_every_operation() {
        const SPEC: &str = r##"
openapi: 3.1.0
info: { title: T, version: "1" }
paths:
  /pets/{petId}:
    parameters:
      - name: petId
        in: path
        required: true
        schema: { type: string }
    get: { operationId: getPet }
    delete: { operationId: deletePet }
"##;
        let tools = tools_from(SPEC);
        assert_eq!(tools.len(), 2);
        for tool in &tools {
            assert_eq!(tool.params.len(), 1);
            assert_eq!(tool.params[0].location, ParamLocation::Path);
        }
    }

    #[test]
    fn recursive_schemas_terminate() {
        const SPEC: &str = r##"
openapi: 3.1.0
info: { title: T, version: "1" }
paths:
  /a:
    post:
      operationId: postA
      requestBody:
        content:
          application/json:
            schema: { $ref: "#/components/schemas/Node" }
components:
  schemas:
    Node:
      type: object
      properties:
        children:
          type: array
          items: { $ref: "#/components/schemas/Node" }
"##;
        let tools = tools_from(SPEC);
        let body = &tools[0].input_schema["properties"]["body"];
        // The cycle collapses to a bare object rather than expanding forever.
        assert_eq!(
            body["properties"]["children"]["items"],
            json!({"type": "object"})
        );
    }

    #[test]
    fn ignores_cookie_parameters() {
        const SPEC: &str = r##"
openapi: 3.1.0
info: { title: T, version: "1" }
paths:
  /a:
    get:
      operationId: getA
      parameters:
        - { name: session, in: cookie, schema: { type: string } }
        - { name: q, in: query, schema: { type: string } }
"##;
        let tools = tools_from(SPEC);
        let names: Vec<_> = tools[0].params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["q"]);
    }
}

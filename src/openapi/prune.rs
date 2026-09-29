//! Cut an OpenAPI document down to a subset of its operations, so the copy a
//! caller reads never describes an operation that caller cannot list.

use std::collections::{BTreeSet, HashSet};

use serde_json::{Map, Value};

use super::Spec;

/// The operation keys of a path item that `oas2mcp` turns into tools. Any other
/// operation (OpenAPI 3.2's `query`, `additionalOperations`) is never a tool,
/// so it never survives pruning.
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// The non-operation fields of a path item kept beside its surviving
/// operations.
const PATH_ITEM_FIELDS: [&str; 4] = ["summary", "description", "servers", "parameters"];

/// A copy of the document holding only the operations `keeps(path, method)`
/// accepts, `method` in lower case.
///
/// - A path item left with no operation is dropped; a path-level `$ref` is
///   inlined, so the component it pointed at is not needed.
/// - `webhooks` are dropped: they are never tools.
/// - `tags` keep the entries a surviving operation carries.
/// - `components` keep what the rest of the document still references,
///   transitively: `$ref`s, discriminator mappings, and the security schemes a
///   `security` requirement names.
/// - Every other top-level field (`info`, `servers`, `security`, extensions) is
///   kept as is.
pub fn prune(spec: &Spec, keeps: impl Fn(&str, &str) -> bool) -> Value {
    let Some(root) = spec.raw().as_object() else {
        return spec.raw().clone();
    };

    let mut paths = Map::new();
    for (path, value) in root
        .get("paths")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if path.starts_with("x-") {
            continue;
        }
        let item = match value.get("$ref").and_then(Value::as_str) {
            Some(reference) => spec.resolve_chain(reference),
            None => Some(value),
        };
        let Some(item) = item.and_then(Value::as_object) else {
            continue;
        };
        let mut kept = Map::new();
        for field in PATH_ITEM_FIELDS {
            if let Some(value) = item.get(field) {
                kept.insert(field.to_string(), value.clone());
            }
        }
        let mut operations = 0usize;
        for method in METHODS {
            if let Some(operation) = item.get(method)
                && keeps(path, method)
            {
                kept.insert(method.to_string(), operation.clone());
                operations += 1;
            }
        }
        if operations > 0 {
            paths.insert(path.clone(), Value::Object(kept));
        }
    }

    let used_tags: HashSet<String> = paths
        .values()
        .filter_map(Value::as_object)
        .flat_map(|item| METHODS.iter().filter_map(|method| item.get(*method)))
        .filter_map(|operation| operation.get("tags").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();

    let mut out = Map::new();
    for (key, value) in root {
        match key.as_str() {
            "webhooks" | "components" => {}
            "paths" => {
                out.insert(key.clone(), Value::Object(std::mem::take(&mut paths)));
            }
            "tags" => {
                let tags = value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|tag| {
                        tag.get("name")
                            .and_then(Value::as_str)
                            .is_some_and(|name| used_tags.contains(name))
                    })
                    .cloned()
                    .collect();
                out.insert(key.clone(), Value::Array(tags));
            }
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    if !out.contains_key("paths") {
        out.insert("paths".to_string(), Value::Object(paths));
    }

    if let Some(components) = root.get("components").and_then(Value::as_object) {
        let pruned = reachable_components(components, &out);
        out.insert("components".to_string(), Value::Object(pruned));
    }

    Value::Object(out)
}

/// The components `document` reaches, directly or through other components.
fn reachable_components(
    components: &Map<String, Value>,
    document: &Map<String, Value>,
) -> Map<String, Value> {
    let mut refs = Refs::default();
    for value in document.values() {
        refs.collect(value);
    }

    let mut reached: BTreeSet<(String, String)> = BTreeSet::new();
    while let Some(next) = refs.pending.pop() {
        if !reached.insert(next.clone()) {
            continue;
        }
        if let Some(target) = components.get(&next.0).and_then(|kind| kind.get(&next.1)) {
            refs.collect(target);
        }
    }
    for scheme in &refs.security_schemes {
        reached.insert(("securitySchemes".to_string(), scheme.clone()));
    }

    let mut out = Map::new();
    for (kind, entries) in components {
        let Some(entries) = entries.as_object() else {
            continue;
        };
        let kept: Map<String, Value> = entries
            .iter()
            .filter(|(name, _)| reached.contains(&(kind.clone(), (*name).clone())))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        if !kept.is_empty() {
            out.insert(kind.clone(), Value::Object(kept));
        }
    }
    out
}

/// The component references found while walking a JSON value.
#[derive(Default)]
struct Refs {
    /// `(kind, name)` of each `#/components/<kind>/<name>` reference.
    pending: Vec<(String, String)>,
    /// Names used as keys of a `security` requirement object.
    security_schemes: BTreeSet<String>,
}

impl Refs {
    fn collect(&mut self, value: &Value) {
        match value {
            Value::Array(items) => items.iter().for_each(|item| self.collect(item)),
            Value::Object(object) => {
                for (key, value) in object {
                    match (key.as_str(), value) {
                        ("$ref", Value::String(reference)) => self.reference(reference),
                        ("security", Value::Array(requirements)) => {
                            for requirement in requirements.iter().filter_map(Value::as_object) {
                                self.security_schemes.extend(requirement.keys().cloned());
                            }
                        }
                        ("discriminator", Value::Object(discriminator)) => {
                            let mapping = discriminator.get("mapping").and_then(Value::as_object);
                            for target in mapping
                                .into_iter()
                                .flatten()
                                .filter_map(|(_, v)| v.as_str())
                            {
                                // A mapping value is a reference or a bare schema name.
                                if target.starts_with('#') {
                                    self.reference(target);
                                } else {
                                    self.pending
                                        .push(("schemas".to_string(), target.to_string()));
                                }
                            }
                        }
                        _ => {}
                    }
                    self.collect(value);
                }
            }
            _ => {}
        }
    }

    fn reference(&mut self, reference: &str) {
        let Some(pointer) = reference.strip_prefix("#/components/") else {
            return;
        };
        let mut segments = pointer.split('/');
        if let (Some(kind), Some(name)) = (segments.next(), segments.next()) {
            self.pending.push((unescape(kind), unescape(name)));
        }
    }
}

/// Undo the JSON pointer escapes (RFC 6901 §4).
fn unescape(segment: &str) -> String {
    segment.replace("~1", "/").replace("~0", "~")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_from(yaml: &str) -> Spec {
        let raw: Value = serde_yaml_ng::from_str(yaml).expect("valid YAML");
        Spec::from_value(raw).expect("supported document")
    }

    const DOC: &str = r##"
openapi: 3.1.0
info: { title: T, version: "1" }
tags:
  - { name: pets }
  - { name: admin, description: the secret back office }
paths:
  /pets:
    parameters: [{ $ref: "#/components/parameters/Tenant" }]
    get:
      operationId: listPets
      tags: [pets]
      security: [{ oauth: [] }]
      responses:
        "200":
          description: ok
          content: { application/json: { schema: { $ref: "#/components/schemas/Pets" } } }
    delete:
      operationId: purgePets
      tags: [admin]
      responses: { "204": { description: gone } }
  /admin: { $ref: "#/components/pathItems/Admin" }
webhooks:
  newPet: { post: { responses: { "200": { description: ok } } } }
components:
  parameters:
    Tenant: { name: tenant, in: header, schema: { type: string } }
  schemas:
    Pets: { type: array, items: { $ref: "#/components/schemas/Pet" } }
    Pet:
      oneOf: [{ $ref: "#/components/schemas/Dog" }]
      discriminator: { propertyName: kind, mapping: { dog: Dog } }
    Dog: { type: object }
    AuditLog: { type: object }
  pathItems:
    Admin:
      get:
        operationId: readAudit
        responses:
          "200":
            description: ok
            content: { application/json: { schema: { $ref: "#/components/schemas/AuditLog" } } }
  securitySchemes:
    oauth: { type: oauth2, flows: {} }
    adminKey: { type: apiKey, in: header, name: X-Admin }
"##;

    #[test]
    fn keeps_only_the_accepted_operations_and_what_they_reference() {
        let doc = prune(&spec_from(DOC), |path, method| {
            path == "/pets" && method == "get"
        });

        let expected: Value = serde_yaml_ng::from_str(
            r##"
openapi: 3.1.0
info: { title: T, version: "1" }
tags: [{ name: pets }]
paths:
  /pets:
    parameters: [{ $ref: "#/components/parameters/Tenant" }]
    get:
      operationId: listPets
      tags: [pets]
      security: [{ oauth: [] }]
      responses:
        "200":
          description: ok
          content: { application/json: { schema: { $ref: "#/components/schemas/Pets" } } }
components:
  parameters:
    Tenant: { name: tenant, in: header, schema: { type: string } }
  schemas:
    Pets: { type: array, items: { $ref: "#/components/schemas/Pet" } }
    Pet:
      oneOf: [{ $ref: "#/components/schemas/Dog" }]
      discriminator: { propertyName: kind, mapping: { dog: Dog } }
    Dog: { type: object }
  securitySchemes:
    oauth: { type: oauth2, flows: {} }
"##,
        )
        .expect("valid YAML");
        assert_eq!(doc, expected);
    }

    #[test]
    fn a_referenced_path_item_is_inlined_and_its_component_dropped() {
        let doc = prune(&spec_from(DOC), |path, _| path == "/admin");

        assert_eq!(
            doc.pointer("/paths/~1admin/get/operationId"),
            Some(&Value::from("readAudit"))
        );
        assert!(doc.pointer("/paths/~1admin/$ref").is_none());
        assert!(doc.pointer("/components/pathItems").is_none());
        assert!(doc.pointer("/components/schemas/AuditLog").is_some());
        assert!(doc.pointer("/paths/~1pets").is_none());
    }

    #[test]
    fn nothing_accepted_leaves_an_empty_but_valid_document() {
        let doc = prune(&spec_from(DOC), |_, _| false);

        assert_eq!(doc["paths"], Value::Object(Map::new()));
        assert_eq!(doc["tags"], Value::Array(Vec::new()));
        assert_eq!(doc["components"], Value::Object(Map::new()));
        assert!(doc.get("webhooks").is_none());
        assert_eq!(doc["info"]["title"], "T");
    }
}

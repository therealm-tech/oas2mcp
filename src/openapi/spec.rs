//! A version-agnostic view of an OpenAPI document, covering 3.0 and 3.1.
//!
//! `oas2mcp` needs only a narrow slice of an OpenAPI document: the API title
//! and version, the server list and, per operation, its identity, parameters
//! and request body. OpenAPI 3.0 and 3.1 describe that slice with the same
//! shapes; where they genuinely diverge is the *schema* dialect — 3.0 uses a
//! modified JSON Schema draft-04 (`nullable`, boolean `exclusiveMinimum`), 3.1
//! uses JSON Schema 2020-12 verbatim (`type` arrays, `const`, `prefixItems`,
//! numeric `exclusiveMinimum`, boolean schemas).
//!
//! So schemas are deliberately *not* modelled in Rust: a typed model forces a
//! choice of dialect and silently drops every keyword it does not know, which
//! is exactly the information an MCP client needs. They are kept as raw JSON
//! and advertised as written, with local `$ref`s inlined. The whole document
//! is retained as a [`Value`] so that any local JSON pointer resolves against
//! it — `#/components/schemas/…` and `#/$defs/…` alike.

use std::borrow::Cow;

use anyhow::bail;
use indexmap::IndexMap;
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// How many `$ref` hops to follow before giving up, so a reference cycle
/// between components cannot spin forever.
const MAX_REF_HOPS: usize = 16;

/// A parsed OpenAPI 3.x document.
#[derive(Debug, Clone)]
pub struct Spec {
    /// The document as raw JSON. Kept whole so local `$ref` pointers resolve
    /// against it and so schemas reach MCP clients exactly as written.
    raw: Value,
    /// The typed fields needed outside `paths` — all small.
    header: Header,
}

/// The document-level fields the server reads directly.
#[derive(Debug, Clone, Default, Deserialize)]
struct Header {
    #[serde(default)]
    openapi: String,
    #[serde(default)]
    info: Info,
    #[serde(default)]
    servers: Vec<Server>,
}

/// The `info` object, reduced to what the MCP instructions string needs.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Info {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub version: String,
}

/// A `servers` entry. Only the URL is used, as the upstream base URL.
#[derive(Debug, Clone, Deserialize)]
pub struct Server {
    pub url: String,
}

/// A value that may be given as a local `$ref` instead of inline.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RefOr<T> {
    /// `{ "$ref": "#/components/…" }`. OpenAPI 3.1 also allows `summary` and
    /// `description` beside `$ref` on this object; neither is used here.
    Ref(Ref),
    Item(T),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Ref {
    #[serde(rename = "$ref")]
    pub reference: String,
}

/// A path item: the operations defined on one path, plus the parameters they
/// all share. Both 3.0 and 3.1 allow the item itself to be a `$ref` (3.1 adds
/// `components.pathItems` as a target for it).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PathItem {
    #[serde(rename = "$ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub parameters: Vec<RefOr<Parameter>>,
    pub get: Option<Operation>,
    pub put: Option<Operation>,
    pub post: Option<Operation>,
    pub delete: Option<Operation>,
    pub options: Option<Operation>,
    pub head: Option<Operation>,
    pub patch: Option<Operation>,
    pub trace: Option<Operation>,
}

/// One operation on a path — the unit that becomes an MCP tool.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub operation_id: Option<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<RefOr<Parameter>>,
    pub request_body: Option<RefOr<RequestBody>>,
}

/// A path, query, header or cookie parameter.
#[derive(Debug, Clone, Deserialize)]
pub struct Parameter {
    pub name: String,
    /// `path`, `query`, `header` or `cookie`.
    #[serde(rename = "in")]
    pub location: String,
    #[serde(default)]
    pub required: bool,
    pub description: Option<String>,
    /// The raw JSON Schema, in whichever dialect the document uses.
    pub schema: Option<Value>,
    /// The alternative to `schema`, for parameters carrying a serialised
    /// media type rather than a simple value.
    #[serde(default)]
    pub content: IndexMap<String, MediaType>,
}

/// A request body. Only its JSON content is proxied.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RequestBody {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub content: IndexMap<String, MediaType>,
}

/// One entry of a `content` map.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaType {
    /// The raw JSON Schema, in whichever dialect the document uses.
    pub schema: Option<Value>,
}

impl Spec {
    /// Build a [`Spec`] from an already-decoded document, rejecting anything
    /// that is not OpenAPI 3.x.
    pub fn from_value(raw: Value) -> anyhow::Result<Self> {
        let header = Header::deserialize(&raw)
            .map_err(|err| anyhow::anyhow!("reading the document's top-level fields: {err}"))?;
        check_version(&raw, &header.openapi)?;

        // 3.1 documents may describe only `webhooks`, and may omit `paths`
        // entirely. A webhook is an inbound callback the upstream API sends to
        // *us*, not an operation we can call, so it never becomes a tool.
        if let Some(count) = raw
            .get("webhooks")
            .and_then(Value::as_object)
            .map(serde_json::Map::len)
            && count > 0
        {
            tracing::debug!(
                webhooks = count,
                "ignoring the document's webhooks: they are inbound callbacks, not callable operations"
            );
        }

        Ok(Self { raw, header })
    }

    /// The `openapi` version string, e.g. `3.1.0`.
    pub fn version(&self) -> &str {
        &self.header.openapi
    }

    pub fn info(&self) -> &Info {
        &self.header.info
    }

    pub fn servers(&self) -> &[Server] {
        &self.header.servers
    }

    /// The document's path items, in document order, with a path-level `$ref`
    /// already resolved. Extension keys (`x-…`) and unparseable entries are
    /// skipped with a log line rather than failing the whole document — a
    /// third-party spec with one bad corner should still yield its other tools.
    pub fn path_items(&self) -> impl Iterator<Item = (&str, PathItem)> {
        self.raw
            .get("paths")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(move |(path, value)| {
                if path.starts_with("x-") {
                    return None;
                }
                let item = match PathItem::deserialize(value) {
                    Ok(item) => item,
                    Err(err) => {
                        tracing::warn!(%path, error = %err, "skipping unparseable path item");
                        return None;
                    }
                };
                let Some(reference) = &item.reference else {
                    return Some((path.as_str(), item));
                };
                match self.resolve_as::<PathItem>(reference) {
                    Some(target) => Some((path.as_str(), target)),
                    None => {
                        tracing::warn!(
                            %path,
                            reference,
                            "skipping path: its `$ref` does not resolve to a path item"
                        );
                        None
                    }
                }
            })
    }

    /// Resolve a [`RefOr`] to the item it denotes, borrowing when it is
    /// already inline. `None` when the reference is external or dangling.
    pub fn resolve<'a, T>(&'a self, target: &'a RefOr<T>) -> Option<Cow<'a, T>>
    where
        T: Clone + DeserializeOwned,
    {
        match target {
            RefOr::Item(item) => Some(Cow::Borrowed(item)),
            RefOr::Ref(Ref { reference }) => self.resolve_as::<T>(reference).map(Cow::Owned),
        }
    }

    /// Resolve `reference` and deserialize the target, following `$ref` chains.
    fn resolve_as<T: DeserializeOwned>(&self, reference: &str) -> Option<T> {
        let mut target = self.resolve_value(reference)?;
        // A component may itself be a bare `$ref` to another component.
        for _ in 0..MAX_REF_HOPS {
            let Some(next) = target.get("$ref").and_then(Value::as_str) else {
                break;
            };
            target = self.resolve_value(next)?;
        }
        match T::deserialize(target) {
            Ok(item) => Some(item),
            Err(err) => {
                tracing::warn!(reference, error = %err, "ignoring a `$ref` whose target has the wrong shape");
                None
            }
        }
    }

    /// Look a local `$ref` up as a JSON pointer into the document. External
    /// references (another file or a URL) are not fetched, so they yield
    /// `None`.
    pub fn resolve_value(&self, reference: &str) -> Option<&Value> {
        let Some(pointer) = reference.strip_prefix('#') else {
            tracing::debug!(
                reference,
                "ignoring an external `$ref`: only local references are resolved"
            );
            return None;
        };
        // `#` alone points at the document root; `Value::pointer` already
        // handles the `~0`/`~1` escapes, but not percent-encoding.
        let pointer = percent_decode_str(pointer).decode_utf8_lossy();
        self.raw.pointer(&pointer)
    }
}

/// Reject documents this tool cannot read, and warn about ones it can only
/// read partially. Anything in the 3.x line is parsed on a best-effort basis:
/// the fields used here have been stable across 3.0, 3.1 and 3.2.
fn check_version(raw: &Value, openapi: &str) -> anyhow::Result<()> {
    if openapi.is_empty() {
        if raw.get("swagger").is_some() {
            bail!(
                "this is a Swagger 2.0 document; convert it to OpenAPI 3.x first \
                 (e.g. with `swagger2openapi`) and point --openapi-file/--openapi-url at the result"
            );
        }
        bail!("the document has no `openapi` version field, so it is not an OpenAPI 3.x document");
    }

    let mut parts = openapi.split('.');
    let major = parts.next().unwrap_or_default();
    let minor = parts.next().unwrap_or_default();
    if major != "3" {
        bail!("unsupported OpenAPI version `{openapi}`: only 3.0.x and 3.1.x are supported");
    }
    if !matches!(minor, "0" | "1") {
        tracing::warn!(
            version = openapi,
            "unrecognised OpenAPI 3.x minor version; reading it as 3.1, so anything the newer \
             revision added may be ignored"
        );
    }
    tracing::debug!(version = openapi, "parsed the OpenAPI document");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_from(yaml: &str) -> Spec {
        let raw: Value = serde_yaml_ng::from_str(yaml).expect("valid YAML");
        Spec::from_value(raw).expect("supported document")
    }

    #[test]
    fn reads_the_header_of_both_versions() {
        for version in ["3.0.3", "3.1.0"] {
            let spec = spec_from(&format!(
                r##"
openapi: "{version}"
info: {{ title: Pets, version: "1.0" }}
servers: [{{ url: "https://api.example.com" }}]
paths: {{}}
"##
            ));
            assert_eq!(spec.version(), version);
            assert_eq!(spec.info().title, "Pets");
            assert_eq!(spec.info().version, "1.0");
            assert_eq!(spec.servers()[0].url, "https://api.example.com");
        }
    }

    #[test]
    fn rejects_swagger_2_with_a_pointed_message() {
        let raw: Value = serde_yaml_ng::from_str("swagger: '2.0'\ninfo: {}").expect("valid YAML");
        let err = Spec::from_value(raw).expect_err("Swagger 2.0 is rejected");
        assert!(format!("{err}").contains("Swagger 2.0"), "{err}");
    }

    #[test]
    fn rejects_a_document_without_a_version() {
        let raw: Value = serde_yaml_ng::from_str("info: { title: T }").expect("valid YAML");
        assert!(Spec::from_value(raw).is_err());
    }

    #[test]
    fn rejects_a_non_3_x_version() {
        let raw: Value = serde_yaml_ng::from_str("openapi: '4.0.0'").expect("valid YAML");
        let err = Spec::from_value(raw).expect_err("4.x is rejected");
        assert!(format!("{err}").contains("4.0.0"), "{err}");
    }

    #[test]
    fn accepts_a_newer_3_x_minor() {
        // Forward-tolerant: 3.2 keeps the shapes this tool reads.
        let spec = spec_from(
            r##"
openapi: "3.2.0"
info: { title: T, version: "1" }
paths:
  /a: { get: { operationId: getA } }
"##,
        );
        assert_eq!(spec.path_items().count(), 1);
    }

    #[test]
    fn accepts_a_3_1_document_without_paths() {
        // 3.1 makes `paths` optional: a document may describe only webhooks.
        let spec = spec_from(
            r##"
openapi: "3.1.0"
info: { title: T, version: "1" }
webhooks:
  newPet:
    post:
      operationId: newPet
"##,
        );
        assert_eq!(spec.path_items().count(), 0);
    }

    #[test]
    fn skips_path_level_extensions_and_bad_entries() {
        let spec = spec_from(
            r##"
openapi: "3.1.0"
info: { title: T, version: "1" }
paths:
  x-internal: "not a path item"
  /good: { get: { operationId: getGood } }
  /bad: "also not a path item"
"##,
        );
        let paths: Vec<_> = spec.path_items().map(|(path, _)| path).collect();
        assert_eq!(paths, vec!["/good"]);
    }

    #[test]
    fn resolves_a_path_item_ref() {
        // 3.1 adds `components.pathItems` as the target of a path-level `$ref`.
        let spec = spec_from(
            r##"
openapi: "3.1.0"
info: { title: T, version: "1" }
paths:
  /pets: { $ref: "#/components/pathItems/pets" }
components:
  pathItems:
    pets:
      get: { operationId: listPets }
"##,
        );
        let (path, item) = spec.path_items().next().expect("one path");
        assert_eq!(path, "/pets");
        assert_eq!(
            item.get.expect("a GET operation").operation_id.as_deref(),
            Some("listPets")
        );
    }

    #[test]
    fn resolves_a_parameter_ref_through_a_chain() {
        let spec = spec_from(
            r##"
openapi: "3.1.0"
info: { title: T, version: "1" }
paths:
  /a:
    get:
      operationId: getA
      parameters:
        - $ref: "#/components/parameters/Alias"
components:
  parameters:
    Alias: { $ref: "#/components/parameters/Real" }
    Real:
      name: petId
      in: path
      required: true
      schema: { type: string }
"##,
        );
        let (_, item) = spec.path_items().next().expect("one path");
        let operation = item.get.expect("a GET operation");
        let parameter = spec
            .resolve(&operation.parameters[0])
            .expect("the chained reference resolves");
        assert_eq!(parameter.name, "petId");
        assert_eq!(parameter.location, "path");
    }

    #[test]
    fn external_and_dangling_refs_resolve_to_nothing() {
        let spec = spec_from(
            r##"
openapi: "3.1.0"
info: { title: T, version: "1" }
paths: {}
"##,
        );
        assert!(spec.resolve_value("common.yaml#/Pet").is_none());
        assert!(spec.resolve_value("#/components/schemas/Missing").is_none());
    }

    #[test]
    fn resolves_a_pointer_with_escapes_and_percent_encoding() {
        let spec = spec_from(
            r##"
openapi: "3.1.0"
info: { title: T, version: "1" }
paths: {}
components:
  schemas:
    "Pet Owner": { type: object }
"##,
        );
        // `%20` for the space, which generators emit in `$ref` URIs.
        assert!(
            spec.resolve_value("#/components/schemas/Pet%20Owner")
                .is_some()
        );
    }
}

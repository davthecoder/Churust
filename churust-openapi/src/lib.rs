//! OpenAPI 3.1 descriptions for [Churust] applications.
//!
//! The router already knows every path and method that exists. This crate turns
//! that inventory into an OpenAPI document, and gives you somewhere to hang the
//! parts a router cannot know: what an operation is for, what it accepts, and
//! what it answers with.
//!
//! ```
//! use churust_core::{Call, Churust, TestClient};
//! use churust_openapi::{OpenApi, Operation};
//! use http::Method;
//!
//! # tokio::runtime::Runtime::new().unwrap().block_on(async {
//! // 1. Register the application's own routes.
//! let builder = Churust::server().routing(|r| {
//!     r.get("/users/{id}", |_c: Call| async { "a user" });
//!     r.post("/users", |_c: Call| async { "created" });
//! });
//!
//! // 2. Describe them. Paths and parameters come from the router; the prose
//! //    and the responses come from you.
//! let api = OpenApi::new("Users API", "1.0.0")
//!     .from_routes(builder.routes())
//!     .operation(
//!         Method::GET,
//!         "/users/{id}",
//!         Operation::new()
//!             .summary("Fetch one user")
//!             .response(200, "The user")
//!             .response(404, "No such user"),
//!     );
//!
//! // 3. Serve it.
//! let app = builder.routing(|r| api.mount(r, "/openapi.json")).build();
//!
//! let res = TestClient::new(app).get("/openapi.json").send().await;
//! assert_eq!(res.header("content-type"), Some("application/json"));
//! assert!(res.text().contains("Fetch one user"));
//! # });
//! ```
//!
//! # What is generated and what is not
//!
//! Generated from the router: every path, every method on it, and a path
//! parameter for each `{name}` in the pattern. Those cannot drift from the
//! application, because they *are* the application.
//!
//! Not generated: request and response schemas. Churust handlers are ordinary
//! Rust functions and their extractor types are erased by the time a router
//! exists, so anything claiming to infer a schema here would be inferring it
//! from an annotation you wrote anyway. Describing an operation is explicit,
//! and the [`schema`](Operation::schema) helpers take a `serde_json::Value` so
//! a schema derived by any other crate drops straight in.
//!
//! # Drift
//!
//! A description of a route that no longer exists is worse than no description
//! at all. [`undescribed`](OpenApi::undescribed) and
//! [`stale`](OpenApi::stale) report both directions of drift, so a test can
//! fail the build when they disagree rather than a client finding out.
//!
//! [Churust]: churust_core::Churust

#![deny(missing_docs)]

use churust_core::{Call, IntoResponse, Response, RouteBuilder};
use http::header::CONTENT_TYPE;
use http::{HeaderValue, Method};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// One documented operation: a method on a path.
///
/// Everything is optional. An operation with nothing set still appears in the
/// document, which is the point of seeding from the router: an undocumented
/// endpoint shows up as an endpoint rather than as nothing.
#[derive(Debug, Clone, Default)]
pub struct Operation {
    summary: Option<String>,
    description: Option<String>,
    operation_id: Option<String>,
    tags: Vec<String>,
    deprecated: bool,
    request_body: Option<(String, Option<Value>, bool)>,
    responses: BTreeMap<u16, (String, Option<Value>)>,
    query_params: Vec<(String, bool, Option<String>)>,
}

impl Operation {
    /// An operation with nothing described yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// A one-line summary, shown as the operation's title in most viewers.
    pub fn summary(mut self, text: impl Into<String>) -> Self {
        self.summary = Some(text.into());
        self
    }

    /// A longer description. CommonMark is allowed by the specification.
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    /// An explicit `operationId`.
    ///
    /// Client generators use it for the generated method name. Left unset, one
    /// is derived from the method and path, which is stable as long as the
    /// route is.
    pub fn operation_id(mut self, id: impl Into<String>) -> Self {
        self.operation_id = Some(id.into());
        self
    }

    /// Group the operation under a tag.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Mark the operation deprecated.
    pub fn deprecated(mut self) -> Self {
        self.deprecated = true;
        self
    }

    /// Document a query parameter.
    pub fn query(mut self, name: impl Into<String>, required: bool) -> Self {
        self.query_params.push((name.into(), required, None));
        self
    }

    /// Document a query parameter with a description.
    pub fn query_described(
        mut self,
        name: impl Into<String>,
        required: bool,
        description: impl Into<String>,
    ) -> Self {
        self.query_params
            .push((name.into(), required, Some(description.into())));
        self
    }

    /// Document the request body's media type, with no schema.
    pub fn accepts(mut self, media_type: impl Into<String>) -> Self {
        self.request_body = Some((media_type.into(), None, true));
        self
    }

    /// Document the request body with a JSON Schema.
    ///
    /// The schema is any `serde_json::Value`, so one produced by `schemars` or
    /// written by hand fits equally well.
    pub fn schema(mut self, media_type: impl Into<String>, schema: Value) -> Self {
        self.request_body = Some((media_type.into(), Some(schema), true));
        self
    }

    /// Document a response status and what it means.
    pub fn response(mut self, status: u16, description: impl Into<String>) -> Self {
        self.responses.insert(status, (description.into(), None));
        self
    }

    /// Document a response with a JSON Schema for its body.
    pub fn response_schema(
        mut self,
        status: u16,
        description: impl Into<String>,
        schema: Value,
    ) -> Self {
        self.responses
            .insert(status, (description.into(), Some(schema)));
        self
    }
}

/// An OpenAPI 3.1 document under construction.
#[derive(Debug, Clone)]
pub struct OpenApi {
    title: String,
    version: String,
    description: Option<String>,
    servers: Vec<(String, Option<String>)>,
    /// Registered routes, in the order the router saw them.
    routes: Vec<(Method, String)>,
    /// Descriptions, keyed by the route they describe.
    operations: BTreeMap<(String, String), Operation>,
}

impl OpenApi {
    /// A document for an API with this title and version.
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
            description: None,
            servers: Vec::new(),
            routes: Vec::new(),
            operations: BTreeMap::new(),
        }
    }

    /// Describe the API as a whole.
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    /// Add a server URL clients should send requests to.
    pub fn server(mut self, url: impl Into<String>, description: Option<&str>) -> Self {
        self.servers
            .push((url.into(), description.map(str::to_string)));
        self
    }

    /// Seed the document from the router's inventory.
    ///
    /// Pass [`AppBuilder::routes`](churust_core::AppBuilder::routes) after the
    /// application's own routes are registered. Every route becomes an
    /// operation, whether or not it is described further.
    pub fn from_routes(mut self, routes: &[(Method, String)]) -> Self {
        for (method, path) in routes {
            // `HEAD` and `OPTIONS` are answered automatically by the engine for
            // every route, so listing them would describe the framework rather
            // than the API.
            if method == Method::HEAD || method == Method::OPTIONS {
                continue;
            }
            if !self
                .routes
                .iter()
                .any(|r| r == &(method.clone(), path.clone()))
            {
                self.routes.push((method.clone(), path.clone()));
            }
        }
        self
    }

    /// Attach a description to one route.
    ///
    /// Describing a route that was never registered is allowed while building,
    /// and reported by [`stale`](Self::stale) rather than refused here, so a
    /// document can be assembled in any order.
    pub fn operation(mut self, method: Method, path: &str, operation: Operation) -> Self {
        self.operations
            .insert((path.to_string(), method.as_str().to_string()), operation);
        self
    }

    /// Registered routes with no description attached.
    ///
    /// ```
    /// use churust_core::{Call, Churust};
    /// use churust_openapi::OpenApi;
    /// use http::Method;
    ///
    /// let builder = Churust::server().routing(|r| {
    ///     r.get("/health", |_c: Call| async { "ok" });
    /// });
    /// let api = OpenApi::new("API", "1.0").from_routes(builder.routes());
    /// assert_eq!(api.undescribed(), vec![(Method::GET, "/health".to_string())]);
    /// ```
    pub fn undescribed(&self) -> Vec<(Method, String)> {
        self.routes
            .iter()
            .filter(|(method, path)| {
                !self
                    .operations
                    .contains_key(&(path.clone(), method.as_str().to_string()))
            })
            .cloned()
            .collect()
    }

    /// Descriptions whose route no longer exists.
    ///
    /// The direction of drift that actually misleads a client: a documented
    /// endpoint that answers `404`.
    pub fn stale(&self) -> Vec<(Method, String)> {
        self.operations
            .keys()
            .filter(|(path, method)| {
                !self
                    .routes
                    .iter()
                    .any(|(m, p)| m.as_str() == method && p == path)
            })
            .filter_map(|(path, method)| {
                Method::from_bytes(method.as_bytes())
                    .ok()
                    .map(|m| (m, path.clone()))
            })
            .collect()
    }

    /// Render the document as JSON.
    pub fn to_value(&self) -> Value {
        let mut paths = Map::new();

        for (method, path) in &self.routes {
            let documented_path = openapi_path(path);
            let entry = paths
                .entry(documented_path.clone())
                .or_insert_with(|| json!({}));
            let Some(item) = entry.as_object_mut() else {
                continue;
            };

            let described = self
                .operations
                .get(&(path.clone(), method.as_str().to_string()));
            item.insert(
                method.as_str().to_ascii_lowercase(),
                self.operation_value(method, path, described),
            );
        }

        let mut info = json!({
            "title": self.title,
            "version": self.version,
        });
        if let (Some(description), Some(map)) = (&self.description, info.as_object_mut()) {
            map.insert("description".into(), json!(description));
        }

        let mut doc = json!({
            // 3.1.0 rather than 3.0.x: it is the first version whose schema
            // dialect is plain JSON Schema, which is what makes a schema from
            // any other crate usable here without translation.
            "openapi": "3.1.0",
            "info": info,
            "paths": paths,
        });

        if !self.servers.is_empty() {
            let servers: Vec<Value> = self
                .servers
                .iter()
                .map(|(url, description)| match description {
                    Some(d) => json!({ "url": url, "description": d }),
                    None => json!({ "url": url }),
                })
                .collect();
            if let Some(map) = doc.as_object_mut() {
                map.insert("servers".into(), json!(servers));
            }
        }

        doc
    }

    /// One operation object.
    fn operation_value(&self, method: &Method, path: &str, described: Option<&Operation>) -> Value {
        let mut out = Map::new();
        let default = Operation::default();
        let op = described.unwrap_or(&default);

        out.insert(
            "operationId".into(),
            json!(op
                .operation_id
                .clone()
                .unwrap_or_else(|| derive_operation_id(method, path))),
        );
        if let Some(summary) = &op.summary {
            out.insert("summary".into(), json!(summary));
        }
        if let Some(description) = &op.description {
            out.insert("description".into(), json!(description));
        }
        if !op.tags.is_empty() {
            out.insert("tags".into(), json!(op.tags));
        }
        if op.deprecated {
            out.insert("deprecated".into(), json!(true));
        }

        let mut parameters: Vec<Value> = path_parameters(path);
        for (name, required, description) in &op.query_params {
            let mut param = json!({
                "name": name,
                "in": "query",
                "required": required,
                "schema": { "type": "string" },
            });
            if let (Some(d), Some(map)) = (description, param.as_object_mut()) {
                map.insert("description".into(), json!(d));
            }
            parameters.push(param);
        }
        if !parameters.is_empty() {
            out.insert("parameters".into(), json!(parameters));
        }

        if let Some((media_type, schema, required)) = &op.request_body {
            let content = match schema {
                Some(schema) => json!({ media_type.clone(): { "schema": schema } }),
                None => json!({ media_type.clone(): {} }),
            };
            out.insert(
                "requestBody".into(),
                json!({ "required": required, "content": content }),
            );
        }

        let mut responses = Map::new();
        if op.responses.is_empty() {
            // Every operation must carry at least one response. An undescribed
            // one says only that it answers, which is true and is better than
            // a document that fails validation.
            responses.insert(
                "default".into(),
                json!({ "description": "Undocumented response" }),
            );
        }
        for (status, (description, schema)) in &op.responses {
            let value = match schema {
                Some(schema) => json!({
                    "description": description,
                    "content": { "application/json": { "schema": schema } },
                }),
                None => json!({ "description": description }),
            };
            responses.insert(status.to_string(), value);
        }
        out.insert("responses".into(), json!(responses));

        Value::Object(out)
    }

    /// Register a route serving this document as JSON.
    ///
    /// Call it inside a [`routing`](churust_core::AppBuilder::routing) block
    /// *after* the routes being described, so the inventory is complete:
    ///
    /// ```
    /// # use churust_core::{Call, Churust};
    /// # use churust_openapi::OpenApi;
    /// let builder = Churust::server().routing(|r| {
    ///     r.get("/health", |_c: Call| async { "ok" });
    /// });
    /// let api = OpenApi::new("API", "1.0").from_routes(builder.routes());
    /// let app = builder.routing(|r| api.mount(r, "/openapi.json")).build();
    /// # let _ = app;
    /// ```
    ///
    /// The document is rendered once, here, rather than per request: it cannot
    /// change after the router is built.
    pub fn mount(&self, routes: &mut RouteBuilder, path: &str) {
        let rendered = self.to_value().to_string();
        routes.get(path, move |_c: Call| {
            let body = rendered.clone();
            async move { JsonDocument(body) }
        });
    }
}

/// A pre-rendered JSON body.
///
/// A newtype rather than `Response::bytes`, because the content type has to be
/// `application/json` and `Response::bytes` takes a `&'static str` that a
/// runtime-built string cannot satisfy.
struct JsonDocument(String);

impl IntoResponse for JsonDocument {
    fn into_response(self) -> Response {
        let mut res = Response::text(self.0);
        res.headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        res
    }
}

/// Convert a Churust pattern to an OpenAPI path template.
///
/// `{id}` is already the OpenAPI spelling. A trailing `{rest...}` has no
/// OpenAPI equivalent, since the specification has no notion of a parameter
/// that spans slashes, so it is written as an ordinary `{rest}` and documented
/// as covering the remaining path.
fn openapi_path(pattern: &str) -> String {
    pattern.replace("...}", "}")
}

/// The path parameters implied by a pattern.
fn path_parameters(pattern: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for segment in pattern.split('/') {
        let Some(inner) = segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
            continue;
        };
        let (name, spans_slashes) = match inner.strip_suffix("...") {
            Some(name) => (name, true),
            None => (inner, false),
        };
        let mut param = json!({
            "name": name,
            "in": "path",
            // A path parameter is required by definition: without it the path
            // is a different path.
            "required": true,
            "schema": { "type": "string" },
        });
        if spans_slashes {
            if let Some(map) = param.as_object_mut() {
                map.insert(
                    "description".into(),
                    json!("Matches the remaining path, including slashes."),
                );
            }
        }
        out.push(param);
    }
    out
}

/// A stable `operationId` from the method and path.
///
/// `GET /users/{id}` becomes `getUsersById`. Derived rather than random so the
/// same route yields the same identifier on every build, which is what stops a
/// generated client from churning.
fn derive_operation_id(method: &Method, path: &str) -> String {
    let mut out = method.as_str().to_ascii_lowercase();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        match segment.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(param) => {
                let name = param.trim_end_matches("...");
                out.push_str("By");
                out.push_str(&capitalize(name));
            }
            None => out.push_str(&capitalize(segment)),
        }
    }
    out
}

/// Upper-case the first character, leaving the rest alone.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routes() -> Vec<(Method, String)> {
        vec![
            (Method::GET, "/users/{id}".to_string()),
            (Method::POST, "/users".to_string()),
            (Method::GET, "/files/{path...}".to_string()),
        ]
    }

    #[test]
    fn every_route_becomes_an_operation() {
        let doc = OpenApi::new("API", "1.0").from_routes(&routes()).to_value();
        let paths = doc["paths"].as_object().unwrap();

        assert!(paths.contains_key("/users/{id}"));
        assert!(paths.contains_key("/users"));
        assert!(paths["/users/{id}"].get("get").is_some());
        assert!(paths["/users"].get("post").is_some());
    }

    #[test]
    fn a_wildcard_becomes_an_ordinary_parameter() {
        let doc = OpenApi::new("API", "1.0").from_routes(&routes()).to_value();
        let paths = doc["paths"].as_object().unwrap();

        assert!(
            paths.contains_key("/files/{path}"),
            "OpenAPI has no slash-spanning parameter: {:?}",
            paths.keys().collect::<Vec<_>>()
        );
        let params = paths["/files/{path}"]["get"]["parameters"]
            .as_array()
            .unwrap();
        assert_eq!(params[0]["name"], "path");
        assert!(params[0]["description"]
            .as_str()
            .unwrap()
            .contains("including slashes"));
    }

    #[test]
    fn path_parameters_are_derived_and_required() {
        let doc = OpenApi::new("API", "1.0").from_routes(&routes()).to_value();
        let params = doc["paths"]["/users/{id}"]["get"]["parameters"]
            .as_array()
            .unwrap();

        assert_eq!(params.len(), 1);
        assert_eq!(params[0]["name"], "id");
        assert_eq!(params[0]["in"], "path");
        assert_eq!(params[0]["required"], true);
    }

    #[test]
    fn an_undescribed_operation_still_carries_a_response() {
        let doc = OpenApi::new("API", "1.0").from_routes(&routes()).to_value();
        let responses = doc["paths"]["/users"]["post"]["responses"]
            .as_object()
            .unwrap();
        assert!(
            responses.contains_key("default"),
            "an operation with no responses fails validation"
        );
    }

    #[test]
    fn a_description_reaches_the_document() {
        let doc = OpenApi::new("API", "1.0")
            .from_routes(&routes())
            .operation(
                Method::GET,
                "/users/{id}",
                Operation::new()
                    .summary("Fetch one user")
                    .tag("users")
                    .response(200, "The user")
                    .response(404, "No such user"),
            )
            .to_value();

        let op = &doc["paths"]["/users/{id}"]["get"];
        assert_eq!(op["summary"], "Fetch one user");
        assert_eq!(op["tags"][0], "users");
        assert_eq!(op["responses"]["200"]["description"], "The user");
        assert_eq!(op["responses"]["404"]["description"], "No such user");
        assert!(op["responses"].get("default").is_none());
    }

    #[test]
    fn a_schema_is_carried_through_unchanged() {
        let schema = json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let doc = OpenApi::new("API", "1.0")
            .from_routes(&routes())
            .operation(
                Method::POST,
                "/users",
                Operation::new().schema("application/json", schema.clone()),
            )
            .to_value();

        assert_eq!(
            doc["paths"]["/users"]["post"]["requestBody"]["content"]["application/json"]["schema"],
            schema
        );
    }

    #[test]
    fn operation_ids_are_derived_and_stable() {
        assert_eq!(
            derive_operation_id(&Method::GET, "/users/{id}"),
            "getUsersById"
        );
        assert_eq!(derive_operation_id(&Method::POST, "/users"), "postUsers");
        assert_eq!(
            derive_operation_id(&Method::GET, "/files/{path...}"),
            "getFilesByPath"
        );
    }

    #[test]
    fn head_and_options_are_not_described() {
        let mut routes = routes();
        routes.push((Method::HEAD, "/users".to_string()));
        routes.push((Method::OPTIONS, "/users".to_string()));

        let doc = OpenApi::new("API", "1.0").from_routes(&routes).to_value();
        assert!(doc["paths"]["/users"].get("head").is_none());
        assert!(doc["paths"]["/users"].get("options").is_none());
    }

    #[test]
    fn drift_is_reported_in_both_directions() {
        let api = OpenApi::new("API", "1.0")
            .from_routes(&routes())
            .operation(Method::GET, "/users/{id}", Operation::new())
            .operation(Method::DELETE, "/gone", Operation::new());

        let undescribed = api.undescribed();
        assert!(undescribed.contains(&(Method::POST, "/users".to_string())));
        assert!(!undescribed.contains(&(Method::GET, "/users/{id}".to_string())));

        assert_eq!(api.stale(), vec![(Method::DELETE, "/gone".to_string())]);
    }

    #[test]
    fn servers_and_description_are_included_when_set() {
        let doc = OpenApi::new("API", "1.0")
            .description("Everything about users")
            .server("https://api.example.com", Some("production"))
            .from_routes(&routes())
            .to_value();

        assert_eq!(doc["info"]["description"], "Everything about users");
        assert_eq!(doc["servers"][0]["url"], "https://api.example.com");
        assert_eq!(doc["servers"][0]["description"], "production");
    }

    #[test]
    fn the_document_declares_openapi_3_1() {
        let doc = OpenApi::new("API", "1.0").to_value();
        assert_eq!(doc["openapi"], "3.1.0");
        assert_eq!(doc["info"]["title"], "API");
        assert_eq!(doc["info"]["version"], "1.0");
    }
}

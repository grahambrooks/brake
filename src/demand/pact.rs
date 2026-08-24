//! Pact v2/v3/v4 HTTP interactions → [`Demand`].
//!
//! Bytes only, like every contract ingester, and it knows nothing about the
//! provider it constrains: a pact's paths are concrete and a contract's are
//! templates, so resolving `/payments/42` to `GET /payments/{id}` needs the
//! contract and belongs in [`super::bind`], not here.
//!
//! **A green brake run is not a passing pact verification.** This checks that
//! the *specification* still satisfies what consumers declared. Whether the
//! implementation matches its own specification is what `--drift` and the
//! provider's test suite are for.
//!
//! A URL anywhere in the document — `_links`, `pb:publish`, an `http://` ref
//! inside an example body — is data. It is never dereferenced, under any flag.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::{Demand, Route, Usage, UsageKind};
use crate::contract::{Constraints, Field, Span, TypeRef, Unmodelled, UnmodelledKind};

/// Matchers whose semantics brake does not model.
///
/// Named rather than ignored: an interaction brake half-understood reaching
/// the verdict as silence is the failure mode the whole tool exists to avoid.
const UNMODELLED_MATCHERS: &[&str] = &["arrayContains", "values", "contentType", "semver"];

const DEFAULT_MEDIA: &str = "application/json";

/// Ingest one pact document.
///
/// # Errors
///
/// Returns a message when the bytes are not UTF-8, are not JSON, or are JSON
/// that is not a pact. The last is what keeps `consumer-undeclared` honest:
/// identification is by parsing, so an arbitrary JSON file in the tree is not
/// called a pact.
pub fn ingest(source: &str, bytes: &[u8]) -> Result<Demand, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("`{source}` is not valid UTF-8: {error}"))?;
    let document: Value = serde_json::from_str(text)
        .map_err(|error| format!("`{source}` is not valid JSON: {error}"))?;

    let consumer = name_of(&document, "consumer")
        .ok_or_else(|| format!("`{source}` has no `consumer.name`, so it is not a pact"))?;
    let provider = name_of(&document, "provider")
        .ok_or_else(|| format!("`{source}` has no `provider.name`, so it is not a pact"))?;

    let interactions = document.get("interactions").and_then(Value::as_array);
    let messages = document.get("messages").and_then(Value::as_array);
    if interactions.is_none() && messages.is_none() {
        return Err(format!(
            "`{source}` names a consumer and a provider but has no `interactions`, \
             so it is not a pact"
        ));
    }

    let positions = Positions::index(text);
    let mut usages = BTreeSet::new();
    let mut unmodelled = Vec::new();

    for (index, interaction) in interactions.into_iter().flatten().enumerate() {
        let pointer = format!("/interactions/{index}");
        read_interaction(
            source,
            &positions,
            &pointer,
            interaction,
            &mut usages,
            &mut unmodelled,
        );
    }

    // A message pact constrains a broker topic, and brake has no topic model.
    // Reported, never ignored.
    for (index, message) in messages.into_iter().flatten().enumerate() {
        let pointer = format!("/messages/{index}");
        unmodelled.push(Unmodelled {
            kind: UnmodelledKind::Unsupported(format!(
                "message interaction `{}` — a message pact constrains a broker topic, \
                 which brake does not model",
                message
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("<undescribed>")
            )),
            span: positions.span(source, &pointer),
            pointer,
        });
    }

    Ok(Demand {
        consumer,
        provider,
        source: source.to_owned(),
        usages,
        unmodelled,
    })
}

fn name_of(document: &Value, key: &str) -> Option<String> {
    document
        .get(key)?
        .get("name")?
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
}

fn read_interaction(
    source: &str,
    positions: &Positions,
    pointer: &str,
    interaction: &Value,
    usages: &mut BTreeSet<Usage>,
    unmodelled: &mut Vec<Unmodelled>,
) {
    let span = positions.span(source, pointer);

    // v4 tags every interaction with its kind; v2 and v3 do not, and an HTTP
    // interaction is the one with a `request`.
    if let Some(kind) = interaction.get("type").and_then(Value::as_str)
        && !kind.eq_ignore_ascii_case("Synchronous/HTTP")
    {
        unmodelled.push(Unmodelled {
            kind: UnmodelledKind::Unsupported(format!(
                "`{kind}` interaction — brake models synchronous HTTP only"
            )),
            pointer: pointer.to_owned(),
            span,
        });
        return;
    }

    let Some(request) = interaction.get("request") else {
        unmodelled.push(Unmodelled {
            kind: UnmodelledKind::InvalidShape,
            pointer: pointer.to_owned(),
            span,
        });
        return;
    };

    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET");
    let Some(path) = request.get("path").and_then(Value::as_str) else {
        unmodelled.push(Unmodelled {
            kind: UnmodelledKind::InvalidShape,
            pointer: pointer.to_owned(),
            span,
        });
        return;
    };
    let route = Route::new(method, path);

    for detail in unmodelled_matchers(interaction) {
        unmodelled.push(Unmodelled {
            kind: UnmodelledKind::Unsupported(detail),
            pointer: pointer.to_owned(),
            span: span.clone(),
        });
    }

    usages.insert(Usage {
        route: route.clone(),
        kind: UsageKind::Endpoint,
        span: span.clone(),
    });

    read_query(&route, request.get("query"), &span, usages);
    read_headers(&route, request.get("headers"), &span, usages);

    if let Some(body) = request.get("body") {
        usages.insert(Usage {
            route: route.clone(),
            kind: UsageKind::Request {
                media_type: media_type_of(request.get("headers"), "content-type"),
                ty: type_of(body),
            },
            span: positions.span(source, &format!("{pointer}/request/body")),
        });
    }

    let Some(response) = interaction.get("response") else {
        return;
    };
    let status = response
        .get("status")
        .and_then(Value::as_u64)
        .map_or_else(|| "200".to_owned(), |status| status.to_string());
    if let Some(body) = response.get("body") {
        usages.insert(Usage {
            route,
            kind: UsageKind::Response {
                status,
                media_type: media_type_of(response.get("headers"), "content-type"),
                // Every present field is `required`: pact's own verification
                // fails if the provider omits a field the consumer expected,
                // so presence in the example is a declaration that the field
                // must keep being produced.
                ty: type_of(body),
            },
            span: positions.span(source, &format!("{pointer}/response/body")),
        });
    } else {
        usages.insert(Usage {
            route,
            kind: UsageKind::Response {
                status,
                media_type: media_type_of(response.get("headers"), "content-type"),
                ty: TypeRef::Object {
                    fields: BTreeMap::new(),
                    additional: true,
                    nullable: false,
                },
            },
            span,
        });
    }
}

/// v2 records the query as a string, v3 and v4 as a map of lists.
fn read_query(route: &Route, query: Option<&Value>, span: &Span, usages: &mut BTreeSet<Usage>) {
    match query {
        Some(Value::String(raw)) => {
            for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                usages.insert(Usage {
                    route: route.clone(),
                    kind: UsageKind::Parameter {
                        name: name.to_owned(),
                        location: "query".to_owned(),
                        value: Some(value.to_owned()),
                    },
                    span: span.clone(),
                });
            }
        }
        Some(Value::Object(entries)) => {
            for (name, values) in entries {
                usages.insert(Usage {
                    route: route.clone(),
                    kind: UsageKind::Parameter {
                        name: name.clone(),
                        location: "query".to_owned(),
                        value: first_string(values),
                    },
                    span: span.clone(),
                });
            }
        }
        _ => {}
    }
}

fn read_headers(route: &Route, headers: Option<&Value>, span: &Span, usages: &mut BTreeSet<Usage>) {
    let Some(Value::Object(entries)) = headers else {
        return;
    };
    for (name, value) in entries {
        // Content negotiation is a media type, not a parameter; it is read as
        // one by `media_type_of`.
        if name.eq_ignore_ascii_case("content-type") || name.eq_ignore_ascii_case("accept") {
            continue;
        }
        usages.insert(Usage {
            route: route.clone(),
            kind: UsageKind::Parameter {
                name: name.clone(),
                location: "header".to_owned(),
                value: first_string(value),
            },
            span: span.clone(),
        });
    }
}

fn first_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => items.first().and_then(first_string),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn media_type_of(headers: Option<&Value>, header: &str) -> String {
    headers
        .and_then(Value::as_object)
        .and_then(|entries| {
            entries
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(header))
                .and_then(|(_, value)| first_string(value))
        })
        .unwrap_or_else(|| DEFAULT_MEDIA.to_owned())
}

/// Matchers brake cannot model, named with what they were.
///
/// Both the v2 spelling (`matchingRules` keyed by JSON path) and the v3/v4 one
/// (keyed by section, then path, then a `matchers` list) are walked, because a
/// repository routinely holds both.
fn unmodelled_matchers(interaction: &Value) -> Vec<String> {
    let mut found = BTreeSet::new();
    let mut walk = |value: &Value| {
        let mut stack = vec![value.clone()];
        while let Some(current) = stack.pop() {
            match current {
                Value::Object(entries) => {
                    if let Some(Value::String(kind)) = entries.get("match")
                        && UNMODELLED_MATCHERS.contains(&kind.as_str())
                    {
                        found.insert(format!(
                            "`{kind}` matcher, whose semantics brake does not model"
                        ));
                    }
                    if let Some(Value::String(plugin)) = entries.get("pluginConfiguration") {
                        found.insert(format!("plugin-backed content `{plugin}`"));
                    }
                    stack.extend(entries.into_values());
                }
                Value::Array(items) => stack.extend(items),
                _ => {}
            }
        }
    };

    for section in ["matchingRules", "pluginConfiguration"] {
        for holder in [interaction.get("request"), interaction.get("response")]
            .into_iter()
            .flatten()
            .chain(std::iter::once(interaction))
        {
            if let Some(rules) = holder.get(section) {
                walk(rules);
            }
        }
    }
    found.into_iter().collect()
}

/// A recorded example body, as a type.
///
/// A pact records an *example* response body, so a field appearing in it is
/// evidence the consumer reads it — good evidence, since pact's own
/// verification fails when the field is absent, but evidence. A GraphQL
/// selection set is a statement; this is not, and §12.1 of the design records
/// the open question of whether whole-payload pastes over-declare.
fn type_of(value: &Value) -> TypeRef {
    match value {
        Value::Null => TypeRef::Scalar {
            ty: "null".to_owned(),
            format: None,
            nullable: true,
            constraints: Constraints::default(),
        },
        Value::Bool(_) => scalar("boolean"),
        Value::Number(number) => scalar(if number.is_f64() { "number" } else { "integer" }),
        Value::String(_) => scalar("string"),
        Value::Array(items) => TypeRef::Array {
            items: Box::new(items.first().map_or(
                // An empty array says nothing about its items. Reconciliation
                // takes the contract's, rather than reporting a difference the
                // consumer never declared.
                TypeRef::Unknown(UnmodelledKind::SchemaDeferred),
                type_of,
            )),
            nullable: false,
        },
        Value::Object(entries) => TypeRef::Object {
            fields: entries
                .iter()
                .map(|(name, value)| (name.clone(), Field::new(type_of(value), true)))
                .collect(),
            // The consumer's example is not a statement that nothing else may
            // appear, so the expectation never closes an object.
            additional: true,
            nullable: false,
        },
    }
}

fn scalar(ty: &str) -> TypeRef {
    TypeRef::Scalar {
        ty: ty.to_owned(),
        format: None,
        nullable: false,
        constraints: Constraints::default(),
    }
}

// ── source positions ────────────────────────────────────────────────────────

/// Where each JSON pointer's value begins.
///
/// `serde_json` discards positions, and a finding that says
/// `pacts/web-checkout-payments.json` without a line is a finding somebody has
/// to go looking for. This is the same trade the OpenAPI ingester makes with
/// `saphyr`, arrived at from the other direction: the document is scanned once
/// for locations while `serde_json` supplies the values.
pub struct Positions {
    at: BTreeMap<String, (usize, usize)>,
}

impl Positions {
    /// Index every value in a JSON document by pointer.
    #[must_use]
    pub fn index(text: &str) -> Self {
        let mut scanner = Scanner {
            bytes: text.as_bytes(),
            offset: 0,
            line: 1,
            column: 1,
            at: BTreeMap::new(),
        };
        scanner.value(String::new());
        Self { at: scanner.at }
    }

    /// The span of a pointer, falling back to the document's start.
    ///
    /// A fallback rather than an error: a location brake could not pin down is
    /// worth reporting at line 1 of the right file, and is never worth losing
    /// the finding over.
    #[must_use]
    pub fn span(&self, source: &str, pointer: &str) -> Span {
        let (line, column) = self.at.get(pointer).copied().unwrap_or((1, 1));
        Span::new(source, line, column, pointer)
    }
}

struct Scanner<'a> {
    bytes: &'a [u8],
    offset: usize,
    line: usize,
    column: usize,
    at: BTreeMap<String, (usize, usize)>,
}

impl Scanner<'_> {
    fn value(&mut self, pointer: String) {
        self.skip_whitespace();
        self.at.insert(pointer.clone(), (self.line, self.column));
        match self.peek() {
            Some(b'{') => self.object(&pointer),
            Some(b'[') => self.array(&pointer),
            Some(b'"') => {
                let _ = self.string();
            }
            Some(_) => self.literal(),
            None => {}
        }
    }

    fn object(&mut self, pointer: &str) {
        self.bump();
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some(b'}') | None => {
                    self.bump();
                    return;
                }
                Some(b',') => {
                    self.bump();
                    continue;
                }
                Some(b'"') => {}
                Some(_) => {
                    self.bump();
                    continue;
                }
            }
            let Some(key) = self.string() else { return };
            self.skip_whitespace();
            if self.peek() == Some(b':') {
                self.bump();
            }
            self.value(format!("{pointer}/{}", escape(&key)));
        }
    }

    fn array(&mut self, pointer: &str) {
        self.bump();
        let mut index = 0usize;
        loop {
            self.skip_whitespace();
            match self.peek() {
                Some(b']') | None => {
                    self.bump();
                    return;
                }
                Some(b',') => {
                    self.bump();
                    continue;
                }
                _ => {}
            }
            self.value(format!("{pointer}/{index}"));
            index += 1;
        }
    }

    /// Read a string, returning its decoded-enough form for a pointer segment.
    fn string(&mut self) -> Option<String> {
        if self.peek() != Some(b'"') {
            return None;
        }
        self.bump();
        let mut out = Vec::new();
        loop {
            match self.peek()? {
                b'"' => {
                    self.bump();
                    return String::from_utf8(out).ok();
                }
                b'\\' => {
                    self.bump();
                    // Escapes are consumed rather than decoded: a pointer
                    // segment only has to match what the lookup builds, and
                    // both sides come from the same key text.
                    if let Some(escaped) = self.peek() {
                        out.push(escaped);
                        self.bump();
                    }
                }
                byte => {
                    out.push(byte);
                    self.bump();
                }
            }
        }
    }

    fn literal(&mut self) {
        while let Some(byte) = self.peek() {
            if matches!(byte, b',' | b'}' | b']') || byte.is_ascii_whitespace() {
                return;
            }
            self.bump();
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() {
                self.bump();
            } else {
                return;
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.offset).copied()
    }

    fn bump(&mut self) {
        let Some(byte) = self.peek() else { return };
        self.offset += 1;
        if byte == b'\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
    }
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACT: &str = r#"{
  "consumer": { "name": "web-checkout" },
  "provider": { "name": "payments" },
  "interactions": [
    {
      "description": "a request for payment 42",
      "request": {
        "method": "GET",
        "path": "/payments/42",
        "query": { "expand": ["customer"] },
        "headers": { "X-Tenant": "acme" }
      },
      "response": {
        "status": 200,
        "headers": { "Content-Type": "application/json" },
        "body": { "id": "42", "amount": { "value": 100, "currency": "GBP" } }
      }
    }
  ],
  "metadata": { "pactSpecification": { "version": "3.0.0" } }
}"#;

    #[test]
    fn reads_the_consumer_provider_and_route() {
        let demand = ingest("pacts/web-checkout.json", PACT.as_bytes()).expect("a pact");
        assert_eq!(demand.consumer, "web-checkout");
        assert_eq!(demand.provider, "payments");
        assert!(
            demand
                .usages
                .iter()
                .any(|usage| { usage.route.method == "GET" && usage.route.path == "/payments/42" })
        );
    }

    #[test]
    fn reads_query_headers_and_the_response_body() {
        let demand = ingest("pacts/web-checkout.json", PACT.as_bytes()).expect("a pact");

        assert!(demand.usages.iter().any(|usage| matches!(
            &usage.kind,
            UsageKind::Parameter { name, location, value }
                if name == "expand" && location == "query" && value.as_deref() == Some("customer")
        )));
        assert!(demand.usages.iter().any(|usage| matches!(
            &usage.kind,
            UsageKind::Parameter { name, location, .. }
                if name == "X-Tenant" && location == "header"
        )));

        let response = demand
            .usages
            .iter()
            .find_map(|usage| match &usage.kind {
                UsageKind::Response { status, ty, .. } if status == "200" => Some(ty),
                _ => None,
            })
            .expect("a 200 response");
        let TypeRef::Object { fields, .. } = response else {
            panic!("expected an object, got {response:?}");
        };
        assert!(fields.contains_key("id"));
        assert!(fields["id"].required, "a recorded field is a declaration");
    }

    #[test]
    fn a_span_points_at_the_interaction_not_the_file() {
        let demand = ingest("pacts/web-checkout.json", PACT.as_bytes()).expect("a pact");
        let span = &demand
            .usages
            .iter()
            .find(|usage| matches!(usage.kind, UsageKind::Response { .. }))
            .expect("a response usage")
            .span;
        assert_eq!(span.file, "pacts/web-checkout.json");
        assert!(
            span.line > 10,
            "the span must point at the interaction, not line 1: {span:?}"
        );
    }

    #[test]
    fn a_message_interaction_is_reported_rather_than_ignored() {
        let pact = r#"{
  "consumer": { "name": "reporting" },
  "provider": { "name": "payments" },
  "interactions": [
    { "type": "Asynchronous/Messages", "description": "a payment settled event" }
  ]
}"#;
        let demand = ingest("pacts/reporting.json", pact.as_bytes()).expect("a pact");
        assert!(demand.usages.is_empty());
        assert_eq!(demand.unmodelled.len(), 1);
        assert!(
            demand.unmodelled[0]
                .kind
                .describe()
                .contains("Asynchronous"),
            "{:?}",
            demand.unmodelled[0]
        );
    }

    #[test]
    fn an_unmodellable_matcher_is_named() {
        let pact = r#"{
  "consumer": { "name": "web-checkout" },
  "provider": { "name": "payments" },
  "interactions": [
    {
      "request": { "method": "GET", "path": "/payments" },
      "response": {
        "status": 200,
        "body": { "items": [] },
        "matchingRules": { "body": { "$.items": { "matchers": [ { "match": "arrayContains" } ] } } }
      }
    }
  ]
}"#;
        let demand = ingest("pacts/web-checkout.json", pact.as_bytes()).expect("a pact");
        assert!(
            demand
                .unmodelled
                .iter()
                .any(|item| item.kind.describe().contains("arrayContains")),
            "{:?}",
            demand.unmodelled
        );
    }

    #[test]
    fn broker_links_are_data_and_do_not_stop_the_ingest() {
        let pact = r#"{
  "consumer": { "name": "web-checkout" },
  "provider": { "name": "payments" },
  "_links": { "pb:publish-verification-results": { "href": "http://broker.example.com/x" } },
  "interactions": [
    {
      "request": { "method": "GET", "path": "/payments/42" },
      "response": { "status": 200, "body": { "$ref": "http://example.com/schema.json" } }
    }
  ]
}"#;
        let demand = ingest("pacts/web-checkout.json", pact.as_bytes()).expect("a pact");
        assert_eq!(demand.consumer, "web-checkout");
        assert!(!demand.usages.is_empty());
    }

    #[test]
    fn json_that_is_not_a_pact_is_refused() {
        let error = ingest("package.json", br#"{"name":"x","version":"1"}"#)
            .expect_err("arbitrary JSON must not be called a pact");
        assert!(error.contains("not a pact"), "{error}");
    }

    #[test]
    fn a_v2_query_string_becomes_parameters() {
        let pact = r#"{
  "consumer": { "name": "c" },
  "provider": { "name": "p" },
  "interactions": [
    { "request": { "method": "GET", "path": "/payments", "query": "status=paid&limit=10" },
      "response": { "status": 200 } }
  ]
}"#;
        let demand = ingest("pacts/c.json", pact.as_bytes()).expect("a pact");
        let names: Vec<_> = demand
            .usages
            .iter()
            .filter_map(|usage| match &usage.kind {
                UsageKind::Parameter { name, value, .. } => {
                    Some((name.clone(), value.clone().unwrap_or_default()))
                }
                _ => None,
            })
            .collect();
        assert!(
            names.contains(&("status".to_owned(), "paid".to_owned())),
            "{names:?}"
        );
        assert!(
            names.contains(&("limit".to_owned(), "10".to_owned())),
            "{names:?}"
        );
    }

    #[test]
    fn positions_index_nested_pointers() {
        let positions = Positions::index("{\n  \"a\": {\n    \"b\": [1, 2]\n  }\n}");
        assert_eq!(positions.span("f.json", "/a").line, 2);
        assert_eq!(positions.span("f.json", "/a/b").line, 3);
        assert_eq!(positions.span("f.json", "/a/b/1").line, 3);
        // An unknown pointer falls back rather than losing the finding.
        assert_eq!(positions.span("f.json", "/nope").line, 1);
    }
}

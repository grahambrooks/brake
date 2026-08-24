//! `*.brake-uses.toml` → [`Demand`].
//!
//! The fallback: for gRPC, for consumers without pact tests, and for third
//! parties who will only tell you in prose. Fidelity is whatever the author
//! wrote, and it says so — a manifest lists field *paths*, not types, so it
//! declares presence and nothing more.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use super::{Demand, Route, Usage, UsageKind, any_scalar};
use crate::contract::{Field, MEDIA_ANY, Span, TypeRef};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    consumer: String,
    provider: String,
    #[serde(default)]
    uses: Vec<RawUse>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUse {
    /// Already templated — `GET /payments/{id}` — so no binding is needed.
    endpoint: String,
    #[serde(default)]
    statuses: Vec<String>,
    #[serde(default)]
    reads: Vec<String>,
    #[serde(default)]
    sends: Vec<String>,
}

/// Ingest one native manifest.
///
/// # Errors
///
/// Returns a message when the bytes are not UTF-8, are not TOML, or are TOML
/// that does not declare `consumer`, `provider` and `[[uses]]` — which is what
/// stops an arbitrary `.toml` in the tree being called a consumer declaration.
pub fn ingest(source: &str, bytes: &[u8]) -> Result<Demand, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| format!("`{source}` is not valid UTF-8: {error}"))?;
    let manifest: RawManifest = toml::from_str(text)
        .map_err(|error| format!("`{source}` is not a brake consumer manifest: {error}"))?;

    if manifest.consumer.trim().is_empty() || manifest.provider.trim().is_empty() {
        return Err(format!(
            "`{source}` must name both a `consumer` and a `provider`"
        ));
    }

    let lines = use_lines(text);
    let mut usages = BTreeSet::new();

    for (index, entry) in manifest.uses.iter().enumerate() {
        let line = lines.get(index).copied().unwrap_or(1);
        let pointer = format!("/uses/{index}");
        let span = Span::new(source, line, 1, &pointer);

        let Some((method, path)) = entry.endpoint.split_once(char::is_whitespace) else {
            return Err(format!(
                "`{source}`: `{}` is not an endpoint — write it as `GET /payments/{{id}}`",
                entry.endpoint
            ));
        };
        let route = Route::new(method, path.trim());

        usages.insert(Usage {
            route: route.clone(),
            kind: UsageKind::Endpoint,
            span: span.clone(),
        });

        // No statuses declared still means the consumer reads the success
        // response; assuming otherwise would silently verify nothing.
        let statuses: Vec<String> = if entry.statuses.is_empty() {
            vec!["200".to_owned()]
        } else {
            entry.statuses.clone()
        };
        for status in statuses {
            usages.insert(Usage {
                route: route.clone(),
                kind: UsageKind::Response {
                    status,
                    media_type: MEDIA_ANY.to_owned(),
                    ty: object_from_paths(&entry.reads),
                },
                span: span.clone(),
            });
        }

        if !entry.sends.is_empty() {
            usages.insert(Usage {
                route,
                kind: UsageKind::Request {
                    media_type: MEDIA_ANY.to_owned(),
                    ty: object_from_paths(&entry.sends),
                },
                span,
            });
        }
    }

    Ok(Demand {
        consumer: manifest.consumer,
        provider: manifest.provider,
        source: source.to_owned(),
        usages,
        unmodelled: Vec::new(),
    })
}

/// The line each `[[uses]]` header sits on.
///
/// Read from the text rather than from the parser: TOML's array-of-tables
/// header is a literal, the entries are in document order, and this is the
/// whole of what a manifest needs a position for.
fn use_lines(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("[[uses]]"))
        .map(|(index, _)| index + 1)
        .collect()
}

/// `["id", "amount.currency"]` → an object with `id` and a nested `amount`.
///
/// Leaves are [`any_scalar`]: a manifest declares that a field is read, not
/// what shape it has, and inventing a shape would report a type change the
/// author never claimed.
fn object_from_paths(paths: &[String]) -> TypeRef {
    let mut fields: BTreeMap<String, TypeRef> = BTreeMap::new();
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for path in paths {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        match path.split_once('.') {
            Some((head, rest)) => children
                .entry(head.to_owned())
                .or_default()
                .push(rest.to_owned()),
            None => {
                fields.entry(path.to_owned()).or_insert_with(any_scalar);
            }
        }
    }
    for (name, rest) in children {
        fields.insert(name, object_from_paths(&rest));
    }

    TypeRef::Object {
        fields: fields
            .into_iter()
            .map(|(name, ty)| (name, Field::new(ty, true)))
            .collect(),
        // A manifest is a list of what is used, never a claim that nothing
        // else may appear.
        additional: true,
        nullable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
consumer = "reporting"
provider = "payments"

[[uses]]
endpoint = "GET /payments/{id}"
statuses = ["200", "404"]
reads = ["id", "amount.currency", "status"]
sends = []

[[uses]]
endpoint = "POST /payments"
statuses = ["201"]
sends = ["amount.value", "amount.currency", "idempotency_key"]
"#;

    #[test]
    fn reads_both_entries_with_their_statuses() {
        let demand =
            ingest("consumers/reporting.brake-uses.toml", MANIFEST.as_bytes()).expect("a manifest");
        assert_eq!(demand.consumer, "reporting");
        assert_eq!(demand.provider, "payments");

        let statuses: BTreeSet<_> = demand
            .usages
            .iter()
            .filter_map(|usage| match &usage.kind {
                UsageKind::Response { status, .. } => Some(status.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            statuses,
            BTreeSet::from(["200".to_owned(), "201".to_owned(), "404".to_owned()])
        );
    }

    #[test]
    fn a_dotted_path_becomes_a_nested_object() {
        let ty = object_from_paths(&["id".to_owned(), "amount.currency".to_owned()]);
        let TypeRef::Object { fields, .. } = &ty else {
            panic!("expected an object");
        };
        assert!(fields.contains_key("id"));
        let TypeRef::Object { fields: nested, .. } = &fields["amount"].ty else {
            panic!("expected `amount` to be an object");
        };
        assert!(nested.contains_key("currency"));
    }

    #[test]
    fn spans_point_at_the_uses_entry() {
        let demand =
            ingest("consumers/reporting.brake-uses.toml", MANIFEST.as_bytes()).expect("a manifest");
        let second = demand
            .usages
            .iter()
            .find(|usage| usage.route.path == "/payments" && usage.route.method == "POST")
            .expect("the POST entry");
        assert!(second.span.line > 10, "{:?}", second.span);
    }

    #[test]
    fn toml_that_is_not_a_manifest_is_refused() {
        let error = ingest("Cargo.toml", b"[package]\nname = \"brake\"\n")
            .expect_err("arbitrary TOML must not be called a manifest");
        assert!(error.contains("not a brake consumer manifest"), "{error}");
    }

    #[test]
    fn an_endpoint_without_a_method_is_refused_rather_than_guessed() {
        let error = ingest(
            "consumers/x.brake-uses.toml",
            b"consumer = \"c\"\nprovider = \"p\"\n[[uses]]\nendpoint = \"/payments\"\n",
        )
        .expect_err("a bare path is not an endpoint");
        assert!(error.contains("GET /payments/{id}"), "{error}");
    }
}

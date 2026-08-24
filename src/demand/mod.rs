//! Consumer demand — the third input.
//!
//! A *demand* is a partial, one-sided description of an API surface: the
//! endpoints, fields, statuses, parameters and media types one named consumer
//! relies on. Three sources, one model, one join, one attribution pass, as
//! `design/05-consumer-demand.md` §2 specifies.
//!
//! Ingest is bytes-only, exactly as the contract ingesters are: a pact's paths
//! are concrete and a contract's are templates, so resolving `/payments/42` to
//! `GET /payments/{id}` needs the contract and is therefore a separate phase —
//! [`bind`], §3.
//!
//! Nothing here opens a socket. A URL inside a pact — `_links`, `pb:publish`,
//! a `$ref` in an example body — is data, and a demand source that is itself a
//! URL is refused at parse time. That is guarantee G1 over the demand axis.

pub mod digest;
pub mod inventory;
pub mod load;
pub mod manifest;
pub mod operations;
pub mod pact;
pub mod policy;
pub mod verify;

use std::collections::{BTreeMap, BTreeSet};

use crate::config::DemandFormat;
use crate::contract::{Contract, EndpointKey, Span, TypeRef, Unmodelled};

/// A route as the *artifact* writes it: concrete for a pact (`/payments/42`),
/// already templated for a manifest (`/payments/{id}`).
///
/// Not an [`EndpointKey`], deliberately. The two are the same shape and mean
/// different things, and collapsing them is what would let an unbound concrete
/// path be compared against a contract as though it were a template.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Route {
    pub method: String,
    pub path: String,
}

impl Route {
    #[must_use]
    pub fn new(method: &str, path: &str) -> Self {
        Self {
            // Uppercased here so `get` from a pact and `GET` from a manifest
            // are the same route rather than two.
            method: method.trim().to_ascii_uppercase(),
            path: normalise_path(path),
        }
    }
}

impl std::fmt::Display for Route {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} {}", self.method, self.path)
    }
}

/// Strip a query string and any trailing slash, and guarantee a leading one.
///
/// A pact records the query separately from the path; one that inlines it
/// anyway must not produce a route nothing can bind.
fn normalise_path(path: &str) -> String {
    let path = path.trim();
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_owned();
    }
    if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

/// What one named consumer relies on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Demand {
    /// `web-checkout`.
    pub consumer: String,
    /// The provider, as the artifact declares it.
    pub provider: String,
    /// Repository-relative, for spans.
    pub source: String,
    pub usages: BTreeSet<Usage>,
    /// Interactions the ingester met and could not model. Never empty
    /// silently — §7 of the contract spec, restated in §6.2 here.
    pub unmodelled: Vec<Unmodelled>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Usage {
    pub route: Route,
    pub kind: UsageKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum UsageKind {
    /// The consumer calls it at all.
    Endpoint,
    /// The consumer sends this request shape.
    Request { media_type: String, ty: TypeRef },
    /// The consumer reads this response.
    Response {
        status: String,
        media_type: String,
        ty: TypeRef,
    },
    Parameter {
        name: String,
        location: String,
        value: Option<String>,
    },
}

/// Ingest one consumer declaration from bytes.
///
/// # Errors
///
/// Returns the ingester's message when the document cannot be parsed, declares
/// a shape brake does not model at the document level, or names a source that
/// would require the network.
pub fn ingest(format: DemandFormat, source: &str, bytes: &[u8]) -> Result<Demand, String> {
    match format {
        DemandFormat::Pact => pact::ingest(source, bytes),
        DemandFormat::GraphqlOperations => operations::ingest(source, bytes),
        DemandFormat::Manifest => manifest::ingest(source, bytes),
    }
}

/// Which demand ingester, if any, can read this file.
///
/// Answered by *parsing*, sharing the posture of `init::identify`. The first
/// version of contract detection asked whether the path contained `api`, which
/// called `.github/workflows/api-tests.yaml` an API; a heuristic that called a
/// fixture a pact would be the same mistake with a new file extension.
#[must_use]
pub fn identify(path: &std::path::Path) -> Option<DemandFormat> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 8 * 1024 * 1024 {
        return None;
    }
    let source = crate::check::display_path(path);
    let name = source.to_ascii_lowercase();

    let candidates: &[DemandFormat] = if name.ends_with(".toml") {
        &[DemandFormat::Manifest]
    } else if name.ends_with(".json") {
        &[DemandFormat::Pact]
    } else if name.ends_with(".graphql") || name.ends_with(".gql") {
        &[DemandFormat::GraphqlOperations]
    } else {
        &[]
    };

    candidates
        .iter()
        .find(|format| ingest(**format, &source, &bytes).is_ok())
        .copied()
}

// ── the join ────────────────────────────────────────────────────────────────

/// A demand bound to a contract.
///
/// `expectation` is a [`Contract`]: the same struct, populated only where the
/// consumer declared something. That is what makes verification a projection
/// of the existing comparator rather than a second one — §4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bound {
    pub expectation: Contract,
    pub issues: Vec<BindIssue>,
    /// What the consumer declared, per endpoint, for attribution — §7.
    pub usage_index: BTreeMap<EndpointKey, Usages>,
}

/// The subjects one consumer named on one endpoint, and where it said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usages {
    /// The interaction that first named this endpoint.
    pub span: Span,
    /// Field names, statuses, media types and parameter names — the same
    /// vocabulary `Finding::subject` uses, which is what makes the join one
    /// lookup rather than a parser.
    pub subjects: BTreeSet<String>,
    /// Statuses read, for `brake consumers`.
    pub statuses: BTreeSet<String>,
    /// Response field paths read, for `brake consumers`.
    pub reads: BTreeSet<String>,
    /// Request field paths sent, for `brake consumers`.
    pub sends: BTreeSet<String>,
}

impl Usages {
    /// An empty usage set at a location.
    #[must_use]
    pub fn empty(span: Span) -> Self {
        Self::new(span)
    }

    fn new(span: Span) -> Self {
        Self {
            span,
            subjects: BTreeSet::new(),
            statuses: BTreeSet::new(),
            reads: BTreeSet::new(),
            sends: BTreeSet::new(),
        }
    }
}

/// Something the join could not do, ready to become a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindIssue {
    pub rule: &'static str,
    pub message: String,
    pub endpoint: Option<EndpointKey>,
    pub subject: Option<String>,
    /// In the *demand* artifact: the interaction that says so.
    pub span: Span,
}

/// How a concrete path lined up with the contract's templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathBinding {
    Bound {
        template: String,
        parameters: BTreeMap<String, String>,
    },
    /// Several templates match and none is more literal than the rest.
    ///
    /// Never a guess: a guessed binding attributes a break to the wrong
    /// endpoint, which is worse than declining to attribute it.
    Ambiguous(Vec<String>),
    Unmatched,
}

/// Bind one concrete path to the contract's path templates — §3.
#[must_use]
pub fn bind_path<'a>(concrete: &str, templates: impl IntoIterator<Item = &'a str>) -> PathBinding {
    let concrete = normalise_path(concrete);
    let wanted: Vec<&str> = segments(&concrete);

    let mut matches: Vec<(usize, String, BTreeMap<String, String>)> = Vec::new();
    for template in templates {
        let parts = segments(template);
        if parts.len() != wanted.len() {
            continue;
        }
        let mut parameters = BTreeMap::new();
        let mut ok = true;
        let mut templated = 0usize;
        for (part, actual) in parts.iter().zip(&wanted) {
            match part
                .strip_prefix('{')
                .and_then(|rest| rest.strip_suffix('}'))
            {
                Some(name) => {
                    templated += 1;
                    if actual.is_empty() {
                        ok = false;
                        break;
                    }
                    parameters.insert(name.to_owned(), (*actual).to_owned());
                }
                None => {
                    if part != actual {
                        ok = false;
                        break;
                    }
                }
            }
        }
        if ok {
            matches.push((templated, template.to_owned(), parameters));
        }
    }

    if matches.is_empty() {
        return PathBinding::Unmatched;
    }
    matches.sort();
    let fewest = matches[0].0;
    let contenders: Vec<_> = matches
        .iter()
        .filter(|(templated, _, _)| *templated == fewest)
        .collect();
    if contenders.len() > 1 {
        return PathBinding::Ambiguous(
            contenders
                .iter()
                .map(|(_, template, _)| template.clone())
                .collect(),
        );
    }
    let (_, template, parameters) = matches.into_iter().next().expect("non-empty");
    PathBinding::Bound {
        template,
        parameters,
    }
}

fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|part| !part.is_empty()).collect()
}

/// The scalar type name a demand uses when it declares *presence* and not
/// shape.
///
/// A GraphQL selection set names a field; a manifest lists a path. Neither
/// says whether the field is an `Int` or a `String`, and pretending otherwise
/// would report a type change the consumer never claimed. Reconciliation
/// treats it as "whatever the contract says", so only absence is reported.
///
/// A pact is different: a recorded example *does* carry a type, and it keeps
/// it.
pub const ANY_SCALAR: &str = "*";

/// A type that asserts the field exists and nothing else.
#[must_use]
pub fn any_scalar() -> TypeRef {
    TypeRef::Scalar {
        ty: ANY_SCALAR.to_owned(),
        format: None,
        nullable: false,
        constraints: crate::contract::Constraints::default(),
    }
}

/// Normalise a media type for comparison: lowercase, parameters stripped.
///
/// `application/json; charset=utf-8` from a pact and `application/json` from an
/// OpenAPI document describe the same body, and a consumer reporting them as
/// two would be reporting a difference that does not exist.
#[must_use]
pub fn normalise_media(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or(media_type)
        .trim()
        .to_ascii_lowercase()
}

pub use verify::bind;

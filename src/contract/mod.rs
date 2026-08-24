//! The normalised contract model.
//!
//! Every ingester produces one of these and `compare/` never learns which
//! format it came from. See `design/02-contract-gates.md` §3.

use std::collections::{BTreeMap, BTreeSet};

pub mod graphql;
pub mod openapi;
pub mod proto;

/// The media type used by formats that do not have media types of their own.
///
/// Both sides of a comparison use it, so `request-media-type-removed` and
/// `response-media-type-removed` cannot misfire on protobuf or GraphQL.
pub const MEDIA_ANY: &str = "*/*";

/// A unary gRPC method body.
pub const MEDIA_GRPC: &str = "application/grpc";

/// A streaming gRPC method body.
///
/// Modelling streaming-ness as a media type rather than a flag on `Endpoint`
/// keeps `compare/` format-agnostic and makes a unary/streaming swap surface
/// as the media-type removal that it is for a client.
pub const MEDIA_GRPC_STREAM: &str = "application/grpc+stream";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EndpointKey {
    pub method: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    pub endpoints: BTreeMap<EndpointKey, Endpoint>,
    pub security_schemes: BTreeMap<String, SecurityScheme>,
    /// Constructs the ingester met and could not model.
    ///
    /// Never empty silently: an `Unknown` reachable from a compared endpoint
    /// becomes `contract-partial`, so the verdict says "not fully verified"
    /// rather than "clean".
    pub unmodelled: Vec<Unmodelled>,
}

impl Contract {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            endpoints: BTreeMap::new(),
            security_schemes: BTreeMap::new(),
            unmodelled: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub operation_id: Option<String>,
    pub deprecated: bool,
    /// `x-sunset`, when the endpoint is deprecated and declares one.
    pub sunset: Option<String>,
    pub parameters: Vec<Parameter>,
    pub request: Option<Payload>,
    pub responses: BTreeMap<String, Payload>,
    pub security: Vec<SecurityRequirement>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub location: String,
    pub required: bool,
    pub deprecated: bool,
    pub ty: TypeRef,
    pub span: Span,
}

/// A request or response body, keyed by media type.
///
/// Modelling every media type rather than the first one the parser happens to
/// yield is what makes the verdict independent of document order — guarantee
/// G3 in `design/02-contract-gates.md` §6.1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub media_types: BTreeMap<String, TypeRef>,
    pub span: Span,
}

impl Payload {
    /// A payload for a format with no media-type concept of its own.
    #[must_use]
    pub fn single(ty: TypeRef, span: Span) -> Self {
        Self {
            media_types: BTreeMap::from([(MEDIA_ANY.to_owned(), ty)]),
            span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SecurityRequirement {
    pub name: String,
    pub scopes: BTreeSet<String>,
}

/// A `components.securitySchemes` entry, reduced to the parts that can break a
/// consumer: swapping `http`/`bearer` for `oauth2` invalidates every client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityScheme {
    pub ty: String,
    pub scheme: Option<String>,
    pub flows: BTreeSet<String>,
    pub location: Option<String>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmodelled {
    pub kind: UnmodelledKind,
    pub pointer: String,
    pub span: Span,
}

/// Why the ingester could not model a construct.
///
/// The variant carries its detail so `contract-partial` can name the construct
/// rather than reporting an anonymous gap — a finding a developer cannot act on
/// is barely better than no finding.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnmodelledKind {
    /// A schema shape the ingester does not model, named.
    Unsupported(String),
    /// A `$ref` into another local file. Ingest is bytes-only by design
    /// (`design/03-implementation-plan.md` §3), so it is reported, not read.
    ExternalRef(String),
    /// A `$ref` that does not resolve within this document.
    UnresolvableRef(String),
    /// A response or body with no schema attached.
    SchemaDeferred,
    /// A construct present but malformed.
    InvalidShape,
}

impl UnmodelledKind {
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Unsupported(detail) => format!("unsupported construct `{detail}`"),
            Self::ExternalRef(reference) => {
                format!("`$ref` into another file (`{reference}`), which ingest does not read")
            }
            Self::UnresolvableRef(reference) => {
                format!("`$ref` `{reference}` does not resolve in this document")
            }
            Self::SchemaDeferred => "no schema declared".to_owned(),
            Self::InvalidShape => "malformed schema".to_owned(),
        }
    }
}

/// A location in a contract artifact.
///
/// `file` is repository-relative with `/` separators, never absolute — an
/// absolute path in output would break guarantees G4 and G6.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub pointer: String,
}

impl Span {
    #[must_use]
    pub fn new(file: &str, line: usize, column: usize, pointer: impl Into<String>) -> Self {
        Self {
            file: file.to_owned(),
            line,
            column,
            pointer: pointer.into(),
        }
    }
}

/// A resolved, normalised type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypeRef {
    Scalar {
        ty: String,
        format: Option<String>,
        nullable: bool,
        /// Value-range and length bounds. Tightening any of them narrows the
        /// accepted input, which spec §5.2 calls `param-type-narrowed`.
        constraints: Constraints,
    },
    Enum {
        values: BTreeSet<String>,
        /// Wire numbers per value, where the format has them.
        ///
        /// Empty for OpenAPI and GraphQL, whose enum members are their own
        /// identity. For protobuf the number is the identity, exactly as it is
        /// for a message field.
        numbers: BTreeMap<String, i32>,
    },
    Array {
        items: Box<TypeRef>,
        nullable: bool,
    },
    Object {
        fields: BTreeMap<String, Field>,
        additional: bool,
        nullable: bool,
    },
    OneOf {
        variants: Vec<TypeRef>,
    },
    Cycle(String),
    Unknown(UnmodelledKind),
}

/// Scalar bounds, as strings so a format's own spelling survives round-trip.
///
/// Numeric bounds are compared numerically where both sides parse, and by
/// equality otherwise, so an unparseable bound is never silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Constraints {
    pub minimum: Option<String>,
    pub maximum: Option<String>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    pub pattern: Option<String>,
}

impl Constraints {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Field {
    pub ty: TypeRef,
    pub required: bool,
    pub deprecated: bool,
    /// The wire identity of the field, where the format has one.
    ///
    /// Protobuf compatibility is defined by field number, not field name: a
    /// field renamed with a stable number is wire-compatible and a field
    /// renumbered with a stable name is a hard break. Formats without wire
    /// numbers leave this `None` and are compared by name.
    pub number: Option<i32>,
    /// Where the field is declared.
    ///
    /// Without it a finding about `customer_id` underlines the enclosing
    /// response, which is the right file and the wrong line — and the line is
    /// what a reader looks at first. `None` where an ingester cannot supply
    /// one, in which case the payload's span is used as before.
    pub span: Option<Span>,
}

impl Field {
    #[must_use]
    pub fn new(ty: TypeRef, required: bool) -> Self {
        Self {
            ty,
            required,
            deprecated: false,
            number: None,
            span: None,
        }
    }

    /// The same, declared at a known location.
    #[must_use]
    pub fn at(ty: TypeRef, required: bool, span: Span) -> Self {
        Self {
            span: Some(span),
            ..Self::new(ty, required)
        }
    }
}

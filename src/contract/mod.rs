use std::collections::BTreeMap;

pub mod graphql;
pub mod openapi;
pub mod proto;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct EndpointKey {
    pub method: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    pub endpoints: BTreeMap<EndpointKey, Endpoint>,
    pub unmodelled: Vec<Unmodelled>,
}

impl Contract {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            endpoints: BTreeMap::new(),
            unmodelled: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub operation_id: Option<String>,
    pub deprecated: bool,
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
    pub ty: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub ty: TypeRef,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityRequirement {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmodelled {
    pub kind: UnmodelledKind,
    pub pointer: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnmodelledKind {
    Parse,
    InvalidShape,
    SchemaDeferred,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub pointer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    Scalar {
        ty: String,
        format: Option<String>,
        nullable: bool,
    },
    Enum {
        values: std::collections::BTreeSet<String>,
    },
    Array {
        items: Box<TypeRef>,
    },
    Object {
        fields: BTreeMap<String, Field>,
        additional: bool,
    },
    OneOf {
        variants: Vec<TypeRef>,
    },
    Cycle(String),
    Unknown(UnmodelledKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub ty: TypeRef,
    pub required: bool,
}

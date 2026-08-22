use thiserror::Error;

use super::{Contract, Unmodelled, UnmodelledKind};

#[derive(Debug, Error)]
pub enum GraphqlError {
    #[error("contract source `{contract_source}` is not valid UTF-8: {error}")]
    InvalidUtf8 {
        contract_source: String,
        error: std::str::Utf8Error,
    },
}

pub fn ingest(source: &str, bytes: &[u8]) -> Result<Contract, GraphqlError> {
    let _input = std::str::from_utf8(bytes).map_err(|error| GraphqlError::InvalidUtf8 {
        contract_source: source.to_owned(),
        error,
    })?;

    let mut contract = Contract::empty();
    contract.unmodelled.push(Unmodelled {
        kind: UnmodelledKind::SchemaDeferred,
        pointer: "/".to_owned(),
        span: super::Span {
            file: source.to_owned(),
            line: 1,
            column: 1,
            pointer: "/".to_owned(),
        },
    });
    Ok(contract)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphql_ingest_surfaces_unmodelled_placeholder() {
        let contract =
            ingest("api/schema.graphql", b"type Query { health: String! }").expect("ingest");
        assert_eq!(contract.endpoints.len(), 0);
        assert_eq!(contract.unmodelled.len(), 1);
        assert!(matches!(
            contract.unmodelled[0].kind,
            UnmodelledKind::SchemaDeferred
        ));
    }
}

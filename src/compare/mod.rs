use std::collections::{BTreeMap, BTreeSet};

use crate::contract::{Contract, EndpointKey, Span};

pub mod types;
use types::{TypeIssue, compare_request_type, compare_response_type};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    EndpointRemoved {
        method: String,
        path: String,
        span: Span,
    },
    MethodRemoved {
        method: String,
        path: String,
        span: Span,
    },
    EndpointPathChanged {
        operation_id: String,
        method: String,
        from_path: String,
        to_path: String,
        span: Span,
    },
    ParamAddedRequired {
        method: String,
        path: String,
        parameter: String,
        span: Span,
    },
    ParamBecameRequired {
        method: String,
        path: String,
        parameter: String,
        span: Span,
    },
    ParamRemoved {
        method: String,
        path: String,
        parameter: String,
        span: Span,
    },
    ParamTypeNarrowed {
        method: String,
        path: String,
        target: String,
        reason: String,
        span: Span,
    },
    ResponseTypeChanged {
        method: String,
        path: String,
        status: String,
        reason: String,
        span: Span,
    },
    ResponseEnumExtended {
        method: String,
        path: String,
        status: String,
        span: Span,
    },
    ResponseStatusRemoved {
        method: String,
        path: String,
        status: String,
        span: Span,
    },
}

pub fn compare_contracts(base: &Contract, head: &Contract) -> Vec<Change> {
    let mut changes = compare_endpoint_sets(base, head);
    changes.extend(compare_endpoint_details(base, head));
    changes.sort();
    changes
}

pub fn compare_endpoint_sets(base: &Contract, head: &Contract) -> Vec<Change> {
    let base_operation_ids = operation_id_index(base);
    let head_operation_ids = operation_id_index(head);

    let mut moved_keys = BTreeSet::new();
    let mut changes = Vec::new();

    for (operation_id, base_key) in &base_operation_ids {
        let Some(head_key) = head_operation_ids.get(operation_id) else {
            continue;
        };

        if base_key.path != head_key.path {
            moved_keys.insert(base_key.clone());
            let span = head
                .endpoints
                .get(head_key)
                .expect("head operation id index points to an existing endpoint")
                .span
                .clone();
            changes.push(Change::EndpointPathChanged {
                operation_id: operation_id.clone(),
                method: head_key.method.clone(),
                from_path: base_key.path.clone(),
                to_path: head_key.path.clone(),
                span,
            });
        }
    }

    for base_key in base.endpoints.keys() {
        if moved_keys.contains(base_key) || head.endpoints.contains_key(base_key) {
            continue;
        }

        let span = base
            .endpoints
            .get(base_key)
            .expect("base key must exist in base endpoints")
            .span
            .clone();

        let has_path_in_head = head.endpoints.keys().any(|key| key.path == base_key.path);
        if has_path_in_head {
            changes.push(Change::MethodRemoved {
                method: base_key.method.clone(),
                path: base_key.path.clone(),
                span,
            });
        } else {
            changes.push(Change::EndpointRemoved {
                method: base_key.method.clone(),
                path: base_key.path.clone(),
                span,
            });
        }
    }

    changes.sort();
    changes
}

fn compare_endpoint_details(base: &Contract, head: &Contract) -> Vec<Change> {
    let mut changes = Vec::new();
    for (key, base_endpoint) in &base.endpoints {
        let Some(head_endpoint) = head.endpoints.get(key) else {
            continue;
        };

        let method = key.method.clone();
        let path = key.path.clone();

        let mut base_params = BTreeMap::new();
        for parameter in &base_endpoint.parameters {
            base_params.insert(
                format!("{}:{}", parameter.location, parameter.name),
                parameter,
            );
        }
        let mut head_params = BTreeMap::new();
        for parameter in &head_endpoint.parameters {
            head_params.insert(
                format!("{}:{}", parameter.location, parameter.name),
                parameter,
            );
        }

        for (parameter_key, base_param) in &base_params {
            if let Some(head_param) = head_params.get(parameter_key) {
                if !base_param.required && head_param.required {
                    changes.push(Change::ParamBecameRequired {
                        method: method.clone(),
                        path: path.clone(),
                        parameter: parameter_key.clone(),
                        span: head_endpoint.span.clone(),
                    });
                }
                for issue in compare_request_type(&base_param.ty, &head_param.ty) {
                    if let TypeIssue::RequestTypeNarrowed { reason } = issue {
                        changes.push(Change::ParamTypeNarrowed {
                            method: method.clone(),
                            path: path.clone(),
                            target: format!("parameter `{parameter_key}`"),
                            reason,
                            span: head_endpoint.span.clone(),
                        });
                    }
                }
            } else {
                changes.push(Change::ParamRemoved {
                    method: method.clone(),
                    path: path.clone(),
                    parameter: parameter_key.clone(),
                    span: base_endpoint.span.clone(),
                });
            }
        }

        for (parameter_key, head_param) in &head_params {
            if !base_params.contains_key(parameter_key) && head_param.required {
                changes.push(Change::ParamAddedRequired {
                    method: method.clone(),
                    path: path.clone(),
                    parameter: parameter_key.clone(),
                    span: head_endpoint.span.clone(),
                });
            }
        }

        if let (Some(base_request), Some(head_request)) =
            (&base_endpoint.request, &head_endpoint.request)
        {
            for issue in compare_request_type(&base_request.ty, &head_request.ty) {
                if let TypeIssue::RequestTypeNarrowed { reason } = issue {
                    changes.push(Change::ParamTypeNarrowed {
                        method: method.clone(),
                        path: path.clone(),
                        target: "request body".to_owned(),
                        reason,
                        span: head_request.span.clone(),
                    });
                }
            }
        }

        for (status, base_response) in &base_endpoint.responses {
            let Some(head_response) = head_endpoint.responses.get(status) else {
                changes.push(Change::ResponseStatusRemoved {
                    method: method.clone(),
                    path: path.clone(),
                    status: status.clone(),
                    span: base_response.span.clone(),
                });
                continue;
            };

            for issue in compare_response_type(&base_response.ty, &head_response.ty) {
                match issue {
                    TypeIssue::ResponseTypeChanged { reason } => {
                        changes.push(Change::ResponseTypeChanged {
                            method: method.clone(),
                            path: path.clone(),
                            status: status.clone(),
                            reason,
                            span: head_response.span.clone(),
                        });
                    }
                    TypeIssue::ResponseEnumExtended | TypeIssue::ResponseVariantAdded => {
                        changes.push(Change::ResponseEnumExtended {
                            method: method.clone(),
                            path: path.clone(),
                            status: status.clone(),
                            span: head_response.span.clone(),
                        });
                    }
                    TypeIssue::RequestTypeNarrowed { .. } | TypeIssue::RequestVariantRemoved => {}
                }
            }
        }
    }

    changes
}

fn operation_id_index(contract: &Contract) -> BTreeMap<String, EndpointKey> {
    let mut operation_ids = BTreeMap::new();
    for (key, endpoint) in &contract.endpoints {
        if let Some(operation_id) = &endpoint.operation_id {
            operation_ids.insert(operation_id.clone(), key.clone());
        }
    }
    operation_ids
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::contract::{
        Contract, Endpoint, Parameter, Payload, TypeRef, UnmodelledKind, openapi,
    };

    fn span() -> Span {
        Span {
            file: "api/openapi.yaml".to_owned(),
            line: 1,
            column: 1,
            pointer: "/paths".to_owned(),
        }
    }

    fn endpoint(operation_id: Option<&str>) -> Endpoint {
        Endpoint {
            operation_id: operation_id.map(ToOwned::to_owned),
            deprecated: false,
            parameters: Vec::new(),
            request: Some(Payload {
                ty: TypeRef::Unknown(UnmodelledKind::SchemaDeferred),
                span: span(),
            }),
            responses: BTreeMap::new(),
            security: Vec::new(),
            span: span(),
        }
    }

    fn endpoint_with_types(
        operation_id: Option<&str>,
        parameters: Vec<Parameter>,
        request: Option<TypeRef>,
        response_200: Option<TypeRef>,
    ) -> Endpoint {
        let mut responses = BTreeMap::new();
        if let Some(ty) = response_200 {
            responses.insert("200".to_owned(), Payload { ty, span: span() });
        }
        Endpoint {
            operation_id: operation_id.map(ToOwned::to_owned),
            deprecated: false,
            parameters,
            request: request.map(|ty| Payload { ty, span: span() }),
            responses,
            security: Vec::new(),
            span: span(),
        }
    }

    fn contract(entries: Vec<(&str, &str, Endpoint)>) -> Contract {
        let mut contract = Contract::empty();
        for (method, path, endpoint) in entries {
            contract.endpoints.insert(
                EndpointKey {
                    method: method.to_owned(),
                    path: path.to_owned(),
                },
                endpoint,
            );
        }
        contract
    }

    #[test]
    fn emits_endpoint_removed_for_missing_path() {
        let base = contract(vec![(
            "GET",
            "/payments/{id}",
            endpoint(Some("getPayment")),
        )]);
        let head = Contract::empty();

        let changes = compare_endpoint_sets(&base, &head);

        assert_eq!(
            changes,
            vec![Change::EndpointRemoved {
                method: "GET".to_owned(),
                path: "/payments/{id}".to_owned(),
                span: span(),
            }]
        );
    }

    #[test]
    fn emits_method_removed_for_missing_method_on_existing_path() {
        let base = contract(vec![
            ("GET", "/payments", endpoint(Some("listPayments"))),
            ("POST", "/payments", endpoint(Some("createPayment"))),
        ]);
        let head = contract(vec![("GET", "/payments", endpoint(Some("listPayments")))]);

        let changes = compare_endpoint_sets(&base, &head);

        assert_eq!(
            changes,
            vec![Change::MethodRemoved {
                method: "POST".to_owned(),
                path: "/payments".to_owned(),
                span: span(),
            }]
        );
    }

    #[test]
    fn emits_path_changed_when_operation_id_survives() {
        let base = contract(vec![(
            "GET",
            "/payments/{id}",
            endpoint(Some("getPayment")),
        )]);
        let head = contract(vec![(
            "GET",
            "/payments/{payment_id}",
            endpoint(Some("getPayment")),
        )]);

        let changes = compare_endpoint_sets(&base, &head);

        assert_eq!(
            changes,
            vec![Change::EndpointPathChanged {
                operation_id: "getPayment".to_owned(),
                method: "GET".to_owned(),
                from_path: "/payments/{id}".to_owned(),
                to_path: "/payments/{payment_id}".to_owned(),
                span: span(),
            }]
        );
    }

    #[test]
    fn compares_request_and_response_types() {
        let base = contract(vec![(
            "POST",
            "/payments",
            endpoint_with_types(
                Some("createPayment"),
                vec![Parameter {
                    name: "mode".to_owned(),
                    location: "query".to_owned(),
                    required: false,
                    ty: TypeRef::Enum {
                        values: BTreeSet::from(["safe".to_owned(), "fast".to_owned()]),
                    },
                }],
                Some(TypeRef::Object {
                    fields: BTreeMap::new(),
                    additional: true,
                }),
                Some(TypeRef::Enum {
                    values: BTreeSet::from(["pending".to_owned()]),
                }),
            ),
        )]);
        let head = contract(vec![(
            "POST",
            "/payments",
            endpoint_with_types(
                Some("createPayment"),
                vec![Parameter {
                    name: "mode".to_owned(),
                    location: "query".to_owned(),
                    required: true,
                    ty: TypeRef::Enum {
                        values: BTreeSet::from(["safe".to_owned()]),
                    },
                }],
                Some(TypeRef::Object {
                    fields: BTreeMap::new(),
                    additional: false,
                }),
                Some(TypeRef::Enum {
                    values: BTreeSet::from(["pending".to_owned(), "paid".to_owned()]),
                }),
            ),
        )]);

        let changes = compare_contracts(&base, &head);

        assert!(changes.iter().any(|change| matches!(
            change,
            Change::ParamBecameRequired { parameter, .. } if parameter == "query:mode"
        )));
        assert!(changes.iter().any(|change| matches!(
            change,
            Change::ParamTypeNarrowed { target, .. } if target == "request body"
        )));
        assert!(changes.iter().any(|change| matches!(
            change,
            Change::ResponseEnumExtended { status, .. } if status == "200"
        )));
    }

    #[test]
    fn faithful_openapi_30_to_31_translation_has_no_changes() {
        let openapi_30 = r#"
openapi: 3.0.3
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                required: [id, note]
                properties:
                  id:
                    type: string
                  note:
                    type: string
                    nullable: true
"#;
        let openapi_31 = r#"
openapi: 3.1.0
paths:
  /payments/{id}:
    get:
      operationId: getPayment
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema:
                type: object
                required: [id, note]
                properties:
                  id:
                    type: string
                  note:
                    type: [string, "null"]
"#;

        let base =
            openapi::ingest("api/openapi-30.yaml", openapi_30.as_bytes()).expect("ingest 30");
        let head =
            openapi::ingest("api/openapi-31.yaml", openapi_31.as_bytes()).expect("ingest 31");
        let changes = compare_contracts(&base, &head);

        assert!(changes.is_empty(), "unexpected changes: {changes:?}");
    }
}

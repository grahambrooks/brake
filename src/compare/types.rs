use std::collections::BTreeSet;

use crate::contract::TypeRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeDirection {
    Request,
    Response,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeIssue {
    RequestTypeNarrowed { reason: String },
    ResponseTypeChanged { reason: String },
    ResponseEnumExtended,
    RequestVariantRemoved,
    ResponseVariantAdded,
}

pub fn compare_request_type(base: &TypeRef, head: &TypeRef) -> Vec<TypeIssue> {
    compare(base, head, TypeDirection::Request)
}

pub fn compare_response_type(base: &TypeRef, head: &TypeRef) -> Vec<TypeIssue> {
    compare(base, head, TypeDirection::Response)
}

fn compare(base: &TypeRef, head: &TypeRef, direction: TypeDirection) -> Vec<TypeIssue> {
    let mut issues = Vec::new();
    compare_inner(base, head, direction, &mut issues);
    issues
}

fn compare_inner(
    base: &TypeRef,
    head: &TypeRef,
    direction: TypeDirection,
    issues: &mut Vec<TypeIssue>,
) {
    if type_equivalent_with_nullable_normalization(base, head) {
        return;
    }

    match (base, head) {
        (TypeRef::Unknown(_), _) | (_, TypeRef::Unknown(_)) => {}
        (TypeRef::Cycle(base_name), TypeRef::Cycle(head_name)) => {
            if base_name != head_name {
                push_changed(
                    direction,
                    format!("cycle changed from `{base_name}` to `{head_name}`"),
                    issues,
                );
            }
        }
        (
            TypeRef::Scalar {
                ty: base_ty,
                format: base_format,
                nullable: base_nullable,
            },
            TypeRef::Scalar {
                ty: head_ty,
                format: head_format,
                nullable: head_nullable,
            },
        ) => {
            if base_ty != head_ty || base_format != head_format {
                push_changed(
                    direction,
                    format!(
                        "scalar changed from `{:?}` to `{:?}`",
                        scalar_display(base_ty, base_format, *base_nullable),
                        scalar_display(head_ty, head_format, *head_nullable)
                    ),
                    issues,
                );
            } else if direction == TypeDirection::Request && *base_nullable && !head_nullable {
                issues.push(TypeIssue::RequestTypeNarrowed {
                    reason: "nullable request type became non-nullable".to_owned(),
                });
            }
        }
        (
            TypeRef::Enum {
                values: base_values,
            },
            TypeRef::Enum {
                values: head_values,
            },
        ) => {
            if head_values.is_superset(base_values) && head_values.len() > base_values.len() {
                if direction == TypeDirection::Response {
                    issues.push(TypeIssue::ResponseEnumExtended);
                }
            } else if head_values.is_subset(base_values) && head_values.len() < base_values.len() {
                if direction == TypeDirection::Request {
                    issues.push(TypeIssue::RequestTypeNarrowed {
                        reason: "request enum removed accepted values".to_owned(),
                    });
                } else {
                    push_changed(
                        direction,
                        "response enum removed previously documented values".to_owned(),
                        issues,
                    );
                }
            } else if base_values != head_values {
                push_changed(direction, "enum changed incompatibly".to_owned(), issues);
            }
        }
        (TypeRef::Array { items: base_items }, TypeRef::Array { items: head_items }) => {
            compare_inner(base_items, head_items, direction, issues);
        }
        (
            TypeRef::Object {
                fields: base_fields,
                additional: base_additional,
            },
            TypeRef::Object {
                fields: head_fields,
                additional: head_additional,
            },
        ) => {
            if direction == TypeDirection::Request && *base_additional && !head_additional {
                issues.push(TypeIssue::RequestTypeNarrowed {
                    reason: "additionalProperties tightened from true to false".to_owned(),
                });
            }

            for (field_name, base_field) in base_fields {
                let Some(head_field) = head_fields.get(field_name) else {
                    push_changed(
                        direction,
                        format!("field `{field_name}` was removed"),
                        issues,
                    );
                    continue;
                };

                if direction == TypeDirection::Request
                    && !base_field.required
                    && head_field.required
                {
                    issues.push(TypeIssue::RequestTypeNarrowed {
                        reason: format!("field `{field_name}` became required"),
                    });
                }
                if direction == TypeDirection::Response
                    && base_field.required
                    && !head_field.required
                {
                    push_changed(
                        direction,
                        format!("response field `{field_name}` became optional"),
                        issues,
                    );
                }
                compare_inner(&base_field.ty, &head_field.ty, direction, issues);
            }

            if direction == TypeDirection::Request {
                for (field_name, head_field) in head_fields {
                    if !base_fields.contains_key(field_name) && head_field.required {
                        issues.push(TypeIssue::RequestTypeNarrowed {
                            reason: format!("new required field `{field_name}` added"),
                        });
                    }
                }
            }
        }
        (
            TypeRef::OneOf {
                variants: base_variants,
            },
            TypeRef::OneOf {
                variants: head_variants,
            },
        ) => {
            let base_set = normalize_variant_set(base_variants);
            let head_set = normalize_variant_set(head_variants);

            let removed = base_set.difference(&head_set).count();
            let added = head_set.difference(&base_set).count();

            if direction == TypeDirection::Request && removed > 0 {
                issues.push(TypeIssue::RequestVariantRemoved);
            }
            if direction == TypeDirection::Response && added > 0 {
                issues.push(TypeIssue::ResponseVariantAdded);
            }
            if direction == TypeDirection::Response && removed > 0 {
                push_changed(
                    direction,
                    "response oneOf removed variants".to_owned(),
                    issues,
                );
            }
            if direction == TypeDirection::Request && added > 0 {
                // Request widening is safe.
            }
        }
        _ => {
            push_changed(direction, "type changed incompatibly".to_owned(), issues);
        }
    }
}

fn push_changed(direction: TypeDirection, reason: String, issues: &mut Vec<TypeIssue>) {
    match direction {
        TypeDirection::Request => issues.push(TypeIssue::RequestTypeNarrowed { reason }),
        TypeDirection::Response => issues.push(TypeIssue::ResponseTypeChanged { reason }),
    }
}

fn type_equivalent_with_nullable_normalization(base: &TypeRef, head: &TypeRef) -> bool {
    normalized_nullable(base) == normalized_nullable(head)
}

fn normalized_nullable(ty: &TypeRef) -> TypeRef {
    match ty {
        TypeRef::OneOf { variants } => {
            if let Some(with_nullable) = fold_one_of_nullable(variants) {
                with_nullable
            } else {
                TypeRef::OneOf {
                    variants: variants.iter().map(normalized_nullable).collect(),
                }
            }
        }
        TypeRef::Array { items } => TypeRef::Array {
            items: Box::new(normalized_nullable(items)),
        },
        TypeRef::Object { fields, additional } => TypeRef::Object {
            fields: fields
                .iter()
                .map(|(name, field)| {
                    (
                        name.clone(),
                        crate::contract::Field {
                            ty: normalized_nullable(&field.ty),
                            required: field.required,
                        },
                    )
                })
                .collect(),
            additional: *additional,
        },
        _ => ty.clone(),
    }
}

fn fold_one_of_nullable(variants: &[TypeRef]) -> Option<TypeRef> {
    if variants.len() != 2 {
        return None;
    }

    let mut non_null_variant: Option<&TypeRef> = None;
    let mut saw_null = false;
    for variant in variants {
        match variant {
            TypeRef::Scalar { ty, .. } if ty == "null" => saw_null = true,
            other => non_null_variant = Some(other),
        }
    }

    let base = non_null_variant?;
    if !saw_null {
        return None;
    }

    match normalized_nullable(base) {
        TypeRef::Scalar { ty, format, .. } => Some(TypeRef::Scalar {
            ty,
            format,
            nullable: true,
        }),
        _ => None,
    }
}

fn normalize_variant_set(variants: &[TypeRef]) -> BTreeSet<String> {
    variants
        .iter()
        .map(|variant| format!("{:?}", normalized_nullable(variant)))
        .collect()
}

fn scalar_display(ty: &str, format: &Option<String>, nullable: bool) -> String {
    if let Some(format) = format {
        format!("{ty}:{format}:nullable={nullable}")
    } else {
        format!("{ty}:nullable={nullable}")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::contract::Field;

    fn scalar(ty: &str, nullable: bool) -> TypeRef {
        TypeRef::Scalar {
            ty: ty.to_owned(),
            format: None,
            nullable,
        }
    }

    #[test]
    fn cycle_with_same_name_is_compatible() {
        let issues = compare_request_type(
            &TypeRef::Cycle("Payment".to_owned()),
            &TypeRef::Cycle("Payment".to_owned()),
        );
        assert!(issues.is_empty());
    }

    #[test]
    fn cycle_name_change_is_incompatible() {
        let issues = compare_response_type(
            &TypeRef::Cycle("Payment".to_owned()),
            &TypeRef::Cycle("Invoice".to_owned()),
        );
        assert!(matches!(issues[0], TypeIssue::ResponseTypeChanged { .. }));
    }

    #[test]
    fn request_detects_oneof_variant_removal() {
        let base = TypeRef::OneOf {
            variants: vec![scalar("string", false), scalar("integer", false)],
        };
        let head = TypeRef::OneOf {
            variants: vec![scalar("string", false)],
        };

        let issues = compare_request_type(&base, &head);
        assert!(issues.contains(&TypeIssue::RequestVariantRemoved));
    }

    #[test]
    fn response_detects_oneof_variant_addition() {
        let base = TypeRef::OneOf {
            variants: vec![scalar("string", false)],
        };
        let head = TypeRef::OneOf {
            variants: vec![scalar("string", false), scalar("integer", false)],
        };

        let issues = compare_response_type(&base, &head);
        assert!(issues.contains(&TypeIssue::ResponseVariantAdded));
    }

    #[test]
    fn request_detects_additional_properties_tightening() {
        let base = TypeRef::Object {
            fields: BTreeMap::new(),
            additional: true,
        };
        let head = TypeRef::Object {
            fields: BTreeMap::new(),
            additional: false,
        };

        let issues = compare_request_type(&base, &head);
        assert!(issues.iter().any(|issue| matches!(
            issue,
            TypeIssue::RequestTypeNarrowed { reason } if reason.contains("additionalProperties")
        )));
    }

    #[test]
    fn normalizes_openapi_nullable_between_30_and_31() {
        let openapi_30 = scalar("string", true);
        let openapi_31 = TypeRef::OneOf {
            variants: vec![scalar("string", false), scalar("null", false)],
        };

        let request_issues = compare_request_type(&openapi_30, &openapi_31);
        let response_issues = compare_response_type(&openapi_30, &openapi_31);
        assert!(request_issues.is_empty());
        assert!(response_issues.is_empty());
    }

    #[test]
    fn request_enum_narrowing_is_detected() {
        let base = TypeRef::Enum {
            values: BTreeSet::from(["pending".to_owned(), "paid".to_owned()]),
        };
        let head = TypeRef::Enum {
            values: BTreeSet::from(["pending".to_owned()]),
        };

        let issues = compare_request_type(&base, &head);
        assert!(issues.iter().any(|issue| matches!(
            issue,
            TypeIssue::RequestTypeNarrowed { reason } if reason.contains("enum")
        )));
    }

    #[test]
    fn response_enum_extension_is_detected() {
        let base = TypeRef::Enum {
            values: BTreeSet::from(["pending".to_owned()]),
        };
        let head = TypeRef::Enum {
            values: BTreeSet::from(["pending".to_owned(), "paid".to_owned()]),
        };

        let issues = compare_response_type(&base, &head);
        assert!(issues.contains(&TypeIssue::ResponseEnumExtended));
    }

    #[test]
    fn request_required_field_addition_is_detected() {
        let base = TypeRef::Object {
            fields: BTreeMap::new(),
            additional: true,
        };
        let head = TypeRef::Object {
            fields: BTreeMap::from([(
                "id".to_owned(),
                Field {
                    ty: scalar("string", false),
                    required: true,
                },
            )]),
            additional: true,
        };

        let issues = compare_request_type(&base, &head);
        assert!(issues.iter().any(|issue| matches!(
            issue,
            TypeIssue::RequestTypeNarrowed { reason } if reason.contains("required field")
        )));
    }
}

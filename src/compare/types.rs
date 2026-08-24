//! `TypeRef` × `TypeRef` → the issues between them.
//!
//! Format-agnostic by construction: this module never learns whether it is
//! holding OpenAPI, protobuf or GraphQL. Every issue carries a JSON pointer
//! relative to the payload root, so a finding can name the field it is about
//! and a suppression can target that field structurally rather than by
//! matching text in a human-readable message.

use std::collections::{BTreeMap, BTreeSet};

use crate::contract::{Constraints, Field, Span, TypeRef, UnmodelledKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeDirection {
    Request,
    Response,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum TypeIssue {
    /// The accepted input shrank: existing callers stop being accepted.
    RequestTypeNarrowed {
        pointer: String,
        reason: String,
    },
    RequestFieldAddedRequired {
        pointer: String,
        field: String,
    },
    /// Additive, and therefore only a finding under `strict`.
    RequestFieldAddedOptional {
        pointer: String,
        field: String,
    },
    RequestVariantRemoved {
        pointer: String,
    },

    /// The produced output changed shape: existing readers break.
    ResponseTypeChanged {
        pointer: String,
        reason: String,
    },
    ResponseFieldRemoved {
        pointer: String,
        field: String,
    },
    ResponseFieldOptional {
        pointer: String,
        field: String,
    },
    /// Additive, and therefore only a finding under `strict`.
    ResponseFieldAdded {
        pointer: String,
        field: String,
    },
    ResponseEnumExtended {
        pointer: String,
    },
    ResponseVariantAdded {
        pointer: String,
    },

    /// A field kept its wire number and changed its name. Wire-compatible;
    /// breaks any JSON or generated-code consumer.
    FieldRenamed {
        pointer: String,
        from: String,
        to: String,
    },
    /// A field kept its name and changed its wire number. This is the
    /// protobuf break: the bytes on the wire no longer mean the same thing.
    FieldNumberChanged {
        pointer: String,
        field: String,
        from: i32,
        to: i32,
    },

    /// Something on this path could not be modelled, so the comparison is not
    /// a clean result — it is an unverified one, and says so.
    Partial {
        pointer: String,
        detail: String,
    },
}

/// An issue and the place it is about.
///
/// The span is the nearest enclosing field's, where the ingester supplied one.
/// Without it a finding about `customer_id` underlines the whole response —
/// the right file, the wrong line, and the line is what a reader checks first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub issue: TypeIssue,
    pub span: Option<Span>,
}

/// Collects issues, stamping each with the field currently being compared.
///
/// Deliberately shaped like `Vec::push` so the thirty-odd call sites below
/// read unchanged: what varies is *where* an issue is, not what it says.
#[derive(Debug, Default)]
struct Issues {
    found: Vec<Located>,
    span: Option<Span>,
}

impl Issues {
    fn push(&mut self, issue: TypeIssue) {
        self.found.push(Located {
            issue,
            span: self.span.clone(),
        });
    }

    /// Run `body` with `span` as the current location, then restore.
    fn within<R>(&mut self, span: Option<Span>, body: impl FnOnce(&mut Self) -> R) -> R {
        // A field with no span of its own keeps its parent's, which is closer
        // than the payload and never worse.
        let previous = match span {
            Some(span) => self.span.replace(span),
            None => self.span.clone(),
        };
        let result = body(self);
        self.span = previous;
        result
    }
}

pub fn compare_request_type(base: &TypeRef, head: &TypeRef) -> Vec<TypeIssue> {
    compare_kinds(base, head, TypeDirection::Request)
}

pub fn compare_response_type(base: &TypeRef, head: &TypeRef) -> Vec<TypeIssue> {
    compare_kinds(base, head, TypeDirection::Response)
}

/// Result of evaluating the subtyping relation `<:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubtypeResult {
    /// Types satisfy the subtyping relation.
    Valid,
    /// Types are incompatible; contains the detected compatibility issues.
    Incompatible(Vec<TypeIssue>),
}

impl SubtypeResult {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid)
    }

    #[must_use]
    pub fn issues(&self) -> &[TypeIssue] {
        match self {
            Self::Valid => &[],
            Self::Incompatible(issues) => issues,
        }
    }
}

/// Checks the structural subtyping relation `<:`.
///
/// In response position (covariance), head must be a subtype of base (`Head <: Base`).
/// In request position (contravariance), head must be a supertype of base (`Base <: Head`).
#[must_use]
pub fn check_subtype(
    sub: &TypeRef,
    sup: &TypeRef,
    direction: TypeDirection,
    pointer: &str,
) -> SubtypeResult {
    let mut issues = Issues::default();
    match direction {
        TypeDirection::Response => {
            // Covariance: sub = Head, sup = Base -> Head <: Base
            compare_inner(sup, sub, direction, pointer, &mut issues);
        }
        TypeDirection::Request => {
            // Contravariance: sub = Base, sup = Head -> Base <: Head
            compare_inner(sub, sup, direction, pointer, &mut issues);
        }
    }
    if issues.found.is_empty() {
        SubtypeResult::Valid
    } else {
        SubtypeResult::Incompatible(issues.found.into_iter().map(|l| l.issue).collect())
    }
}

/// Compare two types, reporting where each issue is.
#[must_use]
pub fn compare(base: &TypeRef, head: &TypeRef, direction: TypeDirection) -> Vec<Located> {
    let mut issues = Issues::default();
    compare_inner(base, head, direction, "", &mut issues);

    let mut found = issues.found;
    found.sort_by(|a, b| a.issue.cmp(&b.issue).then_with(|| a.span.cmp(&b.span)));
    found.dedup();
    found
}

/// The issues alone, for a caller that does not need locations.
#[must_use]
pub fn compare_kinds(base: &TypeRef, head: &TypeRef, direction: TypeDirection) -> Vec<TypeIssue> {
    compare(base, head, direction)
        .into_iter()
        .map(|located| located.issue)
        .collect()
}

fn compare_inner(
    base: &TypeRef,
    head: &TypeRef,
    direction: TypeDirection,
    pointer: &str,
    issues: &mut Issues,
) {
    if normalized(base) == normalized(head) {
        // Identical after normalisation, but an `Unknown` on both sides is
        // still an unverified path rather than a verified-equal one.
        collect_partials(base, pointer, issues);
        return;
    }

    match (base, head) {
        (TypeRef::Unknown(kind), other) | (other, TypeRef::Unknown(kind)) => {
            issues.push(TypeIssue::Partial {
                pointer: pointer.to_owned(),
                detail: kind.describe(),
            });
            // The other side may itself hide further unmodelled constructs.
            collect_partials(other, pointer, issues);
        }
        (TypeRef::Cycle(base_name), TypeRef::Cycle(head_name)) => {
            if base_name != head_name {
                push_changed(
                    direction,
                    pointer,
                    format!("recursive type changed from `{base_name}` to `{head_name}`"),
                    issues,
                );
            }
        }
        (
            TypeRef::Scalar {
                ty: base_ty,
                format: base_format,
                nullable: base_nullable,
                constraints: base_constraints,
            },
            TypeRef::Scalar {
                ty: head_ty,
                format: head_format,
                nullable: head_nullable,
                constraints: head_constraints,
            },
        ) => {
            if base_ty != head_ty {
                push_changed(
                    direction,
                    pointer,
                    format!("type changed from `{base_ty}` to `{head_ty}`"),
                    issues,
                );
                return;
            }
            compare_scalar_format(
                direction,
                pointer,
                base_format.as_deref(),
                head_format.as_deref(),
                issues,
            );
            compare_nullable(direction, pointer, *base_nullable, *head_nullable, issues);
            compare_constraints(
                direction,
                pointer,
                base_constraints,
                head_constraints,
                issues,
            );
        }
        (
            TypeRef::Enum {
                values: base_values,
                numbers: base_numbers,
            },
            TypeRef::Enum {
                values: head_values,
                numbers: head_numbers,
            },
        ) => {
            compare_enum_numbers(pointer, base_numbers, head_numbers, issues);
            compare_enum(direction, pointer, base_values, head_values, issues);
        }
        (
            TypeRef::Array {
                items: base_items,
                nullable: base_nullable,
            },
            TypeRef::Array {
                items: head_items,
                nullable: head_nullable,
            },
        ) => {
            compare_nullable(direction, pointer, *base_nullable, *head_nullable, issues);
            compare_inner(
                base_items,
                head_items,
                direction,
                &format!("{pointer}/items"),
                issues,
            );
        }
        (
            TypeRef::Object {
                fields: base_fields,
                additional: base_additional,
                nullable: base_nullable,
            },
            TypeRef::Object {
                fields: head_fields,
                additional: head_additional,
                nullable: head_nullable,
            },
        ) => {
            compare_nullable(direction, pointer, *base_nullable, *head_nullable, issues);
            if direction == TypeDirection::Request && *base_additional && !head_additional {
                issues.push(TypeIssue::RequestTypeNarrowed {
                    pointer: pointer.to_owned(),
                    reason: "`additionalProperties` tightened from true to false".to_owned(),
                });
            }
            compare_object_fields(direction, pointer, base_fields, head_fields, issues);
        }
        (
            TypeRef::Tuple {
                prefix_items: base_items,
                additional_items: base_additional,
                nullable: base_nullable,
            },
            TypeRef::Tuple {
                prefix_items: head_items,
                additional_items: head_additional,
                nullable: head_nullable,
            },
        ) => {
            compare_nullable(direction, pointer, *base_nullable, *head_nullable, issues);
            let min_len = base_items.len().min(head_items.len());
            for i in 0..min_len {
                compare_inner(
                    &base_items[i],
                    &head_items[i],
                    direction,
                    &format!("{pointer}/prefixItems/{i}"),
                    issues,
                );
            }
            match direction {
                TypeDirection::Request => {
                    if head_items.len() > base_items.len() && base_additional.is_none() {
                        issues.push(TypeIssue::RequestTypeNarrowed {
                            pointer: pointer.to_owned(),
                            reason: format!(
                                "tuple prefixItems expanded from {} to {} required elements",
                                base_items.len(),
                                head_items.len()
                            ),
                        });
                    }
                }
                TypeDirection::Response => {
                    if head_items.len() < base_items.len() && head_additional.is_none() {
                        push_changed(
                            direction,
                            pointer,
                            format!(
                                "tuple prefixItems reduced from {} to {} elements",
                                base_items.len(),
                                head_items.len()
                            ),
                            issues,
                        );
                    }
                }
            }
            match (base_additional, head_additional) {
                (Some(base_add), Some(head_add)) => {
                    compare_inner(
                        base_add,
                        head_add,
                        direction,
                        &format!("{pointer}/items"),
                        issues,
                    );
                }
                (Some(_), None) if direction == TypeDirection::Request => {
                    issues.push(TypeIssue::RequestTypeNarrowed {
                        pointer: pointer.to_owned(),
                        reason: "additional tuple items disallowed".to_owned(),
                    });
                }
                _ => {}
            }
        }
        (
            TypeRef::OneOf {
                variants: base_variants,
                discriminator: base_disc,
            },
            TypeRef::OneOf {
                variants: head_variants,
                discriminator: head_disc,
            },
        ) => {
            if let (Some(base_d), Some(head_d)) = (base_disc, head_disc) {
                if base_d.property_name != head_d.property_name {
                    push_changed(
                        direction,
                        pointer,
                        format!(
                            "discriminator property changed from `{}` to `{}`",
                            base_d.property_name, head_d.property_name
                        ),
                        issues,
                    );
                }
                for (key, target) in &base_d.mapping {
                    match head_d.mapping.get(key) {
                        Some(head_target) if head_target != target => {
                            push_changed(
                                direction,
                                pointer,
                                format!(
                                    "discriminator mapping for `{key}` changed from `{target}` to `{head_target}`"
                                ),
                                issues,
                            );
                        }
                        None => match direction {
                            TypeDirection::Request => {
                                issues.push(TypeIssue::RequestTypeNarrowed {
                                    pointer: pointer.to_owned(),
                                    reason: format!(
                                        "discriminator mapping key `{key}` was removed"
                                    ),
                                });
                            }
                            TypeDirection::Response => {
                                push_changed(
                                    direction,
                                    pointer,
                                    format!("discriminator mapping key `{key}` was removed"),
                                    issues,
                                );
                            }
                        },
                        _ => {}
                    }
                }
            } else if base_disc.is_some() && head_disc.is_none() {
                push_changed(
                    direction,
                    pointer,
                    "discriminator was removed".to_owned(),
                    issues,
                );
            }

            let base_set = variant_set(base_variants);
            let head_set = variant_set(head_variants);
            let removed = base_set.difference(&head_set).count();
            let added = head_set.difference(&base_set).count();

            match direction {
                // Removing a variant stops accepting input that used to work;
                // adding one is safe for a producer to send.
                TypeDirection::Request => {
                    if removed > 0 {
                        issues.push(TypeIssue::RequestVariantRemoved {
                            pointer: pointer.to_owned(),
                        });
                    }
                }
                // Adding a variant breaks an exhaustive reader; removing one
                // breaks a reader that still handles it.
                TypeDirection::Response => {
                    if added > 0 {
                        issues.push(TypeIssue::ResponseVariantAdded {
                            pointer: pointer.to_owned(),
                        });
                    }
                    if removed > 0 {
                        push_changed(
                            direction,
                            pointer,
                            "a `oneOf` variant was removed from the response".to_owned(),
                            issues,
                        );
                    }
                }
            }
            for variant in base_variants.iter().chain(head_variants) {
                collect_partials(variant, pointer, issues);
            }
        }
        (base_other, head_other) => {
            push_changed(
                direction,
                pointer,
                format!(
                    "type changed from {} to {}",
                    describe(base_other),
                    describe(head_other)
                ),
                issues,
            );
        }
    }
}

fn compare_object_fields(
    direction: TypeDirection,
    pointer: &str,
    base_fields: &BTreeMap<String, Field>,
    head_fields: &BTreeMap<String, Field>,
    issues: &mut Issues,
) {
    // Where the format has wire numbers, the number is the field's identity:
    // a rename with a stable number is wire-compatible and a renumber with a
    // stable name is a hard break. Comparing by name would report the first
    // and miss the second, which is exactly backwards.
    let pairs = if uses_wire_numbers(base_fields) && uses_wire_numbers(head_fields) {
        pair_by_number(base_fields, head_fields, pointer, issues)
    } else {
        pair_by_name(base_fields, head_fields)
    };

    for pairing in pairs {
        match pairing {
            Pairing::Both {
                name,
                base_field,
                head_field,
            } => {
                let field_pointer = field_pointer(pointer, name);
                issues.within(head_field.span.clone(), |issues| {
                    if direction == TypeDirection::Request
                        && !base_field.required
                        && head_field.required
                    {
                        issues.push(TypeIssue::RequestTypeNarrowed {
                            pointer: field_pointer.clone(),
                            reason: format!("field `{name}` became required"),
                        });
                    }
                    if direction == TypeDirection::Response
                        && base_field.required
                        && !head_field.required
                    {
                        issues.push(TypeIssue::ResponseFieldOptional {
                            pointer: field_pointer.clone(),
                            field: name.to_owned(),
                        });
                    }
                    compare_inner(
                        &base_field.ty,
                        &head_field.ty,
                        direction,
                        &field_pointer,
                        issues,
                    );
                });
            }
            Pairing::BaseOnly { name, base_field } => {
                let field_pointer = field_pointer(pointer, name);
                issues.span = base_field.span.clone();
                match direction {
                    TypeDirection::Request => issues.push(TypeIssue::RequestTypeNarrowed {
                        pointer: field_pointer,
                        reason: format!(
                            "request field `{name}` was removed and is no longer accepted"
                        ),
                    }),
                    TypeDirection::Response => issues.push(TypeIssue::ResponseFieldRemoved {
                        pointer: field_pointer,
                        field: name.to_owned(),
                    }),
                }
                issues.span = None;
            }
            Pairing::HeadOnly { name, head_field } => {
                let field_pointer = field_pointer(pointer, name);
                issues.span = head_field.span.clone();
                match (direction, head_field.required) {
                    (TypeDirection::Request, true) => {
                        issues.push(TypeIssue::RequestFieldAddedRequired {
                            pointer: field_pointer.clone(),
                            field: name.to_owned(),
                        });
                    }
                    (TypeDirection::Request, false) => {
                        issues.push(TypeIssue::RequestFieldAddedOptional {
                            pointer: field_pointer.clone(),
                            field: name.to_owned(),
                        });
                    }
                    (TypeDirection::Response, _) => {
                        issues.push(TypeIssue::ResponseFieldAdded {
                            pointer: field_pointer.clone(),
                            field: name.to_owned(),
                        });
                    }
                }
                collect_partials(&head_field.ty, &field_pointer, issues);
                issues.span = None;
            }
        }
    }
}

/// How a field on one side lines up with the other.
///
/// Borrows rather than clones: a `Field` carries its span and its whole type,
/// and the pairs are consumed in the loop that builds them.
enum Pairing<'a> {
    Both {
        name: &'a str,
        base_field: &'a Field,
        head_field: &'a Field,
    },
    BaseOnly {
        name: &'a str,
        base_field: &'a Field,
    },
    HeadOnly {
        name: &'a str,
        head_field: &'a Field,
    },
}

fn uses_wire_numbers(fields: &BTreeMap<String, Field>) -> bool {
    !fields.is_empty() && fields.values().all(|field| field.number.is_some())
}

fn pair_by_name<'a>(
    base_fields: &'a BTreeMap<String, Field>,
    head_fields: &'a BTreeMap<String, Field>,
) -> Vec<Pairing<'a>> {
    let mut pairs = Vec::new();
    for (name, base_field) in base_fields {
        match head_fields.get(name) {
            Some(head_field) => pairs.push(Pairing::Both {
                name,
                base_field,
                head_field,
            }),
            None => pairs.push(Pairing::BaseOnly { name, base_field }),
        }
    }
    for (name, head_field) in head_fields {
        if !base_fields.contains_key(name) {
            pairs.push(Pairing::HeadOnly { name, head_field });
        }
    }
    pairs
}

fn pair_by_number<'a>(
    base_fields: &'a BTreeMap<String, Field>,
    head_fields: &'a BTreeMap<String, Field>,
    pointer: &str,
    issues: &mut Issues,
) -> Vec<Pairing<'a>> {
    let by_number = |fields: &'a BTreeMap<String, Field>| {
        fields
            .iter()
            .filter_map(|(name, field)| field.number.map(|n| (n, (name.as_str(), field))))
            .collect::<BTreeMap<i32, (&'a str, &'a Field)>>()
    };
    let base_by_number = by_number(base_fields);
    let head_by_number = by_number(head_fields);

    // A field that kept its name but moved number is a wire break, and the
    // number-keyed pairing below would otherwise report it as one field
    // removed and an unrelated one added.
    for (base_name, base_field) in base_fields {
        let (Some(base_number), Some(head_field)) = (base_field.number, head_fields.get(base_name))
        else {
            continue;
        };
        if let Some(head_number) = head_field.number
            && head_number != base_number
        {
            issues.within(head_field.span.clone(), |issues| {
                issues.push(TypeIssue::FieldNumberChanged {
                    pointer: field_pointer(pointer, base_name),
                    field: base_name.clone(),
                    from: base_number,
                    to: head_number,
                });
            });
        }
    }

    let mut pairs = Vec::new();
    for (number, (base_name, base_field)) in &base_by_number {
        match head_by_number.get(number) {
            Some((head_name, head_field)) => {
                if head_name != base_name {
                    issues.within(head_field.span.clone(), |issues| {
                        issues.push(TypeIssue::FieldRenamed {
                            pointer: field_pointer(pointer, base_name),
                            from: (*base_name).to_owned(),
                            to: (*head_name).to_owned(),
                        });
                    });
                }
                pairs.push(Pairing::Both {
                    name: base_name,
                    base_field,
                    head_field,
                });
            }
            None => pairs.push(Pairing::BaseOnly {
                name: base_name,
                base_field,
            }),
        }
    }
    for (number, (head_name, head_field)) in &head_by_number {
        if !base_by_number.contains_key(number) {
            pairs.push(Pairing::HeadOnly {
                name: head_name,
                head_field,
            });
        }
    }
    pairs
}

fn compare_enum(
    direction: TypeDirection,
    pointer: &str,
    base_values: &BTreeSet<String>,
    head_values: &BTreeSet<String>,
    issues: &mut Issues,
) {
    let removed = base_values.difference(head_values).count();
    let added = head_values.difference(base_values).count();

    match direction {
        TypeDirection::Request => {
            if removed > 0 {
                let gone = base_values
                    .difference(head_values)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                issues.push(TypeIssue::RequestTypeNarrowed {
                    pointer: pointer.to_owned(),
                    reason: format!("enum no longer accepts: {gone}"),
                });
            }
        }
        TypeDirection::Response => {
            if removed > 0 {
                let gone = base_values
                    .difference(head_values)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                push_changed(
                    direction,
                    pointer,
                    format!("response enum no longer produces: {gone}"),
                    issues,
                );
            }
            if added > 0 {
                issues.push(TypeIssue::ResponseEnumExtended {
                    pointer: pointer.to_owned(),
                });
            }
        }
    }
}

/// An enum value's wire number is its identity where the format has one, for
/// the same reason a message field's is: renumbering changes what the bytes
/// mean to every already-deployed client.
fn compare_enum_numbers(
    pointer: &str,
    base_numbers: &BTreeMap<String, i32>,
    head_numbers: &BTreeMap<String, i32>,
    issues: &mut Issues,
) {
    if base_numbers.is_empty() || head_numbers.is_empty() {
        return;
    }

    for (name, base_number) in base_numbers {
        if let Some(head_number) = head_numbers.get(name)
            && head_number != base_number
        {
            issues.push(TypeIssue::FieldNumberChanged {
                pointer: field_pointer(pointer, name),
                field: name.clone(),
                from: *base_number,
                to: *head_number,
            });
        }
    }

    let invert = |numbers: &BTreeMap<String, i32>| {
        numbers
            .iter()
            .map(|(name, number)| (*number, name.clone()))
            .collect::<BTreeMap<i32, String>>()
    };
    for (number, base_name) in invert(base_numbers) {
        if let Some(head_name) = invert(head_numbers).get(&number)
            && *head_name != base_name
        {
            issues.push(TypeIssue::FieldRenamed {
                pointer: field_pointer(pointer, &base_name),
                from: base_name.clone(),
                to: head_name.clone(),
            });
        }
    }
}

fn compare_scalar_format(
    direction: TypeDirection,
    pointer: &str,
    base_format: Option<&str>,
    head_format: Option<&str>,
    issues: &mut Issues,
) {
    if base_format == head_format {
        return;
    }
    match (direction, base_format, head_format) {
        // Constraining a previously unconstrained input rejects callers.
        (TypeDirection::Request, None, Some(added)) => {
            issues.push(TypeIssue::RequestTypeNarrowed {
                pointer: pointer.to_owned(),
                reason: format!("format `{added}` now constrains a previously free value"),
            });
        }
        // Relaxing an input, or documenting an output more precisely, is safe.
        (TypeDirection::Request, Some(_), None) | (TypeDirection::Response, None, Some(_)) => {}
        (_, base, head) => push_changed(
            direction,
            pointer,
            format!(
                "format changed from `{}` to `{}`",
                base.unwrap_or("none"),
                head.unwrap_or("none")
            ),
            issues,
        ),
    }
}

fn compare_nullable(
    direction: TypeDirection,
    pointer: &str,
    base_nullable: bool,
    head_nullable: bool,
    issues: &mut Issues,
) {
    match (direction, base_nullable, head_nullable) {
        // Refusing a null that used to be accepted breaks a caller sending it.
        (TypeDirection::Request, true, false) => issues.push(TypeIssue::RequestTypeNarrowed {
            pointer: pointer.to_owned(),
            reason: "no longer accepts null".to_owned(),
        }),
        // Producing a null where one was never possible breaks a reader whose
        // type for this field is not optional.
        (TypeDirection::Response, false, true) => push_changed(
            direction,
            pointer,
            "may now be null, where it previously could not be".to_owned(),
            issues,
        ),
        _ => {}
    }
}

fn compare_constraints(
    direction: TypeDirection,
    pointer: &str,
    base: &Constraints,
    head: &Constraints,
    issues: &mut Issues,
) {
    if base == head {
        return;
    }

    let mut narrowings = Vec::new();
    if tightened_upper(base.max_length, head.max_length) {
        narrowings.push(format!(
            "`maxLength` tightened from {} to {}",
            describe_bound(base.max_length),
            describe_bound(head.max_length)
        ));
    }
    if tightened_lower(base.min_length, head.min_length) {
        narrowings.push(format!(
            "`minLength` tightened from {} to {}",
            describe_bound(base.min_length),
            describe_bound(head.min_length)
        ));
    }
    if tightened_numeric_upper(base.maximum.as_deref(), head.maximum.as_deref()) {
        narrowings.push(format!(
            "`maximum` tightened from {} to {}",
            base.maximum.as_deref().unwrap_or("unbounded"),
            head.maximum.as_deref().unwrap_or("unbounded")
        ));
    }
    if tightened_numeric_lower(base.minimum.as_deref(), head.minimum.as_deref()) {
        narrowings.push(format!(
            "`minimum` tightened from {} to {}",
            base.minimum.as_deref().unwrap_or("unbounded"),
            head.minimum.as_deref().unwrap_or("unbounded")
        ));
    }
    if base.pattern != head.pattern && head.pattern.is_some() {
        narrowings.push(match &base.pattern {
            // Two patterns cannot be compared for containment without
            // executing them, so any change is treated as a narrowing rather
            // than assumed safe.
            Some(previous) => format!(
                "`pattern` changed from `{previous}` to `{}`",
                head.pattern.as_deref().unwrap_or_default()
            ),
            None => format!(
                "`pattern` `{}` now constrains a previously free value",
                head.pattern.as_deref().unwrap_or_default()
            ),
        });
    }

    if narrowings.is_empty() {
        return;
    }
    let reason = narrowings.join("; ");
    match direction {
        TypeDirection::Request => issues.push(TypeIssue::RequestTypeNarrowed {
            pointer: pointer.to_owned(),
            reason,
        }),
        // A narrower output is not a break on its own: a reader that coped
        // with the wider range still copes. Only the request side fires.
        TypeDirection::Response => {}
    }
}

fn tightened_upper(base: Option<u64>, head: Option<u64>) -> bool {
    match (base, head) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(base), Some(head)) => head < base,
    }
}

fn tightened_lower(base: Option<u64>, head: Option<u64>) -> bool {
    match (base, head) {
        (_, None) => false,
        (None, Some(head)) => head > 0,
        (Some(base), Some(head)) => head > base,
    }
}

fn tightened_numeric_upper(base: Option<&str>, head: Option<&str>) -> bool {
    match (base, head) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(base), Some(head)) => match (base.parse::<f64>(), head.parse::<f64>()) {
            (Ok(base), Ok(head)) => head < base,
            // An unparseable bound that changed is reported rather than
            // ignored: silently skipping it would be a false clean.
            _ => base != head,
        },
    }
}

fn tightened_numeric_lower(base: Option<&str>, head: Option<&str>) -> bool {
    match (base, head) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(base), Some(head)) => match (base.parse::<f64>(), head.parse::<f64>()) {
            (Ok(base), Ok(head)) => head > base,
            _ => base != head,
        },
    }
}

fn describe_bound(bound: Option<u64>) -> String {
    bound.map_or_else(|| "unbounded".to_owned(), |value| value.to_string())
}

/// Walk a type that is not being structurally compared and report anything on
/// it that could not be modelled. Without this, an `Unknown` nested inside a
/// type that is equal on both sides would read as verified.
fn collect_partials(ty: &TypeRef, pointer: &str, issues: &mut Issues) {
    match ty {
        TypeRef::Unknown(kind) => issues.push(TypeIssue::Partial {
            pointer: pointer.to_owned(),
            detail: kind.describe(),
        }),
        TypeRef::Array { items, .. } => {
            collect_partials(items, &format!("{pointer}/items"), issues);
        }
        TypeRef::Tuple {
            prefix_items,
            additional_items,
            ..
        } => {
            for (index, item) in prefix_items.iter().enumerate() {
                collect_partials(item, &format!("{pointer}/prefixItems/{index}"), issues);
            }
            if let Some(additional) = additional_items {
                collect_partials(additional, &format!("{pointer}/items"), issues);
            }
        }
        TypeRef::Object { fields, .. } => {
            for (name, field) in fields {
                collect_partials(&field.ty, &field_pointer(pointer, name), issues);
            }
        }
        TypeRef::OneOf { variants, .. } => {
            for (index, variant) in variants.iter().enumerate() {
                collect_partials(variant, &format!("{pointer}/{index}"), issues);
            }
        }
        TypeRef::Scalar { .. } | TypeRef::Enum { .. } | TypeRef::Cycle(_) => {}
    }
}

fn push_changed(direction: TypeDirection, pointer: &str, reason: String, issues: &mut Issues) {
    match direction {
        TypeDirection::Request => issues.push(TypeIssue::RequestTypeNarrowed {
            pointer: pointer.to_owned(),
            reason,
        }),
        TypeDirection::Response => issues.push(TypeIssue::ResponseTypeChanged {
            pointer: pointer.to_owned(),
            reason,
        }),
    }
}

fn field_pointer(pointer: &str, name: &str) -> String {
    if name.is_empty() {
        return pointer.to_owned();
    }
    format!("{pointer}/{}", name.replace('~', "~0").replace('/', "~1"))
}

fn describe(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Scalar { ty, .. } => format!("`{ty}`"),
        TypeRef::Enum { .. } => "an enum".to_owned(),
        TypeRef::Array { .. } => "an array".to_owned(),
        TypeRef::Tuple { .. } => "a tuple".to_owned(),
        TypeRef::Object { .. } => "an object".to_owned(),
        TypeRef::OneOf { .. } => "a union".to_owned(),
        TypeRef::Cycle(name) => format!("a reference to `{name}`"),
        TypeRef::Unknown(_) => "an unmodelled construct".to_owned(),
    }
}

/// Fold the spellings that mean the same thing, so a faithful translation
/// between OpenAPI 3.0 and 3.1 registers as no change at all.
fn normalized(ty: &TypeRef) -> TypeRef {
    match ty {
        TypeRef::Tuple {
            prefix_items,
            additional_items,
            nullable,
        } => TypeRef::Tuple {
            prefix_items: prefix_items.iter().map(normalized).collect(),
            additional_items: additional_items
                .as_ref()
                .map(|item| Box::new(normalized(item))),
            nullable: *nullable,
        },
        TypeRef::OneOf {
            variants,
            discriminator,
        } => fold_nullable_union(variants).unwrap_or(TypeRef::OneOf {
            variants: {
                let mut folded = variants.iter().map(normalized).collect::<Vec<_>>();
                folded.sort_by_key(|variant| format!("{variant:?}"));
                folded
            },
            discriminator: discriminator.clone(),
        }),
        TypeRef::Array { items, nullable } => TypeRef::Array {
            items: Box::new(normalized(items)),
            nullable: *nullable,
        },
        TypeRef::Object {
            fields,
            additional,
            nullable,
        } => TypeRef::Object {
            fields: fields
                .iter()
                .map(|(name, field)| {
                    (
                        name.clone(),
                        Field {
                            ty: normalized(&field.ty),
                            required: field.required,
                            deprecated: field.deprecated,
                            number: field.number,
                            span: field.span.clone(),
                        },
                    )
                })
                .collect(),
            additional: *additional,
            nullable: *nullable,
        },
        _ => ty.clone(),
    }
}

/// `oneOf: [T, null]` in 3.1 means what `nullable: true` meant in 3.0.
fn fold_nullable_union(variants: &[TypeRef]) -> Option<TypeRef> {
    if variants.len() != 2 {
        return None;
    }
    let mut saw_null = false;
    let mut other = None;
    for variant in variants {
        match variant {
            TypeRef::Scalar { ty, .. } if ty == "null" => saw_null = true,
            candidate => other = Some(candidate),
        }
    }
    if !saw_null {
        return None;
    }

    match normalized(other?) {
        TypeRef::Scalar {
            ty,
            format,
            constraints,
            ..
        } => Some(TypeRef::Scalar {
            ty,
            format,
            nullable: true,
            constraints,
        }),
        TypeRef::Array { items, .. } => Some(TypeRef::Array {
            items,
            nullable: true,
        }),
        TypeRef::Object {
            fields, additional, ..
        } => Some(TypeRef::Object {
            fields,
            additional,
            nullable: true,
        }),
        _ => None,
    }
}

fn variant_set(variants: &[TypeRef]) -> BTreeSet<String> {
    variants
        .iter()
        .map(|variant| format!("{:?}", normalized(variant)))
        .collect()
}

/// Does this type carry anything the ingester could not model?
#[must_use]
pub fn has_unmodelled(ty: &TypeRef) -> bool {
    let mut issues = Issues::default();
    collect_partials(ty, "", &mut issues);
    !issues.found.is_empty()
}

/// The kinds present on a type, for reporting without a comparison.
#[must_use]
pub fn unmodelled_kinds(ty: &TypeRef) -> Vec<UnmodelledKind> {
    fn walk(ty: &TypeRef, out: &mut Vec<UnmodelledKind>) {
        match ty {
            TypeRef::Unknown(kind) => out.push(kind.clone()),
            TypeRef::Array { items, .. } => walk(items, out),
            TypeRef::Tuple {
                prefix_items,
                additional_items,
                ..
            } => {
                for item in prefix_items {
                    walk(item, out);
                }
                if let Some(additional) = additional_items {
                    walk(additional, out);
                }
            }
            TypeRef::Object { fields, .. } => {
                for field in fields.values() {
                    walk(&field.ty, out);
                }
            }
            TypeRef::OneOf { variants, .. } => {
                for variant in variants {
                    walk(variant, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(ty, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::Discriminator;
    use std::collections::{BTreeMap, BTreeSet};

    fn scalar(ty: &str, nullable: bool) -> TypeRef {
        TypeRef::Scalar {
            ty: ty.to_owned(),
            format: None,
            nullable,
            constraints: Constraints::default(),
        }
    }

    fn object(fields: Vec<(&str, TypeRef, bool)>, additional: bool) -> TypeRef {
        TypeRef::Object {
            fields: fields
                .into_iter()
                .map(|(name, ty, required)| (name.to_owned(), Field::new(ty, required)))
                .collect(),
            additional,
            nullable: false,
        }
    }

    fn numbered(fields: Vec<(&str, i32, TypeRef)>) -> TypeRef {
        TypeRef::Object {
            fields: fields
                .into_iter()
                .map(|(name, number, ty)| {
                    (
                        name.to_owned(),
                        Field {
                            ty,
                            required: false,
                            deprecated: false,
                            number: Some(number),
                            span: None,
                        },
                    )
                })
                .collect(),
            additional: false,
            nullable: false,
        }
    }

    #[test]
    fn identical_types_produce_nothing() {
        assert!(
            compare_request_type(&scalar("string", false), &scalar("string", false)).is_empty()
        );
    }

    #[test]
    fn cycle_with_same_name_is_compatible() {
        assert!(
            compare_request_type(
                &TypeRef::Cycle("Payment".to_owned()),
                &TypeRef::Cycle("Payment".to_owned())
            )
            .is_empty()
        );
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
    fn unknown_on_either_side_reports_partial_never_clean() {
        let unknown = TypeRef::Unknown(UnmodelledKind::ExternalRef("common.yaml#/X".to_owned()));
        let issues = compare_response_type(
            &object(vec![("id", scalar("string", false), true)], true),
            &unknown,
        );
        assert!(
            issues
                .iter()
                .any(|issue| matches!(issue, TypeIssue::Partial { .. })),
            "an unmodelled construct must never compare as clean: {issues:?}"
        );
    }

    #[test]
    fn unknown_identical_on_both_sides_is_still_partial() {
        let unknown = TypeRef::Unknown(UnmodelledKind::SchemaDeferred);
        let issues = compare_response_type(&unknown, &unknown);
        assert!(matches!(issues[0], TypeIssue::Partial { .. }));
    }

    #[test]
    fn unknown_nested_in_an_equal_type_is_still_partial() {
        let ty = object(
            vec![(
                "child",
                TypeRef::Unknown(UnmodelledKind::Unsupported("not".to_owned())),
                false,
            )],
            true,
        );
        let issues = compare_response_type(&ty, &ty);
        assert_eq!(issues.len(), 1);
        assert!(matches!(
            &issues[0],
            TypeIssue::Partial { pointer, .. } if pointer == "/child"
        ));
    }

    #[test]
    fn response_field_removal_is_its_own_issue_with_a_pointer() {
        let base = object(
            vec![
                ("id", scalar("string", false), true),
                ("customer_id", scalar("string", false), true),
            ],
            true,
        );
        let head = object(vec![("id", scalar("string", false), true)], true);

        let issues = compare_response_type(&base, &head);
        assert!(issues.iter().any(|issue| matches!(
            issue,
            TypeIssue::ResponseFieldRemoved { field, pointer }
                if field == "customer_id" && pointer == "/customer_id"
        )));
    }

    #[test]
    fn response_field_becoming_optional_is_its_own_issue() {
        let base = object(vec![("id", scalar("string", false), true)], true);
        let head = object(vec![("id", scalar("string", false), false)], true);

        let issues = compare_response_type(&base, &head);
        assert!(issues.iter().any(|issue| matches!(
            issue,
            TypeIssue::ResponseFieldOptional { field, .. } if field == "id"
        )));
    }

    #[test]
    fn added_response_field_is_additive_and_reported_separately() {
        let base = object(vec![("id", scalar("string", false), true)], true);
        let head = object(
            vec![
                ("id", scalar("string", false), true),
                ("extra", scalar("string", false), false),
            ],
            true,
        );

        let issues = compare_response_type(&base, &head);
        assert_eq!(issues.len(), 1);
        assert!(matches!(
            &issues[0],
            TypeIssue::ResponseFieldAdded { field, .. } if field == "extra"
        ));
    }

    #[test]
    fn request_detects_oneof_variant_removal_and_ignores_addition() {
        let base = TypeRef::OneOf {
            variants: vec![scalar("string", false), scalar("integer", false)],
            discriminator: None,
        };
        let head = TypeRef::OneOf {
            variants: vec![scalar("string", false)],
            discriminator: None,
        };

        assert!(
            compare_request_type(&base, &head)
                .iter()
                .any(|issue| matches!(issue, TypeIssue::RequestVariantRemoved { .. }))
        );
        assert!(compare_request_type(&head, &base).is_empty());
    }

    #[test]
    fn response_detects_oneof_variant_addition() {
        let base = TypeRef::OneOf {
            variants: vec![scalar("string", false)],
            discriminator: None,
        };
        let head = TypeRef::OneOf {
            variants: vec![scalar("string", false), scalar("integer", false)],
            discriminator: None,
        };

        assert!(
            compare_response_type(&base, &head)
                .iter()
                .any(|issue| matches!(issue, TypeIssue::ResponseVariantAdded { .. }))
        );
    }

    #[test]
    fn request_detects_additional_properties_tightening() {
        let issues = compare_request_type(&object(vec![], true), &object(vec![], false));
        assert!(issues.iter().any(|issue| matches!(
            issue,
            TypeIssue::RequestTypeNarrowed { reason, .. }
                if reason.contains("additionalProperties")
        )));
    }

    #[test]
    fn normalizes_openapi_nullable_between_30_and_31() {
        let openapi_30 = scalar("string", true);
        let openapi_31 = TypeRef::OneOf {
            variants: vec![scalar("string", false), scalar("null", false)],
            discriminator: None,
        };

        assert!(compare_request_type(&openapi_30, &openapi_31).is_empty());
        assert!(compare_response_type(&openapi_30, &openapi_31).is_empty());
    }

    #[test]
    fn request_enum_narrowing_is_detected_and_widening_is_not() {
        let wide = TypeRef::Enum {
            values: BTreeSet::from(["pending".to_owned(), "paid".to_owned()]),
            numbers: BTreeMap::new(),
        };
        let narrow = TypeRef::Enum {
            values: BTreeSet::from(["pending".to_owned()]),
            numbers: BTreeMap::new(),
        };

        assert!(compare_request_type(&wide, &narrow).iter().any(|issue| {
            matches!(issue, TypeIssue::RequestTypeNarrowed { reason, .. } if reason.contains("paid"))
        }));
        assert!(compare_request_type(&narrow, &wide).is_empty());
    }

    #[test]
    fn response_enum_extension_is_detected_and_narrowing_is_a_change() {
        let narrow = TypeRef::Enum {
            values: BTreeSet::from(["pending".to_owned()]),
            numbers: BTreeMap::new(),
        };
        let wide = TypeRef::Enum {
            values: BTreeSet::from(["pending".to_owned(), "paid".to_owned()]),
            numbers: BTreeMap::new(),
        };

        assert!(
            compare_response_type(&narrow, &wide)
                .iter()
                .any(|issue| matches!(issue, TypeIssue::ResponseEnumExtended { .. }))
        );
        assert!(
            compare_response_type(&wide, &narrow)
                .iter()
                .any(|issue| matches!(issue, TypeIssue::ResponseTypeChanged { .. }))
        );
    }

    #[test]
    fn request_required_field_addition_is_distinct_from_optional() {
        let base = object(vec![], true);
        let required = object(vec![("id", scalar("string", false), true)], true);
        let optional = object(vec![("id", scalar("string", false), false)], true);

        assert!(
            compare_request_type(&base, &required)
                .iter()
                .any(|issue| matches!(issue, TypeIssue::RequestFieldAddedRequired { .. }))
        );
        assert!(
            compare_request_type(&base, &optional)
                .iter()
                .any(|issue| matches!(issue, TypeIssue::RequestFieldAddedOptional { .. }))
        );
    }

    #[test]
    fn tightening_max_length_narrows_a_request() {
        let base = TypeRef::Scalar {
            ty: "string".to_owned(),
            format: None,
            nullable: false,
            constraints: Constraints {
                max_length: Some(100),
                ..Constraints::default()
            },
        };
        let head = TypeRef::Scalar {
            ty: "string".to_owned(),
            format: None,
            nullable: false,
            constraints: Constraints {
                max_length: Some(10),
                ..Constraints::default()
            },
        };

        assert!(compare_request_type(&base, &head).iter().any(|issue| {
            matches!(issue, TypeIssue::RequestTypeNarrowed { reason, .. } if reason.contains("maxLength"))
        }));
        // Relaxing it is safe.
        assert!(compare_request_type(&head, &base).is_empty());
    }

    #[test]
    fn adding_a_bound_where_there_was_none_narrows_a_request() {
        let base = scalar("integer", false);
        let head = TypeRef::Scalar {
            ty: "integer".to_owned(),
            format: None,
            nullable: false,
            constraints: Constraints {
                maximum: Some("10".to_owned()),
                ..Constraints::default()
            },
        };
        assert!(compare_request_type(&base, &head).iter().any(|issue| {
            matches!(issue, TypeIssue::RequestTypeNarrowed { reason, .. } if reason.contains("maximum"))
        }));
    }

    #[test]
    fn request_that_stops_accepting_null_is_narrowed() {
        assert!(
            compare_request_type(&scalar("string", true), &scalar("string", false))
                .iter()
                .any(|issue| matches!(
                    issue,
                    TypeIssue::RequestTypeNarrowed { reason, .. } if reason.contains("null")
                ))
        );
    }

    #[test]
    fn response_that_starts_producing_null_is_a_change() {
        assert!(
            compare_response_type(&scalar("string", false), &scalar("string", true))
                .iter()
                .any(|issue| matches!(issue, TypeIssue::ResponseTypeChanged { .. }))
        );
        // The reverse tightens the output, which no reader minds.
        assert!(
            compare_response_type(&scalar("string", true), &scalar("string", false)).is_empty()
        );
    }

    #[test]
    fn field_renumbered_is_a_wire_break() {
        let base = numbered(vec![("id", 1, scalar("string", false))]);
        let head = numbered(vec![("id", 7, scalar("string", false))]);

        let issues = compare_response_type(&base, &head);
        assert!(
            issues.iter().any(|issue| matches!(
                issue,
                TypeIssue::FieldNumberChanged { field, from, to, .. }
                    if field == "id" && *from == 1 && *to == 7
            )),
            "renumbering a field is the protobuf break: {issues:?}"
        );
    }

    #[test]
    fn field_renamed_with_a_stable_number_is_a_rename_not_a_removal() {
        let base = numbered(vec![("id", 1, scalar("string", false))]);
        let head = numbered(vec![("identifier", 1, scalar("string", false))]);

        let issues = compare_response_type(&base, &head);
        assert!(issues.iter().any(|issue| matches!(
            issue,
            TypeIssue::FieldRenamed { from, to, .. } if from == "id" && to == "identifier"
        )));
        assert!(
            !issues
                .iter()
                .any(|issue| matches!(issue, TypeIssue::ResponseFieldRemoved { .. })),
            "a stable wire number means the field was not removed: {issues:?}"
        );
    }

    #[test]
    fn numbered_field_removal_is_still_a_removal() {
        let base = numbered(vec![
            ("id", 1, scalar("string", false)),
            ("note", 2, scalar("string", false)),
        ]);
        let head = numbered(vec![("id", 1, scalar("string", false))]);

        assert!(
            compare_response_type(&base, &head)
                .iter()
                .any(|issue| matches!(
                    issue,
                    TypeIssue::ResponseFieldRemoved { field, .. } if field == "note"
                ))
        );
    }

    #[test]
    fn check_subtype_evaluates_covariance_and_contravariance() {
        let wide_str = scalar("string", true);
        let narrow_str = scalar("string", false);

        // Covariance in response: Head must be subtype of Base (Head <: Base)
        // narrow_str <: wide_str is valid
        let cov_valid = check_subtype(&narrow_str, &wide_str, TypeDirection::Response, "/response");
        assert!(cov_valid.is_valid());
        assert!(cov_valid.issues().is_empty());

        // wide_str <: narrow_str in response produces issues
        let cov_invalid =
            check_subtype(&wide_str, &narrow_str, TypeDirection::Response, "/response");
        assert!(!cov_invalid.is_valid());
        assert!(!cov_invalid.issues().is_empty());

        // Contravariance in request: Base must be subtype of Head (Base <: Head)
        // Base = narrow_str, Head = wide_str -> narrow_str <: wide_str is valid
        let contra_valid =
            check_subtype(&narrow_str, &wide_str, TypeDirection::Request, "/request");
        assert!(contra_valid.is_valid());
        assert!(contra_valid.issues().is_empty());

        // Base = wide_str, Head = narrow_str -> wide_str <: narrow_str in request produces narrowing issue
        let contra_invalid =
            check_subtype(&wide_str, &narrow_str, TypeDirection::Request, "/request");
        assert!(!contra_invalid.is_valid());
        assert!(!contra_invalid.issues().is_empty());
    }

    #[test]
    fn check_subtype_structural_objects_and_enums() {
        // Enums:
        let enum_ab = TypeRef::Enum {
            values: BTreeSet::from(["A".to_owned(), "B".to_owned()]),
            numbers: BTreeMap::new(),
        };
        let enum_abc = TypeRef::Enum {
            values: BTreeSet::from(["A".to_owned(), "B".to_owned(), "C".to_owned()]),
            numbers: BTreeMap::new(),
        };

        // Response: extending enum values in response produces ResponseEnumExtended issue
        let resp_extended = check_subtype(&enum_abc, &enum_ab, TypeDirection::Response, "/enum");
        assert!(!resp_extended.is_valid());
        assert!(
            resp_extended
                .issues()
                .iter()
                .any(|i| matches!(i, TypeIssue::ResponseEnumExtended { .. }))
        );

        // Request: enum_abc as Head accepts all variants of Base enum_ab (valid widening)
        assert!(check_subtype(&enum_ab, &enum_abc, TypeDirection::Request, "/enum").is_valid());
        // Request: enum_ab as Head rejects variant C from Base enum_abc (invalid narrowing)
        let req_narrowed = check_subtype(&enum_abc, &enum_ab, TypeDirection::Request, "/enum");
        assert!(!req_narrowed.is_valid());
        assert!(
            req_narrowed
                .issues()
                .iter()
                .any(|i| matches!(i, TypeIssue::RequestTypeNarrowed { .. }))
        );

        // Objects:
        let obj_base = object(vec![("id", scalar("string", false), true)], true);
        let obj_optional_field = object(
            vec![
                ("id", scalar("string", false), true),
                ("note", scalar("string", false), false),
            ],
            true,
        );
        let obj_required_field = object(
            vec![
                ("id", scalar("string", false), true),
                ("token", scalar("string", false), true),
            ],
            true,
        );

        // Response: adding a field to response produces ResponseFieldAdded issue (additive change)
        let resp_added = check_subtype(
            &obj_optional_field,
            &obj_base,
            TypeDirection::Response,
            "/obj",
        );
        assert!(
            resp_added
                .issues()
                .iter()
                .any(|i| matches!(i, TypeIssue::ResponseFieldAdded { .. }))
        );

        // Response: removing a field from response produces ResponseFieldRemoved issue
        let resp_removed = check_subtype(
            &obj_base,
            &obj_optional_field,
            TypeDirection::Response,
            "/obj",
        );
        assert!(
            resp_removed
                .issues()
                .iter()
                .any(|i| matches!(i, TypeIssue::ResponseFieldRemoved { .. }))
        );

        // Request: adding optional field to request produces RequestFieldAddedOptional issue (additive notice)
        let req_optional = check_subtype(
            &obj_base,
            &obj_optional_field,
            TypeDirection::Request,
            "/obj",
        );
        assert!(
            req_optional
                .issues()
                .iter()
                .any(|i| matches!(i, TypeIssue::RequestFieldAddedOptional { .. }))
        );

        // Request: adding required field to request is a break (incompatible)
        let req_break = check_subtype(
            &obj_base,
            &obj_required_field,
            TypeDirection::Request,
            "/obj",
        );
        assert!(!req_break.is_valid());
        assert!(
            req_break
                .issues()
                .iter()
                .any(|i| matches!(i, TypeIssue::RequestFieldAddedRequired { .. }))
        );

        // Arrays:
        let arr_narrow = TypeRef::Array {
            items: Box::new(scalar("string", false)),
            nullable: false,
        };
        let arr_wide = TypeRef::Array {
            items: Box::new(scalar("string", true)),
            nullable: false,
        };
        // Response: narrow item <: wide item in response is valid
        assert!(check_subtype(&arr_narrow, &arr_wide, TypeDirection::Response, "/arr").is_valid());
        // Response: wide item <: narrow item in response produces issue
        assert!(!check_subtype(&arr_wide, &arr_narrow, TypeDirection::Response, "/arr").is_valid());

        // Request: narrow item <: wide item in request is valid (Head accepts null)
        assert!(check_subtype(&arr_narrow, &arr_wide, TypeDirection::Request, "/arr").is_valid());
        // Request: wide item <: narrow item in request produces issue (Head rejects null)
        assert!(!check_subtype(&arr_wide, &arr_narrow, TypeDirection::Request, "/arr").is_valid());

        // Tuples:
        let tuple_2 = TypeRef::Tuple {
            prefix_items: vec![scalar("string", false), scalar("integer", false)],
            additional_items: None,
            nullable: false,
        };
        let tuple_3 = TypeRef::Tuple {
            prefix_items: vec![
                scalar("string", false),
                scalar("integer", false),
                scalar("boolean", false),
            ],
            additional_items: None,
            nullable: false,
        };
        // Response: tuple with fewer items reduces tuple length -> ResponseTypeChanged
        let resp_tuple_reduced = compare_response_type(&tuple_3, &tuple_2);
        assert!(
            resp_tuple_reduced
                .iter()
                .any(|i| matches!(i, TypeIssue::ResponseTypeChanged { .. }))
        );

        // Request: tuple requiring more items narrows request input -> RequestTypeNarrowed
        let req_tuple_expanded = compare_request_type(&tuple_2, &tuple_3);
        assert!(
            req_tuple_expanded
                .iter()
                .any(|i| matches!(i, TypeIssue::RequestTypeNarrowed { .. }))
        );

        // Discriminator mappings:
        let disc_base = Discriminator {
            property_name: "kind".to_owned(),
            mapping: BTreeMap::from([
                ("cat".to_owned(), "Cat".to_owned()),
                ("dog".to_owned(), "Dog".to_owned()),
            ]),
        };
        let disc_head_removed = Discriminator {
            property_name: "kind".to_owned(),
            mapping: BTreeMap::from([("cat".to_owned(), "Cat".to_owned())]),
        };
        let union_base = TypeRef::OneOf {
            variants: vec![scalar("string", false)],
            discriminator: Some(disc_base),
        };
        let union_head_removed = TypeRef::OneOf {
            variants: vec![scalar("string", false)],
            discriminator: Some(disc_head_removed),
        };

        // Removing a mapping in request narrows request
        let req_disc_removed = compare_request_type(&union_base, &union_head_removed);
        assert!(req_disc_removed.iter().any(
            |i| matches!(i, TypeIssue::RequestTypeNarrowed { reason, .. } if reason.contains("dog"))
        ));

        // Removing a mapping in response changes response
        let resp_disc_removed = compare_response_type(&union_base, &union_head_removed);
        assert!(resp_disc_removed.iter().any(
            |i| matches!(i, TypeIssue::ResponseTypeChanged { reason, .. } if reason.contains("dog"))
        ));
    }
}

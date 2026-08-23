//! The rustc-style text diagnostic.
//!
//! Rendered with `annotate-snippets`, the same renderer tropism uses, so the
//! two tools' output is indistinguishable in a hook. The rationale line is
//! rendered verbatim at the moment a developer is blocked, on the grounds that
//! this is when someone actually wants to know why the constraint exists.

use crate::Severity;
use crate::report::Report;
use crate::rules::{Finding, catalogue};

#[cfg(feature = "cli")]
use annotate_snippets::{AnnotationKind, Group, Level, Origin, Renderer, Snippet};

pub fn render(report: &Report) -> String {
    let mut out = String::new();

    for finding in &report.findings {
        out.push_str(&render_finding(report, finding));
        out.push('\n');
    }

    for unavailable in &report.unavailable {
        let contract = unavailable.contract.as_deref().unwrap_or("<unknown>");
        out.push_str(&format!(
            "error: brake could not check `{contract}`: {}\n\
             note: this is a tool failure, not an API break — exiting 2\n\n",
            unavailable.message
        ));
    }

    if report.findings.is_empty() && report.unavailable.is_empty() {
        out.push_str(&format!(
            "clean: {} contract{} checked, no findings at the selected threshold\n",
            report.contracts_checked,
            if report.contracts_checked == 1 {
                ""
            } else {
                "s"
            }
        ));
    }

    out
}

#[cfg(feature = "cli")]
fn render_finding(report: &Report, finding: &Finding) -> String {
    let level = match finding.severity {
        Severity::Error => Level::ERROR,
        Severity::Warning => Level::WARNING,
        Severity::Info => Level::INFO,
    };

    let rule = catalogue::lookup(finding.rule_id);
    let contract_note = format!("contract: `{}`", finding.contract);

    // The help block is what someone reads at the moment they are blocked, so
    // it carries what to *do*. The rationale for why the rule exists is a
    // paragraph they do not need in order to act, and lives in `brake explain`.
    let help = rule.map(|rule| help_text(rule, finding));

    // Only quote source when the artifact's text is on the report; a baseline
    // resolved from a git blob has text, one that failed to read does not.
    let quoted = finding.span.as_ref().and_then(|span| {
        let source = report.sources.get(&span.file)?;
        let (start, end) = line_range(source, span.line, span.column)?;
        Some((span, source.as_str(), start, end))
    });

    // The rule id only, without `id_url`: a terminal hyperlink writes OSC-8
    // escapes even from the plain renderer, and output has to stay byte-stable
    // and pipe-safe. The help URI is in the SARIF and in `brake explain`.
    let primary = level.primary_title(&finding.message).id(finding.rule_id);

    let mut group = match quoted {
        Some((span, source, start, end)) => primary.element(
            Snippet::source(source)
                .path(&span.file)
                .fold(true)
                .annotation(AnnotationKind::Primary.span(start..end).label("here")),
        ),
        None => {
            let group = primary.element(Level::NOTE.message(&contract_note));
            match &finding.span {
                Some(span) => group.element(
                    Origin::path(span.file.clone())
                        .line(span.line)
                        .char_column(span.column),
                ),
                None => group,
            }
        }
    };

    if quoted.is_some() {
        group = group.element(Level::NOTE.message(&contract_note));
    }

    let mut report_groups = vec![group];
    if let Some(help) = &help {
        report_groups.push(Group::with_title(Level::HELP.secondary_title(help)));
    }

    format!("{}\n", Renderer::plain().render(&report_groups))
}

/// Fallback when the crate is built without the `cli` feature: a consumer
/// embedding the library renders findings itself.
#[cfg(not(feature = "cli"))]
fn render_finding(_report: &Report, finding: &Finding) -> String {
    let mut out = format!(
        "{}[{}]: {}",
        severity_label(finding.severity),
        finding.rule_id,
        finding.message
    );
    if let Some(span) = &finding.span {
        out.push_str(&format!(
            "\n  --> {}:{}:{}",
            span.file, span.line, span.column
        ));
    }
    out.push_str(&format!("\n  = contract: {}\n", finding.contract));
    out
}

#[cfg(not(feature = "cli"))]
fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

/// The `help:` block: the ways out, then where to read more.
///
/// brake names the applicable strategies and what each costs, and does not
/// pick between them — which one fits depends on whether you control every
/// consumer and whether you have a version scheme, neither of which brake can
/// see. Saying so is more useful than a confident guess.
#[cfg(feature = "cli")]
fn help_text(rule: &catalogue::Rule, finding: &Finding) -> String {
    let remediations = finding.remediations();
    if remediations.is_empty() {
        return format!(
            "{}\n\nrun `brake explain {}` for the full rationale",
            rule.summary, rule.id
        );
    }

    let count = match remediations.len() {
        1 => "one way".to_owned(),
        2 => "two ways".to_owned(),
        3 => "three ways".to_owned(),
        other => format!("{other} ways"),
    };
    let mut out = format!("{count} to make this change safely\n");
    for (index, remediation) in remediations.iter().enumerate() {
        // No leading indent: annotate-snippets already indents the whole
        // help block, and the two compound into a hanging column.
        out.push_str(&format!(
            "{}. {} — {}\n   costs: {}\n",
            index + 1,
            remediation.strategy,
            remediation.summary,
            remediation.cost
        ));
    }
    // Only where there is a genuine choice: a single option is not a
    // decision, and saying otherwise reads as boilerplate.
    if remediations.len() > 1 {
        out.push_str(
            "\nwhich one fits depends on whether you control every consumer — brake \
cannot see that.\n",
        );
    } else {
        out.push('\n');
    }
    out.push_str(&format!(
        "run `brake explain {}` for why this breaks",
        rule.id
    ));
    out
}

/// Byte offsets for the annotated span, clamped to the line.
///
/// A one-based line and column from the ingester become the byte range
/// `annotate-snippets` needs, without assuming the file is ASCII.
#[cfg(feature = "cli")]
fn line_range(source: &str, line: usize, column: usize) -> Option<(usize, usize)> {
    let mut offset = 0;
    for (index, text) in source.split_inclusive('\n').enumerate() {
        if index + 1 == line {
            let trimmed = text.trim_end_matches(['\n', '\r']);
            let start_in_line = trimmed
                .char_indices()
                .nth(column.saturating_sub(1))
                .map_or(trimmed.len(), |(byte, _)| byte);
            let start = offset + start_in_line;
            // Underline the token at the column, or the rest of the line.
            let token_length = trimmed[start_in_line..]
                .find(|c: char| c.is_whitespace() || c == ':')
                .unwrap_or(trimmed.len() - start_in_line);
            let end = start + token_length.max(1);
            return Some((start, end.min(offset + trimmed.len()).max(start + 1)));
        }
        offset += text.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::Severity;
    use crate::contract::Span;
    use crate::report::{Report, Unavailable};
    use crate::rules::Finding;

    use super::render;

    fn finding(rule_id: &'static str, severity: Severity, span: Option<Span>) -> Finding {
        Finding {
            rule_id,
            severity,
            contract: "payments".to_owned(),
            message: "response field removed: field `customer_id`".to_owned(),
            method: Some("GET".to_owned()),
            path: Some("/payments/{id}".to_owned()),
            pointer: "/paths/~1payments~1{id}/get/responses/200".to_owned(),
            subject: Some("customer_id".to_owned()),
            span,
        }
    }

    fn report_with_source() -> Report {
        let mut report = Report::new(
            vec![finding(
                "response-field-removed",
                Severity::Error,
                Some(Span::new("api/openapi.yaml", 4, 9, "/paths")),
            )],
            Vec::new(),
            1,
        );
        report.sources.insert(
            "api/openapi.yaml".to_owned(),
            "openapi: 3.1.0\npaths:\n  /payments:\n        customer_id: gone\n".to_owned(),
        );
        report
    }

    #[test]
    fn quotes_the_offending_line_from_the_artifact() {
        let rendered = render(&report_with_source());
        assert!(rendered.contains("response-field-removed"), "{rendered}");
        assert!(rendered.contains("api/openapi.yaml"), "{rendered}");
        assert!(
            rendered.contains("customer_id"),
            "the diagnostic should quote the source line:\n{rendered}"
        );
        assert!(
            rendered.contains('^'),
            "the diagnostic should underline the span:\n{rendered}"
        );
    }

    #[test]
    fn tells_a_blocked_developer_how_to_make_the_change_safely() {
        let rendered = render(&report_with_source());

        assert!(
            rendered.contains("three ways to make this change safely"),
            "{rendered}"
        );
        assert!(rendered.contains("deprecate-then-remove"), "{rendered}");
        assert!(
            rendered.contains("`customer_id`"),
            "the strategy must name the field it is about:\n{rendered}"
        );
        // Options with no costs read as though they are all free.
        assert!(rendered.contains("costs:"), "{rendered}");
        // And brake must not pretend it can choose.
        assert!(
            rendered.contains("brake\ncannot see that") || rendered.contains("cannot see that"),
            "{rendered}"
        );
        assert!(
            rendered.contains("brake explain response-field-removed"),
            "{rendered}"
        );
    }

    #[test]
    fn names_the_contract_a_finding_came_from() {
        let rendered = render(&report_with_source());
        assert!(rendered.contains("payments"), "{rendered}");
    }

    #[test]
    fn renders_a_finding_with_no_source_text_available() {
        let report = Report::new(
            vec![finding(
                "endpoint-removed",
                Severity::Error,
                Some(Span::new(
                    "git:origin/main:api/openapi.yaml",
                    42,
                    9,
                    "/paths",
                )),
            )],
            Vec::new(),
            1,
        );
        let rendered = render(&report);
        assert!(
            rendered.contains("git:origin/main:api/openapi.yaml:42:9"),
            "{rendered}"
        );
    }

    #[test]
    fn distinguishes_a_tool_failure_from_an_api_break() {
        let report = Report::new(
            Vec::new(),
            vec![Unavailable {
                contract: Some("ledger".to_owned()),
                message: "baseline file missing".to_owned(),
            }],
            1,
        );
        let rendered = render(&report);
        assert!(rendered.contains("could not check `ledger`"), "{rendered}");
        assert!(rendered.contains("tool failure"), "{rendered}");
    }

    #[test]
    fn reports_how_much_was_checked_when_clean() {
        let rendered = render(&Report::new(Vec::new(), Vec::new(), 3));
        assert!(rendered.contains("3 contracts checked"), "{rendered}");
    }

    #[test]
    fn output_is_plain_text_with_no_terminal_escapes() {
        let rendered = render(&report_with_source());
        assert!(
            !rendered.contains('\u{1b}'),
            "piped output must not carry terminal escape sequences:\n{rendered:?}"
        );
    }

    #[test]
    fn rendering_is_byte_stable() {
        let report = report_with_source();
        assert_eq!(render(&report), render(&report));
    }
}

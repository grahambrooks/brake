//! The catalogue of API evolution strategies.
//!
//! A finding tells a developer they have broken something. That is only half
//! the job: there is nearly always a way to make the same change safely, and
//! the reason people ship breaks is usually that the safe path did not occur
//! to them at the moment they were blocked.
//!
//! **These are catalogued, not generated.** A strategy is a named technique
//! with fixed text, bound to the specific field or endpoint that triggered the
//! finding. Nothing here is invented per call, which is what lets brake stand
//! behind the wording and lets a test assert it.
//!
//! **brake does not pick for you.** Which strategy fits depends on whether you
//! control every consumer, whether you have a version scheme, and how long you
//! can carry two shapes at once — none of which brake can see. It names the
//! applicable options and says what each costs. See
//! `design/02-contract-gates.md` §5.7.

/// A named technique for evolving an API without breaking its consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strategy {
    pub id: &'static str,
    /// What to do, with `{subject}` and `{endpoint}` bound at render time.
    pub summary: &'static str,
    /// What it costs. The reason you would not always pick this one — without
    /// it a list of options is a list of things that all look free.
    pub cost: &'static str,
}

pub static STRATEGIES: &[Strategy] = &[
    Strategy {
        id: "deprecate-then-remove",
        summary: "mark {subject} deprecated now and remove it in a later release, once \
consumers have had a version to migrate",
        cost: "the removal waits for a deprecation window you have to actually observe",
    },
    Strategy {
        id: "expand-then-contract",
        summary: "add the replacement alongside {subject}, move readers across, and remove \
{subject} only when nothing reads it",
        cost: "both shapes are live at once, and the second half is easy to forget",
    },
    Strategy {
        id: "version-the-endpoint",
        summary: "serve the change at a new path, media type or version header, leaving \
{endpoint} answering as it does today",
        cost: "two implementations to maintain until the old one is retired",
    },
    Strategy {
        id: "optional-with-default",
        summary: "keep {subject} optional and give it a default, then require it in a later \
release once callers send it",
        cost: "the default has to be a value that is correct for existing callers",
    },
    Strategy {
        id: "keep-accepting",
        summary: "go on accepting {subject} and ignore it, rather than rejecting requests \
that still send it",
        cost: "the input surface keeps a field nothing uses, until you deprecate it properly",
    },
    Strategy {
        id: "widen-dont-narrow",
        summary: "accept both the old and the new form of {subject} and normalise them \
inside the handler",
        cost: "the handler carries the union until the old form is retired",
    },
    Strategy {
        id: "keep-emitting",
        summary: "go on producing {subject} alongside whatever replaces it, so existing \
readers keep working",
        cost: "the response carries a field you have stopped using",
    },
    Strategy {
        id: "accept-both-locations",
        summary: "read {subject} from both the old and the new location for a release, \
preferring the new one",
        cost: "two places to look, and a rule for what happens when both are sent",
    },
    Strategy {
        id: "dual-accept-credentials",
        summary: "accept the existing credential alongside the new one until consumers \
have issued themselves new ones",
        cost: "the weaker credential stays valid for the length of the transition",
    },
    Strategy {
        id: "document-open-enum",
        summary: "document {subject} as an open set so consumers parse unknown values \
instead of matching exhaustively, and add the value once they do",
        cost: "consumers have to ship the tolerant reader before you ship the value",
    },
    Strategy {
        id: "keep-the-name",
        summary: "leave {subject} named as it is — the rename buys nothing on the wire and \
costs every generated client a code change",
        cost: "you live with a name you no longer like",
    },
    Strategy {
        id: "reserve-the-number",
        summary: "restore {subject} to its original field number and add `reserved` for any \
number you are retiring, so it can never be reused",
        cost: "none — this is the only correct move; a reused number silently misreads data",
    },
    Strategy {
        id: "add-sunset-date",
        summary: "give {subject} an `x-sunset` date, and announce it, so the eventual \
removal is something consumers were told about",
        cost: "you are committing to a date",
    },
    Strategy {
        id: "confirm-intended",
        summary: "confirm this was deliberate — if it was, record why in a `[[contract.allow]]` \
entry so the next reviewer does not have to work it out",
        cost: "none, but it is a decision someone has to actually make",
    },
    Strategy {
        id: "major-version",
        summary: "if the break is genuinely intended, ship it as a new major version and \
tell consumers, rather than moving {endpoint} underneath them",
        cost: "a major version is a migration you are asking every consumer to do",
    },
];

#[must_use]
pub fn lookup(id: &str) -> Option<&'static Strategy> {
    STRATEGIES.iter().find(|strategy| strategy.id == id)
}

/// A strategy with its placeholders filled in from the finding that raised it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remediation {
    pub strategy: &'static str,
    /// The `summary`, with `{subject}` and `{endpoint}` bound.
    pub summary: String,
    pub cost: &'static str,
}

/// Bind a strategy to a specific subject and endpoint.
///
/// `subject` is the field, parameter or status the finding is about.
/// Where it is unknown the wording falls back to a pronoun rather than
/// printing an empty pair of backticks.
#[must_use]
pub fn bind(
    strategy: &'static Strategy,
    subject: Option<&str>,
    endpoint: Option<&str>,
) -> Remediation {
    let subject = subject.map_or_else(|| "it".to_owned(), |name| format!("`{name}`"));
    let endpoint = endpoint.map_or_else(|| "the endpoint".to_owned(), |name| format!("`{name}`"));
    Remediation {
        strategy: strategy.id,
        summary: strategy
            .summary
            .replace("{subject}", &subject)
            .replace("{endpoint}", &endpoint),
        cost: strategy.cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_strategy_is_named_and_costed() {
        for strategy in STRATEGIES {
            assert!(
                strategy
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-'),
                "strategy id is not kebab-case: {}",
                strategy.id
            );
            assert!(
                strategy.summary.len() > 40,
                "{}: summary is too thin to act on",
                strategy.id
            );
            // A list of options with no costs is a list of things that all
            // look free, which is not a decision anyone can make.
            assert!(strategy.cost.len() > 10, "{}: no stated cost", strategy.id);
        }
    }

    #[test]
    fn strategy_ids_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for strategy in STRATEGIES {
            assert!(
                seen.insert(strategy.id),
                "duplicate strategy `{}`",
                strategy.id
            );
        }
    }

    #[test]
    fn a_strategy_that_is_the_only_answer_says_so_in_its_cost() {
        // `reserve-the-number` is the sole remedy for a renumbered field.
        // Its cost line has to read as "there is no trade here" rather than
        // leaving a reader looking for the catch.
        let only = lookup("reserve-the-number").expect("known strategy");
        assert!(only.cost.starts_with("none"), "{}", only.cost);
    }

    #[test]
    fn binding_fills_the_placeholders() {
        let strategy = lookup("deprecate-then-remove").expect("known strategy");
        let bound = bind(strategy, Some("customer_id"), Some("GET /payments/{id}"));

        assert!(bound.summary.contains("`customer_id`"), "{}", bound.summary);
        assert!(!bound.summary.contains("{subject}"), "{}", bound.summary);
    }

    #[test]
    fn binding_without_a_subject_reads_as_a_sentence() {
        let strategy = lookup("deprecate-then-remove").expect("known strategy");
        let bound = bind(strategy, None, None);

        assert!(
            bound.summary.contains("mark it deprecated"),
            "{}",
            bound.summary
        );
        assert!(
            !bound.summary.contains("``"),
            "an unknown subject must not render as empty backticks: {}",
            bound.summary
        );
    }

    #[test]
    fn an_endpoint_placeholder_survives_a_path_template() {
        // The path itself contains braces; binding must not confuse them for
        // placeholders.
        let strategy = lookup("version-the-endpoint").expect("known strategy");
        let bound = bind(strategy, None, Some("GET /payments/{id}"));
        assert!(
            bound.summary.contains("`GET /payments/{id}`"),
            "{}",
            bound.summary
        );
    }
}

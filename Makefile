# Convenience targets. `make check` is the gate: the same commands as CI's
# lint and test jobs. It must pass before every commit, and the prek pre-push
# hook (`make hooks`) runs it before every push.
#
# Releases come from release.mk (release-kit): `make release` tags origin/main
# with the next CalVer, vYYYY.M.D, and pushes only the tag. The release
# workflow builds and publishes, and its PR bumps Cargo.toml and the formula.

SHELL := /usr/bin/env bash
.SHELLFLAGS := -eu -o pipefail -c
.DEFAULT_GOAL := help

# release, release-dry, release-version
include release.mk

.PHONY: help
help: ## List targets
	@awk 'BEGIN {FS = ":.*?## "} /^[a-zA-Z_-]+:.*?## / {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

.PHONY: check
check: fmt-check lint test ## fmt, clippy -D warnings, tests — the gate, as CI's lint and test jobs

.PHONY: build
build: ## Debug build
	cargo build

.PHONY: test
test: ## Tests, as CI (all targets, then doctests)
	cargo test --all-features --all-targets --locked
	cargo test --all-features --doc --locked

.PHONY: lint
lint: ## Clippy, warnings denied (as CI)
	cargo clippy --all-targets --all-features --locked -- -D warnings

.PHONY: eval-tests
eval-tests: ## User-guide evaluation test matrix
	cargo test --all-features --test user_guide_cases

.PHONY: docs
docs: ## Regenerate docs/rules.md from the catalogue
	BRAKE_BLESS=1 cargo test --all-features --test docs_match_the_catalogue
	BRAKE_BLESS=1 cargo test --all-features --test surface_parity the_committed_capabilities_document

# AGENTS.md and .junie/skills/ are copies. Claude Code reads CLAUDE.md and
# .claude/skills/, other agents read AGENTS.md, Junie reads .junie/skills/ —
# and none of them follows a pointer between the three reliably, so the copies
# are real files kept in step by a test rather than by a symlink.
.PHONY: agents
agents: ## Regenerate AGENTS.md and .junie/skills from CLAUDE.md
	BRAKE_BLESS=1 cargo test --all-features --test agent_guidance_is_consistent

# brake, applied to itself. The repository carries a fixture contract so the
# tool gates its own API surface the way it asks consumers to gate theirs.
.PHONY: self-check
self-check: ## Run brake against its own fixture contract
	cargo run --quiet -- check --format text api/payments-openapi.yaml

.PHONY: fmt
fmt: ## Format all Rust sources
	cargo fmt --all

.PHONY: fmt-check
fmt-check: ## Verify formatting
	cargo fmt --all --check

.PHONY: hooks
hooks: ## Install the git hooks (pre-commit and pre-push) from prek.toml
	prek install

.PHONY: watch-ci
watch-ci: ## After pushing, wait for every CI run on HEAD
	@command -v watch-ci >/dev/null || { echo "watch-ci not on PATH: ln -s ~/dev/projects/agentic-twin/scripts/watch-ci.sh ~/.local/bin/watch-ci" >&2; exit 1; }
	watch-ci

# Convenience targets. `make check` is the pre-commit gate and must pass before
# every commit.
#
# CalVer is YYYY.M.MICRO, where MICRO counts the releases already cut this
# month. The month has no leading zero: 2026.08.1 is not valid SemVer, and
# cargo requires SemVer. The version is committed in Cargo.toml rather than
# injected at release time, because that is what `brake --version`, every
# installer, and crates.io all read.

SHELL := /usr/bin/env bash
.SHELLFLAGS := -eu -o pipefail -c
.DEFAULT_GOAL := help

YEAR    := $(shell date -u +%Y)
MONTH   := $(shell date -u +%-m)
# One past the highest MICRO already tagged this month, not the *number* of
# tags. Counting assumes no tag is ever deleted or skipped: delete one and the
# next release silently reuses a version that is already on crates.io, where
# the only thing standing between that and a bad publish is release-guard
# failing with a message about a tag that exists for reasons nobody remembers.
MICRO    = $(shell last=$$(git tag -l 'v$(YEAR).$(MONTH).*' \
             | sed -n 's/^v[0-9][0-9]*\.[0-9][0-9]*\.\([0-9][0-9]*\)$$/\1/p' \
             | sort -n | tail -1); echo $$(( $${last:--1} + 1 )))
VERSION  = $(YEAR).$(MONTH).$(MICRO)
TAG      = v$(VERSION)

.PHONY: help
help:
	@echo 'make release      cut $(TAG) — bump, commit, tag, push'
	@echo 'make check        fmt, clippy, tests — the pre-commit gate'
	@echo 'make build        debug build'
	@echo 'make test         tests only'
	@echo 'make eval-tests   user-guide evaluation test matrix'
	@echo 'make docs         regenerate docs/rules.md from the catalogue'
	@echo 'make self-check   run brake against its own fixture contract'
	@echo 'make version      print the next version'
	@echo 'make release-dry  show what a release would do, without doing it'

.PHONY: check
check:
	cargo fmt --all --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test --all-features

.PHONY: build
build:
	cargo build

.PHONY: test
test:
	cargo test --all-features

.PHONY: eval-tests
eval-tests:
	cargo test --all-features --test user_guide_cases

.PHONY: docs
docs:
	BRAKE_BLESS=1 cargo test --all-features --test docs_match_the_catalogue

# brake, applied to itself. The repository carries a fixture contract so the
# tool gates its own API surface the way it asks consumers to gate theirs.
.PHONY: self-check
self-check:
	cargo run --quiet -- check --format text api/payments-openapi.yaml

.PHONY: fmt
fmt:
	cargo fmt --all

.PHONY: version
version:
	@echo '$(VERSION)'

.PHONY: release-dry
release-dry:
	@echo 'would set version to $(VERSION) and tag $(TAG)'
	@echo 'crates.io publishing stays manual — it is the irreversible step'
	@echo "currently: $$(grep -m1 '^version = ' Cargo.toml)"

.PHONY: release-guard
release-guard:
	@test "$$(git rev-parse --abbrev-ref HEAD)" = main \
		|| { echo 'release: not on main'; exit 1; }
	@test -z "$$(git status --porcelain)" \
		|| { echo 'release: working tree is dirty'; exit 1; }
	@git fetch --quiet origin main
	@test "$$(git rev-parse HEAD)" = "$$(git rev-parse origin/main)" \
		|| { echo 'release: local main differs from origin/main'; exit 1; }
	@! git rev-parse -q --verify 'refs/tags/$(TAG)' >/dev/null \
		|| { echo 'release: tag $(TAG) already exists'; exit 1; }

.PHONY: release
release: release-guard check
	@echo '--> releasing $(VERSION)'
	perl -pi -e 's/^version = "[^"]*"$$/version = "$(VERSION)"/ if $$. < 20' Cargo.toml
	@grep -q '^version = "$(VERSION)"$$' Cargo.toml \
		|| { echo 'release: failed to set the version'; exit 1; }
	cargo update --workspace --offline
	git add Cargo.toml Cargo.lock
	@git diff --cached --quiet \
		&& echo 'version already at $(VERSION); tagging existing commit' \
		|| git commit --quiet -m 'Release $(VERSION)'
	git tag -a '$(TAG)' -m 'brake $(VERSION)'
	git push --quiet origin main
	git push --quiet origin '$(TAG)'
	@echo 'pushed $(TAG)'

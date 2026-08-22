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
MICRO    = $(shell git tag -l 'v$(YEAR).$(MONTH).*' | wc -l | tr -d ' ')
VERSION  = $(YEAR).$(MONTH).$(MICRO)
TAG      = v$(VERSION)

.PHONY: help
help:
	@echo 'make check        fmt, clippy, tests — the pre-commit gate'
	@echo 'make build        debug build'
	@echo 'make test         tests only'
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

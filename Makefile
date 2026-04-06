SHELL := /bin/bash

.PHONY: help fmt fmt-check lint test compliance licenses verify verify-repo-state use-local-codex reset-local-codex release-checks bump-version release-notes release-prep release-precheck

help:
	@echo "Available targets:"
	@echo "  make fmt"
	@echo "  make fmt-check"
	@echo "  make lint"
	@echo "  make test"
	@echo "  make compliance"
	@echo "  make licenses"
	@echo "  make verify"
	@echo "  make verify-repo-state"
	@echo "  make use-local-codex"
	@echo "  make reset-local-codex"
	@echo "  make release-checks"
	@echo "  make bump-version VERSION=X.Y.Z [DATE=YYYY-MM-DD]"
	@echo "  make release-notes VERSION=X.Y.Z"
	@echo "  make release-precheck VERSION=X.Y.Z"
	@echo "  make release-prep VERSION=X.Y.Z [DATE=YYYY-MM-DD]"

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all --check

lint:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test -q

verify-repo-state:
	bash scripts/check-no-local-codex-override.sh

compliance:
	cargo deny check

licenses:
	cargo about -L off generate --output-file THIRD_PARTY_LICENSES.md about.hbs

verify: verify-repo-state fmt-check lint test

use-local-codex:
	bash scripts/use-local-codex.sh

reset-local-codex:
	bash scripts/reset-local-codex.sh

release-checks:
	bash scripts/check-release-tools.sh
	$(MAKE) fmt-check
	$(MAKE) lint
	$(MAKE) test
	$(MAKE) compliance
	$(MAKE) licenses

bump-version:
	@test -n "$(VERSION)" || (echo "VERSION is required, e.g. make bump-version VERSION=0.1.1" >&2; exit 2)
	bash scripts/bump-version.sh "$(VERSION)" "$(or $(DATE),$$(date +%F))"

release-notes:
	@test -n "$(VERSION)" || (echo "VERSION is required, e.g. make release-notes VERSION=0.1.1" >&2; exit 2)
	mkdir -p dist
	bash scripts/render-release-notes.sh "v$(VERSION)" > dist/RELEASE_NOTES.md

release-precheck:
	@test -n "$(VERSION)" || (echo "VERSION is required, e.g. make release-precheck VERSION=0.1.1" >&2; exit 2)
	$(MAKE) release-checks
	bash scripts/check-release-metadata.sh "v$(VERSION)"
	$(MAKE) release-notes VERSION="$(VERSION)"

release-prep:
	@test -n "$(VERSION)" || (echo "VERSION is required, e.g. make release-prep VERSION=0.1.1" >&2; exit 2)
	$(MAKE) release-checks
	$(MAKE) bump-version VERSION="$(VERSION)" DATE="$(DATE)"
	bash scripts/check-release-metadata.sh "v$(VERSION)"
	$(MAKE) release-notes VERSION="$(VERSION)"

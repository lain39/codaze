# Contributing

## Scope

- `Codaze` is a local, Codex-oriented gateway.
- Do not broaden it into a general multi-provider proxy unless explicitly requested.
- Keep outbound behavior as close as practical to the Codex Rust client stack.

## Before Opening A PR

Run:

```bash
make verify
```

`make verify` is a pure check command; it does not rewrite files.
Its repo-state guard only blocks the managed local Codex override, not arbitrary custom Cargo config.

Equivalent expanded commands:

```bash
bash scripts/check-no-local-codex-override.sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test -q
```

If you temporarily enabled the local `local/codex` override for debugging, run this before opening a PR:

```bash
bash scripts/reset-local-codex.sh
```

If your change touches release/compliance files, or if you want to reproduce the release pipeline locally, also run when available:

```bash
make compliance
make licenses
```

Equivalent expanded commands:

```bash
cargo deny check
cargo about -L off generate --output-file THIRD_PARTY_LICENSES.md about.hbs
```

These release/compliance commands are optional for ordinary feature development. They are not required for day-to-day coding unless you are validating release artifacts or compliance changes.

If you are preparing a tagged release, also run:

```bash
make release-precheck VERSION=X.Y.Z
make release-prep VERSION=X.Y.Z
```

If you want the expanded manual flow instead of the wrapper, use:

```bash
bash scripts/bump-version.sh X.Y.Z
bash scripts/check-release-metadata.sh vX.Y.Z
bash scripts/render-release-notes.sh vX.Y.Z
```

## Documentation Rules

- Do not commit machine-local absolute paths such as `/Users/...`.
- Use repository-relative links in Markdown.
- Keep examples portable across machines unless a machine-specific detail is the point.

## Design Constraints

- Reuse Codex Rust crates and request builders where practical.
- Do not invent fingerprint fields that real Codex does not stably send.
- Keep account runtime state in memory unless a change explicitly requires persistence.
- Prefer explicit, testable error classification over transport-layer guesswork.

## Pull Request Notes

Please include:

- what changed
- why it changed
- how you validated it
- any known follow-up work

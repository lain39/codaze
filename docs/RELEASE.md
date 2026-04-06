[English](RELEASE.md) | [简体中文](RELEASE.zh-CN.md)

# Codaze Release Guide

- Repository: https://github.com/lain39/codaze
- GitHub Releases: https://github.com/lain39/codaze/releases

## Release Checklist

1. Move the pending notes into the `Unreleased` section of [../CHANGELOG.md](../CHANGELOG.md).
2. Bump the crate version and cut the current release section out of `Unreleased`:

```bash
bash scripts/bump-version.sh X.Y.Z
```

Or use the wrapper:

```bash
make release-prep VERSION=X.Y.Z
```

For a dry-run prerelease tag that should still use the `X.Y.Z` changelog section, `vX.Y.Z-rcN` is also accepted by the metadata and release-notes scripts.

3. Confirm the pinned Codex Git revision in [../Cargo.toml](../Cargo.toml) is still the intended one.
4. Run the release checks:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test -q
cargo deny check
cargo about -L off generate --output-file THIRD_PARTY_LICENSES.md about.hbs
bash scripts/check-release-metadata.sh vX.Y.Z
bash scripts/render-release-notes.sh vX.Y.Z > dist/RELEASE_NOTES.md
```

If you only want the same release prechecks without mutating `Cargo.toml` or `CHANGELOG.md`:

```bash
make release-precheck VERSION=X.Y.Z
```

5. Build a local release binary if needed:

```bash
mkdir -p dist
cargo build --release
```

6. Create and push the tag:

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

7. Make sure the GitHub Release artifacts include at least:

- the `codaze` archive
- `LICENSE`
- `NOTICE`
- `THIRD_PARTY_LICENSES.md`
- release notes rendered from `CHANGELOG.md`

## Release Notes Template

```md
# Codaze X.Y.Z

### Added

- ...

### Changed

- ...

### Fixed

- ...
```

## Notes

- `scripts/bump-version.sh` updates `Cargo.toml` and cuts the current `Unreleased` section into a dated release section.
- It resets `Unreleased` back to the standard empty template.
- The script is intentionally conservative and refuses to overwrite an existing version section.
- `scripts/check-release-metadata.sh` and `scripts/render-release-notes.sh` accept both `vX.Y.Z` and prerelease tags such as `vX.Y.Z-rc1`; prerelease tags still map to the `## X.Y.Z - YYYY-MM-DD` changelog section unless `Cargo.toml` itself already carries the prerelease version.
- `make release-precheck` does not mutate `Cargo.toml` or `CHANGELOG.md`; it only generates transient artifacts such as `THIRD_PARTY_LICENSES.md` and `dist/RELEASE_NOTES.md`.
- `make release-prep` runs the tooling/build checks first, then mutates `Cargo.toml` and `CHANGELOG.md`, then runs release metadata validation and release-note rendering.
- `make verify` is the everyday development check and stays separate from release/compliance tooling.
- In the release matrix, each platform renders its own `THIRD_PARTY_LICENSES.md` with `cargo about --target <matrix-target>` before packaging.
- GitHub Release publishing is allowed to proceed with whatever platform archives were built successfully; a failed matrix target no longer blocks already-built archives from being attached to the release.
- The current Linux release targets are `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`; user-facing docs should describe them as binaries for reasonably recent `glibc`-based distributions, not as universal Linux packages.

# Changelog

All notable changes to `Codaze` should be recorded in this file.

The format is intentionally simple and human-maintained.

## Unreleased

### Added

- Nothing yet

### Changed

- Nothing yet

### Fixed

- Nothing yet

### Removed

- Nothing yet

## 0.1.0 - 2026-04-06

### Added

- Initial Codex-oriented local gateway surface for `chatgpt.com/backend-api/codex`
- Multi-account refresh-token pool, routing, block/wake management, and lazy recovery
- Public/admin split listeners with loopback-only defaults
- Codex-aligned HTTP/SSE/websocket forwarding built on the Codex Rust transport stack
- GitHub Actions CI and release workflows
- Apache-2.0 licensing, NOTICE attribution, and third-party license compliance config

### Changed

- Repository defaults renamed from `foo` runtime naming to `codaze`

### Fixed

- Auth-invalid refresh tokens now tombstone immediately in memory even if `trash/` persistence fails
- Account rescan no longer performs lock-held filesystem existence probing on the hot path

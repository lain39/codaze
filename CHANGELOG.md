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

## 0.2.1 - 2026-04-07

### Added

- Documentation for non-Codex `/v1/responses` compatibility normalization rules

### Changed

- Non-Codex `/v1/responses` now keeps compatibility normalization active even in `passthrough` fingerprint mode

### Fixed

- String-form `tool_choice` web-search aliases now normalize to the upstream-accepted object form

## 0.2.0 - 2026-04-07

### Added

- OpenAI-compatible `/v1/models` shape for non-Codex callers while keeping Codex `{"models":[...]}`
- Non-Codex `/v1/responses` pre-stream failures now return ordinary HTTP JSON errors instead of Codex-only synthetic SSE
- Request normalization now fills missing or `null` `instructions` with `""`

### Changed

- `/v1/models` now refreshes and caches the upstream Codex models snapshot, and uses that snapshot to derive `parallel_tool_calls`
- Streaming `/v1/responses` requests no longer inherit the 600-second total request timeout used for unary HTTP and refresh requests

### Fixed

- Account directories and persisted refresh-token files now tighten permissions instead of relying on process `umask`

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

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

## 0.3.0 - 2026-04-16

### Added

- OpenAI-oriented response shaping for non-Codex callers across `/v1/models`, `/v1/responses`, and `/v1/responses/compact`, including route-specific tests and expanded API/design documentation
- Compatibility normalization for non-Codex request bodies on `/v1/responses` and `/v1/responses/compact`, including top-level string `input` support and legacy web-search alias rewriting on the `/v1/responses` path
- `thread_source: "user"` normalization in Codex turn metadata for normalized request headers and websocket `response.create.client_metadata`

### Changed

- Default Codex fingerprint version updated to `0.121.0`
- Codex Rust dependencies updated to `224dad41ac1bdf0c8a848b1fd0068262f1f99223`
- Public non-Codex HTTP responses now preserve only `Content-Type` and `Cache-Control` while keeping Codex callers on the existing Codex-shaped surface
- Public routing failures are now collapsed into gateway-level unavailable semantics and no longer expose upstream `retry-after`, `resets_at`, or `resets_in_seconds`
- Non-Codex `/v1/responses/compact` responses now rewrite `type: "compaction_summary"` to `type: "compaction"` before returning downstream

### Fixed

- Responses websocket and SSE failure rendering now better matches the selected caller shape, including OpenAI-style downstream error events for non-Codex streaming callers
- `cargo deny check` is clean again after updating `rustls-webpki` to `0.103.12`

## 0.2.2 - 2026-04-13

### Added

- `x-codex-installation-id` fingerprint normalization for `/v1/responses`, `/v1/responses/compact`, and responses websocket `response.create`
- Documentation covering the account-derived installation-id strategy and websocket pre-commit failover replay behavior

### Changed

- Default Codex fingerprint version updated to `0.121.0`
- Codex Rust dependencies updated to `224dad41ac1bdf0c8a848b1fd0068262f1f99223`

### Fixed

- Responses websocket pre-commit failover now rewrites replayed `response.create` messages with the replacement upstream connection's installation id

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

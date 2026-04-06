[English](README.md) | [简体中文](README.zh-CN.md)

<div align="center">
  <h1>Codaze☆</h1>
  <p><strong>Aggregate multiple ChatGPT accounts into a local gateway that stays as close as practical to the official Codex client.</strong></p>
</div>

<p align="center">
  <a href="https://github.com/lain39/codaze/releases"><img src="https://img.shields.io/github/v/release/lain39/codaze?style=flat-square&label=release&sort=semver" alt="Release" /></a>
  <a href="https://github.com/lain39/codaze/actions/workflows/release.yml"><img src="https://img.shields.io/github/actions/workflow/status/lain39/codaze/release.yml?style=flat-square&label=release%20pipeline" alt="Release Pipeline" /></a>
  <a href="https://github.com/lain39/codaze/blob/main/LICENSE"><img src="https://img.shields.io/github/license/lain39/codaze?style=flat-square" alt="License" /></a>
  <a href="https://github.com/lain39/codaze/releases"><img src="https://img.shields.io/badge/support-linux%20%7C%20macOS%20%7C%20windows%20%C2%B7%20amd64%20%7C%20arm64-0A7EA4?style=flat-square" alt="Support Matrix" /></a>
  <a href="docs/DESIGN.md"><img src="https://img.shields.io/badge/docs-DESIGN.md-1F6FEB?style=flat-square" alt="Design Docs" /></a>
</p>

Docs index: [docs/README.md](docs/README.md)  

---

## Why Codaze

- **High-fidelity fingerprint alignment**: Reuses Codex's native Rust transport stack so outbound network behavior, HTTP headers, and websocket characteristics stay as close as practical to the official client, while still keeping the gateway intentionally lightweight.
- **Failover and protocol surgery**: Switches accounts on quota or selected failure paths, and rewrites a few critical protocol-level errors when needed, such as converting `previous_response_not_found` into a client-reset-triggering error to prompt the upstream client to gracefully reset.
- **Lazy initialization**: Accounts refresh on demand instead of during startup, which avoids noisy bulk refreshes.
- **Zero-bloat local isolation**: No YAML-heavy control plane; the account directory acts as the durable source of truth, while public and admin APIs are physically separated onto different loopback ports.

## Quick Start

Download the platform-specific `codaze` binary from [GitHub Releases](https://github.com/lain39/codaze/releases), then run it directly:

```bash
./codaze
```

> [!NOTE]
> The published Linux binaries are `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, intended for reasonably recent `glibc`-based distributions.
> They are not guaranteed to run on every Linux distribution; `musl` environments such as Alpine should build from source.

That starts with the defaults:

- public listener: `127.0.0.1:18039`
- admin listener: `127.0.0.1:18040`
- accounts dir: `$HOME/.codaze` on Unix-like systems, `%USERPROFILE%\\.codaze` on Windows
- Codex version: `0.118.0` for UA and related client-version fingerprinting
- routing policy: `least_in_flight`
- fingerprint mode: `normalize`

If you want to override defaults, pass flags explicitly:

```bash
./codaze \
  --listen 127.0.0.1:18039 \
  --admin-listen 127.0.0.1:18040 \
  --codex-version 0.118.0 \
  --routing-policy least_in_flight \
  --fingerprint-mode normalize
```

> [!NOTE]
> The gateway is loopback-only and refuses to bind to non-local addresses.
> Public traffic and admin traffic use different local ports by default.

The process shuts down gracefully on `SIGINT` and `SIGTERM`.

If you are maintaining the project and want local builds or `cargo run`, see [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

## Run As A Background Service

If you want `codaze` to keep running in the background, use the provided service templates instead of keeping a shell session open:

- Linux systemd: [docs/PACKAGING.md](docs/PACKAGING.md)
- macOS launchd: [docs/PACKAGING.md](docs/PACKAGING.md)

The actual template files live under:

- [`packaging/systemd/codaze.service`](packaging/systemd/codaze.service)
- [`packaging/launchd/io.github.lain39.codaze.plist`](packaging/launchd/io.github.lain39.codaze.plist)

## Import Accounts

The simplest path is to drop account files directly into `--accounts-dir`. Codaze rescans the directory periodically, so new files are picked up without restarting the service.

Minimal account file example:

```json
{
  "refresh_token": "rt_xxx"
}
```

`label` and `email` are optional metadata. If omitted, Codaze can fill them later from refresh results.

Runtime behavior:

- refresh is lazy; importing does not immediately refresh the account, it refreshes only when the account is actually routed
- duplicate imports are deduplicated by the current refresh token
- invalid refresh tokens are moved into `trash/`
- the in-memory `refresh_token` is authoritative at runtime; manually editing `refresh_token` in an already loaded file is not supported
- there is no separate shutdown-time runtime-state flush; account-file mutations are persisted immediately on import, successful refresh, delete, and trash transitions, while `blocked_*` and `last_error` remain memory-only runtime state

If you want to import accounts dynamically while the process is already running, use HTTP:

```bash
curl -X POST http://127.0.0.1:18040/admin/accounts/import \
  -H 'Content-Type: application/json' \
  -d '{"refresh_token":"rt_xxx","label":"main","email":"user@example.com"}'
```

## Proxy Environment Variables

Codaze's outbound client follows the standard process-level proxy environment variables unless it is running in the special `CODEX_SANDBOX=seatbelt` path, where proxy autodetection is explicitly disabled.

Typical forms:

```bash
export HTTP_PROXY="http://127.0.0.1:3128"
export HTTPS_PROXY="http://127.0.0.1:3128"
export ALL_PROXY="socks5h://127.0.0.1:1080"
export NO_PROXY="127.0.0.1,localhost"
```

Notes:

- for ordinary HTTP(S) upstream proxies, use `HTTP_PROXY` / `HTTPS_PROXY`
- for SOCKS5 with remote DNS resolution, `ALL_PROXY=socks5h://host:port` is the safest form
- if you run Codaze via systemd or launchd, set the same environment variables in the service definition

## API Overview

| Module | Method | Path | Description |
| :--- | :--- | :--- | :--- |
| **Public API (18039)** | `GET` | `/v1/models` | List available models |
| | `POST` | `/v1/responses` | Responses over SSE |
| | `GET` | `/v1/responses` | Responses over websocket |
| | `POST` | `/v1/responses/compact` | Compact endpoint |
| | `POST` | `/v1/memories/trace_summarize` | Present for compatibility; upstream is not available yet |
| | `GET` | `/health` | Basic gateway health check |
| **Admin API (18040)** | `GET` | `/admin/accounts` | Inspect account-pool state |
| | `POST` | `/admin/accounts/import` | Import or update an account |
| | `POST` | `/admin/accounts/wake` | Wake all blocked accounts |
| | `POST` | `/admin/accounts/:id/wake` | Wake a specific blocked account |
| | `DELETE` | `/admin/accounts/:id` | Remove an account |
| | `GET/PUT` | `/admin/routing/policy` | Read or change routing policy |

> For full request and response shapes, see [docs/API.md](docs/API.md).

## Codex Compatibility

Codaze intentionally reuses Codex's Rust transport stack instead of inventing a separate outbound client:

- `codex_login::default_client::build_reqwest_client`
- `codex_client::ReqwestTransport`
- Codex-style request headers and endpoint paths

The default fingerprint mode is `normalize`. It only injects fields that real Codex requests stably carry, such as:

- `store: false` on `/v1/responses`
- `instructions: ""` when the caller omits `instructions` or sends `null`
- model-derived `parallel_tool_calls`
- identity headers derivable from `x-codex-session-source`, such as `x-openai-subagent` and `x-codex-parent-thread-id`

`passthrough` only affects outbound request fingerprint shaping. It does not disable routing, refresh, error classification, or local admin behavior.

Important boundary:

- if the caller already provides `x-codex-window-id`, Codaze forwards it
- for non-Codex callers, Codaze does not synthesize `x-codex-window-id` or websocket `response.create.client_metadata` identity keys
- `GET /v1/models` returns Codex `{"models":[...]}` only when `originator` identifies a Codex client; other callers receive OpenAI-style `{"object":"list","data":[...]}` metadata
- `/v1/responses` pre-stream failures stay as synthetic SSE only for Codex callers; non-Codex callers receive ordinary HTTP JSON errors instead

For the full design rationale, see [docs/DESIGN.md](docs/DESIGN.md).

## Optional `_gateway` Control Field

`POST /v1/responses`, `POST /v1/responses/compact`, and `POST /v1/memories/trace_summarize` accept a private `_gateway` object in the JSON body. It is consumed locally and stripped before forwarding.

Example:

```json
{
  "_gateway": {
    "session_source": "exec"
  }
}
```

`_gateway.session_source` must match the JSON representation of `codex_protocol::SessionSource`.

## Point Codex at Codaze

Using the built-in `openai` provider:

`OPENAI_API_KEY=dummy` is not used for upstream calls here; it only satisfies Codex's provider-side config validation.

```bash
OPENAI_API_KEY=dummy \
codex \
  -c 'openai_base_url="http://127.0.0.1:18039/v1"' \
  -c 'model_provider="openai"'
```

Using a custom provider:

```toml
model_provider = "cpa"
model = "gpt-5.4"

[model_providers.cpa]
name = "My Proxy API"
base_url = "http://127.0.0.1:18039/v1"
env_key = "OPENAI_API_KEY"
wire_api = "responses"
supports_websockets = true
```

If you omit `supports_websockets = true` on a custom provider entry, Codex falls back to HTTP/SSE.

Admin APIs stay on `http://127.0.0.1:18040` by default and are never exposed through the public `/v1` listener.

## License And Docs

- License: `Apache-2.0`
- Attribution: [`NOTICE`](NOTICE)
- Development and local builds: [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)
- Release process: [docs/RELEASE.md](docs/RELEASE.md)
- Service templates: [docs/PACKAGING.md](docs/PACKAGING.md)
- GitHub Releases: https://github.com/lain39/codaze/releases

Project maintenance docs:

- [`CHANGELOG.md`](CHANGELOG.md)
- [`CONTRIBUTING.md`](CONTRIBUTING.md)
- [`SECURITY.md`](SECURITY.md)
- [`docs/README.md`](docs/README.md)
- [`docs/RELEASE.md`](docs/RELEASE.md)

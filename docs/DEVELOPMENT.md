[English](DEVELOPMENT.md) | [简体中文](DEVELOPMENT.zh-CN.md)

# Codaze Development Guide

This document is for maintainers. It does not repeat the product-facing explanation from the README. It focuses on local development, debugging, and extension work.

Design principles: [DESIGN.md](DESIGN.md)  
Operations and troubleshooting: [OPERATIONS.md](OPERATIONS.md)

## 1. Local Environment

Suggested environment:

- stable Rust
- macOS / Linux
- `cargo`
- `curl`
- `jq`

Project root:

```bash
cd <repo-root>
```

## 2. Common Commands

Format:

```bash
cargo fmt --all
```

Build:

```bash
cargo build
cargo build --release
```

Run:

```bash
cargo run --release -- \
  --listen 127.0.0.1:18039 \
  --admin-listen 127.0.0.1:18040 \
  --codex-version 0.118.0 \
  --routing-policy least_in_flight \
  --fingerprint-mode normalize
```

Run tests:

```bash
cargo test -q
```

Run focused tests:

```bash
cargo test wake -q
cargo test responses -q
cargo test classifier -q
```

More logging:

```bash
RUST_LOG=debug cargo run --release -- ...
```

Default logging keeps `codex_client::custom_ca` noise low.

CI / release notes:

- the repo includes [../.github/workflows/ci.yml](../.github/workflows/ci.yml) and [../.github/workflows/release.yml](../.github/workflows/release.yml)
- the default dependency graph is self-contained; CI and release only check out `codaze`
- if the pinned Codex reference changes later, update the fixed Git revisions in [../Cargo.toml](../Cargo.toml)

## 2.1 Optional Local Codex Override

By default, `codaze` pulls the Codex Rust crates from the pinned Git revision in `Cargo.toml`.

If you want to debug against a local Codex checkout instead:

1. clone `openai/codex` into `local/codex`
2. run `bash scripts/use-local-codex.sh`
3. rebuild `codaze`

Example:

```bash
git clone https://github.com/openai/codex.git local/codex
bash scripts/use-local-codex.sh
cargo build
```

Notes:

- `local/codex` is ignored by git and is only for local development
- `.cargo/config.toml` is also local-only and should not be committed
- the patch paths in `.cargo/config.toml` are resolved from the workspace root, so they should point to `local/codex/...`, not `../local/codex/...`
- `scripts/use-local-codex.sh` runs in strict managed mode: if `.cargo/config.toml` already exists, the script refuses to start instead of merging with it
- if you already use a custom `.cargo/config.toml`, move it aside temporarily or use a clean worktree before enabling the local Codex override
- `scripts/use-local-codex.sh` refuses to start if `Cargo.lock` already has local edits, so that reset does not later discard unrelated lockfile work
- while the local override is active, Cargo may rewrite `Cargo.lock` from pinned git sources to local path-resolved packages
- before opening a PR or pushing release-related changes, run `bash scripts/reset-local-codex.sh`
- `scripts/reset-local-codex.sh` removes only the managed override and restores `Cargo.lock` from the local backup only when it detects local Codex override contamination

## 3. Directory Layout

`src/` is split by responsibility:

- `main.rs`
  - process entrypoint
  - logging bootstrap
  - dual listeners
  - graceful shutdown
  - background account-directory rescans
- `config.rs`
  - CLI parsing
  - defaults
  - loopback listener guardrails
- `app/`
  - local HTTP routing entrypoints
  - `api.rs` for business `/v1/*`
  - `admin.rs` for `/admin/*`
- `accounts/`
  - account data model
  - account file I/O
  - candidate selection
  - block / wake / trash / post-refresh transitions
- `upstream/`
  - upstream HTTP / websocket / refresh calls
  - outbound header assembly
  - send / receive behavior
- `responses/`
  - `/v1/responses` logic
  - SSE wrapping
  - pre-stream failure synthesis
  - websocket responses proxying
- `classifier.rs`
  - upstream failure classification
- `failover.rs`
  - intra-request retry and account switching
- `router.rs`
  - routing-policy enum and selection entrypoint
- `request_normalization.rs`
  - fingerprint shaping
  - `store: false`
  - `parallel_tool_calls`
  - private `_gateway` field handling
- `gateway_errors.rs`
  - gateway-level errors exposed downstream
- `error_semantics.rs`
  - error-semantics helpers

## 4. Runtime Model

The service has two local listener groups:

- public: default `127.0.0.1:18039`
- admin: default `127.0.0.1:18040`

Public endpoints:

- `GET /health`
- `GET /v1/models`
- `POST /v1/responses`
- `GET /v1/responses` websocket upgrade
- `POST /v1/responses/compact`
- `POST /v1/memories/trace_summarize`

Admin endpoints:

- `POST /admin/accounts/import`
- `POST /admin/accounts/wake`
- `POST /admin/accounts/{id}/wake`
- `GET /admin/accounts`
- `DELETE /admin/accounts/{id}`
- `GET /admin/routing/policy`
- `PUT /admin/routing/policy`

## 5. Account Data And Persistence

The persistence boundary is intentionally narrow:

- account files are persisted
- runtime state is not written into a separate state store

Actions that persist immediately:

- HTTP account import
- successful refresh updating refresh token or metadata
- account deletion
- moving a confirmed-invalid refresh token into `trash/`

Runtime-only state:

- `blocked_until`
- `blocked_source`
- `blocked_reason`
- `last_error`
- `in_flight_requests`
- hot-path selection state

Do not manually edit `refresh_token` inside an already loaded account file while the process is running. Runtime semantics are "memory wins; disk is for cold start and account-entity persistence."

## 6. Request Flow

Typical `/v1/responses` flow:

1. downstream request enters `app/api.rs`
2. `request_normalization.rs` handles `store: false`, `parallel_tool_calls`, and `_gateway`
3. `failover.rs` selects an account and executes one attempt
4. `upstream/` performs the HTTP or websocket request
5. `responses/` reshapes upstream output into downstream SSE / WS form
6. account runtime state is updated on success or failure

Behaviors worth remembering:

- pre-stream failure is not returned as a raw upstream HTTP error to Codex; it is reshaped into the currently supported client-facing failure surface
- `previous_response_not_found` is intentionally converted into a failure path that triggers downstream reset behavior
- `passthrough` only disables outbound request normalization; it does not bypass routing or account management

## 7. Fingerprint Constraints

The goal is not "look like the OpenAI API." The goal is "look like Codex as much as practical."

Before changing outbound behavior, ask:

- does real Codex send this header?
- does real Codex send this body field?
- is this a stable fingerprint, or an environment-specific detail?

Current practice:

- prefer Codex Rust crates for transport / login / protocol behavior
- do not synthesize `x-codex-window-id`
- do not synthesize identity keys inside websocket `client_metadata`
- only inject stable Codex fields such as `store: false`

If a compatibility tweak exists only to support some unknown third-party caller, and not because it matches Codex, it should not be added by default.

## 8. Debugging Tips

Server logs:

```bash
RUST_LOG=info cargo run --release -- ...
RUST_LOG=debug cargo run --release -- ...
```

Import an account:

```bash
curl -sS http://127.0.0.1:18040/admin/accounts/import \
  -H 'Content-Type: application/json' \
  -d '{"refresh_token":"rt_xxx"}' | jq
```

Inspect accounts:

```bash
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

Wake locally blocked accounts:

```bash
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/wake | jq
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/<id>/wake | jq
```

Call the business API directly:

```bash
curl -N http://127.0.0.1:18039/v1/responses \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-5.4",
    "instructions": "",
    "input": [
      {
        "role": "user",
        "content": [
          { "type": "input_text", "text": "Reply with exactly: OK" }
        ]
      }
    ]
  }'
```

Point Codex at the local gateway:

```bash
OPENAI_API_KEY=dummy \
codex \
  -c 'openai_base_url="http://127.0.0.1:18039/v1"' \
  -c 'model_provider="openai"'
```

If you use a custom provider, confirm `supports_websockets = true`; otherwise Codex falls back to SSE.

## 9. Minimal Pre-Merge Checklist

Before finishing a change, check at least:

- `cargo fmt --all`
- `cargo test -q` or at least the affected test subset
- `cargo build --release`
- at least one happy path
- at least one classified failure path

If you changed these areas, also verify:

- `request_normalization.rs`
  - no fields that real Codex would not send
- `upstream/headers.rs`
  - header names, presence conditions, and semantics still line up with Codex
- `responses/`
  - downstream SSE / websocket consumption shape has not drifted
- `classifier.rs`
  - account block / cooldown / trash transitions still match existing policy

## 10. Things Not To Do

- do not expand the project into a generic multi-upstream proxy
- do not bypass Codex's Rust transport by writing a separate outbound client
- do not add a separate persisted runtime-state system for accounts
- do not invent speculative fields for unknown clients
- do not change Codex fingerprint details without evidence

## 11. Where Future Docs Should Live

Keep the split clear:

- `README*.md`
  - user-facing quick start
- `docs/DESIGN*.md`
  - design principles and tradeoffs
- `docs/DEVELOPMENT*.md`
  - maintainer workflow and development notes

Do not mix development workflow, design memo, and end-user usage in a single file.

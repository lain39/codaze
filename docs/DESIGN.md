[English](DESIGN.md) | [简体中文](DESIGN.zh-CN.md)

# Codaze Design Memo

This document records design principles, boundaries, and tradeoffs. It is not meant to be a quick-start guide. Startup commands, endpoint examples, and operator-facing entry points stay in the top-level README files.

## 1. Product Positioning

Codaze is a local multi-account gateway for `chatgpt.com/backend-api/codex`.

It is not trying to become a generic LLM gateway. The goal is to be an OpenAI/Codex-specific relay that stays as close as practical to Codex client behavior.

Concretely, the project exists to solve three problems:

- provide a single local entrypoint for Codex-shaped traffic
- route and recover across multiple ChatGPT accounts
- keep outbound request paths, headers, and transport behavior close to real Codex

## 2. Non-Goals

The following are explicitly out of scope:

- multi-provider abstraction
- Claude / Gemini / Amp / custom protocol matrices
- turning the gateway into a remote multi-tenant SaaS
- rebuilding a full Codex session runtime inside the gateway
- replacing Codex's local thread / compact / resume / fork state machine

The project staying "Codex-specific" and "local-first" is intentional, not a temporary limitation.

## 3. High-Level Structure

The project has three core surfaces:

- downstream entry
  - exposes `/v1/models`, `/v1/responses`, `/v1/responses/compact`, and `/v1/memories/trace_summarize`
  - accepts local client HTTP and websocket traffic
- routing and account pool
  - persists refresh tokens
  - manages access-token refresh, account state, cooldown, and recovery
  - selects a currently usable account for each request
- upstream transport
  - uses Codex's Rust transport and request-building behavior to access `chatgpt.com/backend-api/codex`

This is not meant to be a feature-heavy application service. It is a deliberately narrow proxy layer.

## 4. Why Staying Close to Codex Matters

Codaze does not treat "it works" as sufficient. "It behaves like real Codex as much as practical" is the more important constraint.

There are three reasons:

- upstream risk controls and capability toggles increasingly depend on real client paths
- the closer we stay to Codex, the easier it is to reason about upstream changes later
- if the gateway invents too much of its own behavior, every issue becomes ambiguous: did upstream change, or did we drift?

That leads to a few hard constraints:

- reuse Codex's Rust HTTP stack instead of writing a separate outbound transport
- prefer Codex endpoint paths and header conventions
- do not inject fingerprint fields that real Codex does not send
- do not guess at client-internal state that the gateway cannot maintain lightly

## 5. Fingerprint Strategy

The project supports two outbound fingerprint modes:

- `normalize`
- `passthrough`

### 5.1 `normalize`

`normalize` is the default mode. It means:

- shape downstream requests into something closer to real Codex
- but only for fields that real Codex stably carries

It is not "fill every missing field." It is intentionally narrow shaping.

Current examples of allowed shaping:

- `store: false` on `/v1/responses`
- `parallel_tool_calls`
- derive `x-openai-subagent` from `x-codex-session-source`
- derive `x-codex-parent-thread-id` from `x-codex-session-source`

### 5.2 `passthrough`

`passthrough` means:

- keep the downstream caller's request fingerprint as-is as much as possible
- avoid extra Codex-specific normalization

It does not mean:

- disable the account pool
- disable refresh
- disable error classification
- disable response-side compatibility rewrites
- disable local admin behavior

So `passthrough` only changes request fingerprint shaping. It does not turn the gateway into a blind TCP tunnel.

## 6. Things We Must Not Invent

This is one of the most important design boundaries in the project.

### 6.1 `x-codex-window-id`

Recent Codex builds send `x-codex-window-id`, but it is not a static value. Its shape is:

`conversation_id:generation`

The generation changes across:

- new conversation: `0`
- after compact: increments
- resume: inherited
- fork: reset

That means the field depends on Codex's internal thread state, not just the current request.

Current rule:

- if the downstream caller is real Codex and already sends the header, the gateway forwards it
- if the caller is not Codex, the gateway does not synthesize it

### 6.2 Websocket `response.create.client_metadata`

Codex websocket `response.create` also carries identity information in `client_metadata`, for example:

- `x-codex-window-id`
- `x-openai-subagent`
- `x-codex-parent-thread-id`
- `x-codex-turn-metadata`
- `x-codex-installation-id`

For fields tied to client-local thread/session state, the rule is the same:

- forward when there is a real source value
- do not fabricate fields that depend on client-internal session state

`x-codex-installation-id` is the exception. Current Codex sends it as a stable installation marker, and
Codaze treats it as a selected-upstream-account fingerprint instead of a downstream-thread-state
field:

- in `normalize` mode, Codaze derives a stable UUID from the selected upstream `ChatGPT-Account-ID`
- it writes that value to `/v1/responses` `client_metadata`, `/v1/responses/compact` request
  headers, and websocket `response.create.client_metadata`
- in `passthrough` mode, Codaze does not synthesize or override the field
- if websocket pre-commit failover switches to a replacement upstream account, the replayed
  `response.create` is rewritten with the replacement connection's installation id so each upstream
  websocket connection sees a self-consistent value

## 7. Why We Do Not Rebuild the Full Session State Machine

Technically, the gateway could maintain more state. But doing so would change the nature of the project.

To generate `x-codex-window-id` correctly, the gateway would need to know:

- which logical thread is current
- whether compact really succeeded
- whether the current request is a resume
- whether it is a fork
- which requests belong to the same logical thread

That would push the project from "lightweight Codex gateway" toward "half of a Codex runtime."

That direction is not justified right now because:

- state complexity rises sharply
- it duplicates Codex client-side state
- it is easier to drift from real Codex semantics
- the maintenance cost is disproportionate to the benefit

So the project chooses to admit its boundary instead of building a fragile fake state machine for the sake of appearances.

## 8. Account System Design

The account system is centered on refresh tokens, not access tokens.

Core rules:

- refresh tokens are the durable root credentials
- access tokens are runtime cache
- access-token failure does not imply account failure
- only confirmed refresh-token invalidation moves an account into `trash/`

Current shape:

- one JSON file per account
- startup and runtime both support rescanning the account directory
- HTTP import and manual file drop are both supported

### 8.1 Why Runtime State Stays In Memory

Runtime account state is intentionally not persisted separately.

Reasons:

- state files add a lot of complexity
- much of the state is scheduling advice, not long-lived truth
- process restarts are allowed to lazily rediscover usable state

Only account files are persisted. Runtime state remains in memory.

### 8.2 Why Refresh Is Lazy

The project intentionally does not refresh immediately on import.

Reasons:

- keeps the main flow simpler
- reduces meaningless refresh traffic
- makes "refresh only when routed" the single path
- concentrates error classification and account-state transitions on real request paths

## 9. Routing Design

The goal is not "pin one client to one account forever." The goal is:

- select a usable account
- preserve overall availability under quota and risk constraints
- keep a basic level of fairness across accounts

Current policies:

- `round_robin`
- `least_in_flight`
- `fill_first`

`fill_first` means:

- prefer to pour traffic into the first usable account
- it is a quota-consumption policy, not HTTP session stickiness

The project does not make HTTP-layer stickiness the default because that would conflict with the balancing goal of an account pool.

## 10. Error Classification And Recovery

Upstream failures are not handled as raw text blobs. They are classified first, then the gateway decides:

- whether the account should be cooled down
- whether failover should happen
- whether lazy recovery is allowed
- whether the account should be permanently removed from routing

At a high level:

- access token invalid: refresh, do not move to `trash/`
- refresh token invalid: move to `trash/`
- rate limit / usage limit: set `blocked_until`
- risk-control signals: long cooldown
- temporary upstream failure: local backoff, not permanent disable

One important rule:

`blocked_until` is a local scheduling hint, not an authoritative mirror of upstream quota-reset time.

## 11. Websocket Compatibility Strategy

There is one intentionally special websocket-side compatibility rewrite:

- if upstream returns `previous_response_not_found`
- the gateway rewrites it into `websocket_connection_limit_reached`

The purpose is not to hide the error. It is to trigger Codex's existing reset/retry path so the next `response.create` drops `previous_response_id` and resends a full request.

This is one of the few places where the gateway explicitly changes error shape for Codex compatibility.

It is intentional design, not an accidental patch.

## 12. Why `/models` Is Kept Simple

The project does not turn `/models` into a synthetic account-pool-derived result, and it does not do complex local aggregation.

Current rule:

- `/models` still fetches its ground-truth snapshot from the normal upstream path
- it keeps a local cache of the most recent successful upstream models snapshot
- Codex callers always go straight to upstream and refresh the current snapshot
- for non-Codex callers:
  - if the cache is still within TTL, return the fresh snapshot directly
  - if only a stale snapshot exists, return the current snapshot first and trigger a background refresh
  - if no snapshot exists yet, fetch synchronously from upstream
- it does not try to invent cross-account aggregation or new model-visibility semantics just to look clever

Reasons:

- other endpoints need a stable models snapshot to derive default `parallel_tool_calls`
- upstream already knows which models are visible, so the gateway should not rebuild that policy locally
- a light cache reduces `/models` churn without turning the project into a heavier state machine

## 13. Local-First Security Boundary

The project intentionally only supports loopback listeners and does not add a second token-based auth layer on top of admin APIs by default.

To reduce the risk that someone proxies public traffic outward and accidentally exposes `/admin/*` too, public and admin APIs live on separate loopback ports by default.

That is not because admin endpoints are insensitive. It is because the current product shape is explicitly a local tool.

If the project ever becomes a remote service, the security model must be redesigned instead of inheriting today's local defaults.

## 14. Ongoing Maintenance Rules

If the project keeps evolving, these rules should stay in place:

- check Codex source and real traffic before adding fields
- forward when possible; do not pretend to maintain state that cannot be maintained lightly
- keep error classification centralized instead of scattering it across transport details
- keep README usage-oriented and keep design principles in a dedicated document
- if some behavior is a deliberate rewrite for Codex compatibility, say so clearly in both code and docs

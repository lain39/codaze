[English](OPERATIONS.md) | [简体中文](OPERATIONS.zh-CN.md)

# Codaze Operations And Troubleshooting

This document is for people running and maintaining Codaze. It focuses on:

- how to read account state
- how to interpret common failures
- when an account moves into `trash/`
- what `wake` actually clears
- which endpoints are useful for inspection

Development details: [DEVELOPMENT.md](DEVELOPMENT.md)

## 1. Service Ports

There are two local ports by default:

- public: `127.0.0.1:18039`
- admin: `127.0.0.1:18040`

Public:

- `GET /health`
- `GET /v1/models`
- `POST /v1/responses`
- `GET /v1/responses` websocket
- `POST /v1/responses/compact`
- `POST /v1/memories/trace_summarize`

Admin:

- `POST /admin/accounts/import`
- `POST /admin/accounts/wake`
- `POST /admin/accounts/{id}/wake`
- `GET /admin/accounts`
- `DELETE /admin/accounts/{id}`
- `GET /admin/routing/policy`
- `PUT /admin/routing/policy`

## 2. Basic Inspection

Check whether the process is alive:

```bash
curl -sS http://127.0.0.1:18039/health | jq
```

Current `/health` only returns:

```json
{ "ok": true }
```

Its meaning is intentionally narrow:

- local HTTP service is responding
- it does not prove upstream is available
- it does not prove the account pool has a usable account
- it does not prove refresh or websocket is healthy

Inspect the account pool:

```bash
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

Inspect the current routing policy:

```bash
curl -sS http://127.0.0.1:18040/admin/routing/policy | jq
```

## 3. Account View Fields

For each account returned by `GET /admin/accounts`, pay attention to:

- `id`
  - local account identifier
  - currently a short hash derived from the refresh token
- `label`
  - optional human-readable label
- `email`
  - known email, usually filled after successful refresh
- `routing_state`
  - current routing state
- `blocked_reason`
  - why the scheduler should not currently select it
- `blocked_source`
  - whether the block came from upstream retry hints, local backoff, or fixed policy
- `blocked_until`
  - earliest local retry time
- `refresh_in_flight`
  - whether a refresh is active
- `in_flight_requests`
  - number of active requests
- `access_token_expires_at`
  - current cached access-token expiry
- `last_refresh_at`
  - last successful refresh
- `last_success_at`
  - last successful request
- `last_error_at`
  - last failure timestamp
- `last_error`
  - last recorded failure detail

## 4. `routing_state` Meanings

### `cold`

Meaning:

- no usable access token is currently cached
- the account must refresh when it is selected

Common sources:

- freshly imported account
- access token was cleared after rejection
- block expired and the account returned to an unrefreshed state

### `warming`

Meaning:

- refresh is in progress
- or the account has just been selected and is still establishing the request

Usually short-lived.

### `ready`

Meaning:

- there is a cached access token
- there is no active block
- the account may be routed

### `cooldown`

Meaning:

- the account should not be routed right now
- usually rate limit or quota exhaustion

Recovery:

- wait until `blocked_until`
- or manually `wake`

### `risk_controlled`

Meaning:

- the account hit a risk-control path
- more severe than ordinary temporary failure

Current fixed cooldown:

- 30 minutes

### `temporarily_unavailable`

Meaning:

- temporarily unusable
- can be caused by upstream 5xx, network errors, rejected access tokens, websocket transient errors, and similar failures

### `auth_invalid`

Meaning:

- refresh token has been confirmed invalid
- this is the closest state to a dead account entity

`wake` does not clear this state.

## 5. `blocked_reason` / `blocked_source`

### `blocked_reason`

Current main values:

- `rate_limited`
- `quota_exhausted`
- `risk_controlled`
- `temporarily_unavailable`
- `auth_invalid`

### `blocked_source`

Current main values:

- `upstream_retry_after`
  - upstream provided `retry-after` or equivalent `resets_*`
- `local_backoff`
  - upstream did not provide a specific retry time, so the gateway used exponential backoff
- `fixed_policy`
  - fixed local policy such as 30 minutes for risk control or 60 seconds for temporary failure

## 6. How To Read `blocked_until`

`blocked_until` is a local scheduling hint, not an authoritative mirror of upstream reset time.

It can come from three places:

- upstream `retry-after`
- structured upstream fields such as `resets_at` / `resets_in_seconds`
- gateway-local backoff or fixed cooldown

So:

- it is suitable for answering "when should I try this account again?"
- it is not suitable as the authoritative source of real upstream quota reset time

## 7. Cooldown Rules

Important current values:

- fixed cooldown for temporary failure: 60 seconds
- fixed cooldown for risk control: 30 minutes
- local exponential backoff:
  - starts at 1 second
  - doubles on each step
  - capped at 30 minutes

Local exponential backoff is used for:

- rate limit when upstream does not provide a concrete `retry-after`
- quota exhaustion when upstream does not provide a concrete retry time

## 8. How Common Failures Map To Account State

### Access token rejected

Typical result:

- clear current access token
- move to `temporarily_unavailable`
- default cooldown of 60 seconds

This does not go to `trash/`.

Reason:

- access-token failure does not imply refresh-token failure

### Refresh token invalid

Typical result:

- account enters `auth_invalid`
- the account file moves into `trash/`
- the record leaves the active pool

This is the only hard-failure path that automatically enters `trash/`.

### Rate limit

Typical result:

- `routing_state = cooldown`
- `blocked_reason = rate_limited`
- if upstream provides time, use that
- otherwise use local exponential backoff

### Usage limit / quota exhausted

Typical result:

- `routing_state = cooldown`
- `blocked_reason = quota_exhausted`
- if upstream provides `resets_at` / `resets_in_seconds` / `retry-after`, store it in `blocked_until`
- otherwise use local exponential backoff

### Unusual activity / Arkose / Turnstile

Typical result:

- `routing_state = risk_controlled`
- `blocked_reason = risk_controlled`
- fixed cooldown of 30 minutes

### 5xx / timeout / network error

Typical result:

- `routing_state = temporarily_unavailable`
- usually a short local block

### Invalid request body

Examples:

- `invalid_prompt`
- `context_length_exceeded`
- other request-rejected cases

These errors usually:

- get recorded into `last_error`
- but do not permanently kill the account

## 9. Meaning Of `trash/`

`trash/` is only for account files whose refresh token has been confirmed invalid.

Current semantics:

- access token expired: not `trash/`
- request was rate limited: not `trash/`
- quota exhausted: not `trash/`
- risk control: not `trash/`
- network error: not `trash/`
- refresh token confirmed invalid: yes, `trash/`

So `trash/` is not a generic "failed account recycle bin." It is a "hard-invalid refresh token" recycle bin.

## 10. What `wake` Can And Cannot Do

Wake all:

```bash
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/wake | jq
```

Wake one:

```bash
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/<id>/wake | jq
```

What `wake` does:

- clears `blocked_reason`
- clears `blocked_source`
- clears `blocked_until`
- resets local backoff level
- if the account already has an access token, returns it to `ready`
- if there is no access token, returns it to `cold`
- if refresh is still in progress, keeps it in `warming`

What `wake` does not do:

- it does not recover `auth_invalid`
- it does not move accounts back out of `trash/`
- it does not refresh immediately
- it does not fix account-file problems

## 11. Client Errors And Admin Errors Are Different Layers

This is intentional.

Client-facing errors:

- are kept as retry-friendly and sanitized as practical
- avoid leaking per-account details
- may still differ by caller surface, for example Codex-style SSE versus ordinary HTTP JSON
- for example, when the whole pool is unavailable, clients see:
  - `No account available right now. Try again later.`

Admin-facing errors:

- try to preserve the real reason in `last_error`
- are meant for distinguishing rate limit, quota, risk control, refresh-token invalidation, websocket upstream failure, and so on

So:

- client errors are for the calling surface
- admin errors are for operations and debugging

Do not expect them to be identical.

## 12. What Happens When The Client Disconnects

Current behavior:

- when an SSE downstream connection drops, the related account occupancy is released
- when a websocket client disconnects or upgrade fails, the upstream websocket is also closed and the account is settled

That means:

- normal client interruption should not keep `in_flight_requests` occupied for long
- the account should be released back into the routing pool

If you suspect abnormal occupancy, inspect:

```bash
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

Focus on:

- `in_flight_requests`
- `refresh_in_flight`
- `last_error`

## 13. Graceful Shutdown Boundaries

On `SIGINT` / `SIGTERM`:

- the public listener stops accepting new requests
- the admin listener stops accepting new requests
- the account-rescan task exits

There is no extra "flush all runtime state to disk during shutdown" stage.

That is intentional:

- account-entity changes are already persisted immediately during normal operation
- hot state such as `blocked_*` and `last_error` is intentionally memory-only

## 14. Common Troubleshooting Paths

### The service looks alive, but Codex keeps failing

First check:

```bash
curl -sS http://127.0.0.1:18039/health | jq
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

If `/health` is fine but all accounts are blocked:

- inspect `blocked_reason`
- inspect `blocked_until`
- inspect `last_error`

### One account keeps failing

Check its:

- `routing_state`
- `blocked_reason`
- `blocked_source`
- `last_error`

Then decide:

- access-token issue: usually wait for refresh or reselection
- refresh-token invalid: it moves into `trash/`
- rate limit / quota: wait for `blocked_until` or use `wake`
- risk control: usually do not force `wake` repeatedly

### All accounts are unavailable

The client usually only sees a sanitized error. It will not tell you which account hit which quota or risk-control path.

The exact error shape can still depend on the caller surface, for example Codex-style SSE versus ordinary HTTP JSON.

In that situation, trust `/admin/accounts`, not the sanitized client-facing error surface.

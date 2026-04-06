[English](API.md) | [简体中文](API.zh-CN.md)

# Codaze API Reference

This document lists the HTTP and websocket surfaces exposed by Codaze. Request and response examples aim to stay close to the current implementation.

Design rationale: [DESIGN.md](DESIGN.md)  
Operational semantics: [OPERATIONS.md](OPERATIONS.md)

## Ports

By default there are two local ports:

- public: `127.0.0.1:18039`
- admin: `127.0.0.1:18040`

The public port is for downstream clients.  
The admin port is for local control only.

## Public Endpoints

### `GET /health`

Purpose:

- checks whether the local HTTP service is alive

Request:

```bash
curl -sS http://127.0.0.1:18039/health | jq
```

Response:

```json
{
  "ok": true
}
```

Notes:

- does not imply upstream is available
- does not imply the account pool currently has a usable account

### `GET /v1/models`

Purpose:

- selects a usable account via the current routing policy
- forwards to upstream `GET /backend-api/codex/models`

Request:

```bash
curl -sS http://127.0.0.1:18039/v1/models | jq
```

Example response:

```json
[
  {
    "slug": "gpt-5.4",
    "supports_parallel_tool_calls": true
  },
  {
    "slug": "gpt-5.4-mini",
    "supports_parallel_tool_calls": true
  }
]
```

Notes:

- this is an upstream passthrough surface; exact fields depend on upstream
- `normalize` / `passthrough` only affect outbound request shaping, not the response body

### `POST /v1/responses`

Purpose:

- main business endpoint
- returns `text/event-stream`
- exposes Codex-shaped Responses SSE over HTTP

Example request:

```bash
curl -N http://127.0.0.1:18039/v1/responses \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-5.4",
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

Request notes:

- in `normalize` mode, the gateway injects stable Codex fields such as:
  - `store: false`
  - model-derived `parallel_tool_calls`
- the request body may contain a private `_gateway` object; it is consumed locally and stripped before forwarding

Example with `_gateway`:

```json
{
  "model": "gpt-5.4",
  "instructions": "You are a helpful assistant.",
  "input": [
    {
      "role": "user",
      "content": [
        { "type": "input_text", "text": "Reply with exactly: OK" }
      ]
    }
  ],
  "_gateway": {
    "session_source": "exec"
  }
}
```

Successful response example:

```text
event: response.created
data: {"type":"response.created","sequence_number":1,"response":{"id":"resp_123","object":"response","created_at":1770000000,"status":"in_progress","background":false,"error":null}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","sequence_number":2,"item_id":"item_123","output_index":0,"content_index":0,"delta":"OK"}

event: response.completed
data: {"type":"response.completed","sequence_number":3,"response":{"id":"resp_123","object":"response","created_at":1770000000,"status":"completed","background":false}}
```

Pre-stream failure example:

```text
event: response.failed
data: {"type":"response.failed","sequence_number":1,"response":{"id":"resp_123","object":"response","created_at":1770000000,"status":"failed","background":false,"error":{"code":"rate_limit_exceeded","message":"Rate limit reached for gpt-5.4. Please try again in 11.5s."}}}
```

Notes:

- pre-stream failures on `/v1/responses` are wrapped into synthetic SSE instead of returning raw upstream HTTP 4xx/5xx to Codex
- this exists so Codex can continue using its existing Responses error-handling path

### `GET /v1/responses` websocket

Purpose:

- provides the Responses websocket channel for websocket-capable downstream clients
- currently used mainly by Codex `exec`

Example websocket upgrade:

```bash
curl --http1.1 \
  -H 'Connection: Upgrade' \
  -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' \
  -H 'Sec-WebSocket-Key: SGVsbG9Xb3JsZDEyMzQ1Ng==' \
  http://127.0.0.1:18039/v1/responses
```

In practice, this is normally initiated by Codex or another websocket client, not by manual `curl`.

First-frame request example:

```json
{
  "type": "response.create",
  "model": "gpt-5.4",
  "instructions": "You are Codex...",
  "input": [
    {
      "role": "user",
      "content": [
        { "type": "input_text", "text": "Reply with exactly: OK" }
      ]
    }
  ]
}
```

Successful upstream message example:

```json
{
  "type": "response.created",
  "response": {
    "id": "resp_123",
    "object": "response",
    "created_at": 1770000000,
    "status": "in_progress",
    "background": false,
    "completed_at": null,
    "error": null
  }
}
```

Error message example:

```json
{
  "type": "error",
  "status": 400,
  "error": {
    "type": "invalid_request_error",
    "code": "websocket_connection_limit_reached",
    "message": "Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue."
  }
}
```

Notes:

- for pre-commit websocket errors such as `previous_response_not_found`, the gateway rewrites the error into a retryable reset/reconnect shape
- this is websocket-specific behavior and does not apply to ordinary HTTP `/v1/responses`

### `POST /v1/responses/compact`

Purpose:

- unary JSON form of responses

Example request:

```bash
curl -sS http://127.0.0.1:18039/v1/responses/compact \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-5.4",
    "instructions": "You are a helpful assistant.",
    "input": [
      {
        "role": "user",
        "content": [
          { "type": "input_text", "text": "Reply with exactly: OK" }
        ]
      }
    ]
  }' | jq
```

Successful response example:

```json
{
  "id": "resp_123",
  "object": "response",
  "created_at": 1770000000,
  "status": "completed",
  "model": "gpt-5.4",
  "output": [
    {
      "id": "msg_123",
      "type": "message",
      "role": "assistant",
      "content": [
        {
          "type": "output_text",
          "text": "OK"
        }
      ]
    }
  ]
}
```

Notes:

- this is normal JSON, not SSE
- unlike `/v1/responses`, the implementation does not invent `store` here
- `parallel_tool_calls` is only injected in `normalize` mode using model rules

### `POST /v1/memories/trace_summarize`

Purpose:

- thin compatibility endpoint
- currently proxies upstream `memories/trace_summarize`

Example request:

```bash
curl -sS http://127.0.0.1:18039/v1/memories/trace_summarize \
  -H 'Content-Type: application/json' \
  -d '{
    "trace": "hello"
  }' | jq
```

Example response when upstream is available:

```json
{
  "summary": "hello"
}
```

Common response while upstream is still unavailable:

```json
{
  "detail": "Not Found"
}
```

Notes:

- availability depends mainly on upstream
- the gateway only routes and forwards; it does not implement local trace summarization

## Admin Endpoints

### `POST /admin/accounts/import`

Purpose:

- import one refresh-token-backed account
- deduplicate repeated imports by current refresh token
- the only required field is `refresh_token`; `label` and `email` are optional metadata

Example request:

```bash
curl -sS http://127.0.0.1:18040/admin/accounts/import \
  -H 'Content-Type: application/json' \
  -d '{
    "refresh_token": "rt_xxx"
  }' | jq
```

Create response example:

```json
{
  "account": {
    "id": "a5bbe57811399ad3e973551fe6ac1f48",
    "label": "main",
    "email": "user@example.com",
    "routing_state": "cold",
    "blocked_reason": null,
    "blocked_source": null,
    "blocked_until": null,
    "account_id": null,
    "plan_type": null,
    "refresh_in_flight": false,
    "in_flight_requests": 0,
    "access_token_expires_at": null,
    "last_refresh_at": null,
    "last_selected_at": null,
    "last_success_at": null,
    "last_error_at": null,
    "last_error": null
  },
  "already_exists": false
}
```

Repeated import example:

```json
{
  "account": {
    "id": "a5bbe57811399ad3e973551fe6ac1f48",
    "label": "updated",
    "email": "updated@example.com",
    "routing_state": "ready",
    "blocked_reason": null,
    "blocked_source": null,
    "blocked_until": null,
    "account_id": "acct_123",
    "plan_type": "plus",
    "refresh_in_flight": false,
    "in_flight_requests": 0,
    "access_token_expires_at": "2026-04-15T07:58:42Z",
    "last_refresh_at": "2026-04-05T07:58:42Z",
    "last_selected_at": "2026-04-05T07:58:41Z",
    "last_success_at": "2026-04-05T07:58:47Z",
    "last_error_at": null,
    "last_error": null
  },
  "already_exists": true
}
```

Invalid refresh-token request example:

```json
{
  "error": {
    "message": "refresh_token must not be empty"
  }
}
```

Optional metadata update example:

```bash
curl -sS http://127.0.0.1:18040/admin/accounts/import \
  -H 'Content-Type: application/json' \
  -d '{
    "refresh_token": "rt_xxx",
    "label": "main",
    "email": "user@example.com"
  }' | jq
```

### `GET /admin/accounts`

Purpose:

- inspect the current active account pool

Request:

```bash
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

Response example:

```json
{
  "accounts": [
    {
      "id": "a5bbe57811399ad3e973551fe6ac1f48",
      "label": "main",
      "email": "user@example.com",
      "routing_state": "ready",
      "blocked_reason": null,
      "blocked_source": null,
      "blocked_until": null,
      "account_id": "acct_123",
      "plan_type": "plus",
      "refresh_in_flight": false,
      "in_flight_requests": 0,
      "access_token_expires_at": "2026-04-15T07:58:42Z",
      "last_refresh_at": "2026-04-05T07:58:42Z",
      "last_selected_at": "2026-04-05T07:58:41Z",
      "last_success_at": "2026-04-05T07:58:47Z",
      "last_error_at": null,
      "last_error": null
    }
  ]
}
```

Notes:

- only the active account pool is listed
- invalid account files already moved into `trash/` are not listed here

### `POST /admin/accounts/{id}/wake`

Purpose:

- clear block state for one account
- does not recover `auth_invalid`

Example request:

```bash
curl -sS -X POST \
  http://127.0.0.1:18040/admin/accounts/a5bbe57811399ad3e973551fe6ac1f48/wake | jq
```

Response example:

```json
{
  "disposition": "woken",
  "account": {
    "id": "a5bbe57811399ad3e973551fe6ac1f48",
    "label": "main",
    "email": "user@example.com",
    "routing_state": "ready",
    "blocked_reason": null,
    "blocked_source": null,
    "blocked_until": null,
    "account_id": "acct_123",
    "plan_type": "plus",
    "refresh_in_flight": false,
    "in_flight_requests": 0,
    "access_token_expires_at": "2026-04-15T07:58:42Z",
    "last_refresh_at": "2026-04-05T07:58:42Z",
    "last_selected_at": "2026-04-05T07:58:41Z",
    "last_success_at": "2026-04-05T07:58:47Z",
    "last_error_at": "2026-04-05T08:00:00Z",
    "last_error": "rate limited"
  }
}
```

Unknown account example:

```json
{
  "error": {
    "message": "unknown account `a5bbe57811399ad3e973551fe6ac1f48`"
  }
}
```

### `POST /admin/accounts/wake`

Purpose:

- clear block state for all wakeable accounts

Example request:

```bash
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/wake | jq
```

Response example:

```json
{
  "woken": 1,
  "skipped_auth_invalid": 1,
  "accounts": [
    {
      "disposition": "woken",
      "account": {
        "id": "acc_ready",
        "label": null,
        "email": null,
        "routing_state": "ready",
        "blocked_reason": null,
        "blocked_source": null,
        "blocked_until": null,
        "account_id": null,
        "plan_type": null,
        "refresh_in_flight": false,
        "in_flight_requests": 0,
        "access_token_expires_at": null,
        "last_refresh_at": null,
        "last_selected_at": null,
        "last_success_at": null,
        "last_error_at": null,
        "last_error": null
      }
    },
    {
      "disposition": "skipped_auth_invalid",
      "account": {
        "id": "acc_dead",
        "label": null,
        "email": null,
        "routing_state": "auth_invalid",
        "blocked_reason": "auth_invalid",
        "blocked_source": "fixed_policy",
        "blocked_until": null,
        "account_id": null,
        "plan_type": null,
        "refresh_in_flight": false,
        "in_flight_requests": 0,
        "access_token_expires_at": null,
        "last_refresh_at": null,
        "last_selected_at": null,
        "last_success_at": null,
        "last_error_at": null,
        "last_error": null
      }
    }
  ]
}
```

### `DELETE /admin/accounts/{id}`

Purpose:

- remove an account from the active pool
- delete the corresponding account file

Example request:

```bash
curl -i -X DELETE \
  http://127.0.0.1:18040/admin/accounts/a5bbe57811399ad3e973551fe6ac1f48
```

Successful response:

```text
HTTP/1.1 204 No Content
```

Unknown account:

```json
{
  "error": {
    "message": "unknown account `a5bbe57811399ad3e973551fe6ac1f48`"
  }
}
```

### `GET /admin/routing/policy`

Purpose:

- inspect the current routing policy

Request:

```bash
curl -sS http://127.0.0.1:18040/admin/routing/policy | jq
```

Response example:

```json
{
  "routing_policy": "least_in_flight"
}
```

### `PUT /admin/routing/policy`

Purpose:

- change the routing policy at runtime

Supported values:

- `round_robin`
- `least_in_flight`
- `fill_first`

Example request:

```bash
curl -sS -X PUT http://127.0.0.1:18040/admin/routing/policy \
  -H 'Content-Type: application/json' \
  -d '{
    "routing_policy": "fill_first"
  }' | jq
```

Successful response:

```json
{
  "routing_policy": "fill_first"
}
```

Missing field:

```json
{
  "error": {
    "message": "missing routing_policy"
  }
}
```

Unsupported value:

```json
{
  "error": {
    "message": "unsupported routing policy `bad`; expected one of: round_robin, least_in_flight, fill_first"
  }
}
```

## Common Error Examples

### No usable account is available right now

Typical business-endpoint response:

```json
{
  "error": {
    "code": "server_is_overloaded",
    "message": "No account available right now. Try again later."
  }
}
```

Notes:

- this is the unified downstream-facing error surface
- it does not directly expose a specific account's quota-reset time to the client

### Upstream refresh failure

If the failure happens during refresh, and not in the `/v1/responses` pre-stream SSE-wrapping path, the upstream JSON error is usually forwarded to the caller directly.

Example:

```json
{
  "error": {
    "message": "Could not validate your refresh token. Please try signing in again.",
    "type": "invalid_request_error",
    "param": null,
    "code": "refresh_token_expired"
  }
}
```

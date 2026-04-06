[English](API.md) | [简体中文](API.zh-CN.md)

# Codaze API 文档

这份文档单独列出 `Codaze` 当前暴露的 HTTP / websocket 接口，请求和响应样例尽量贴近当前实现。

设计取舍见 [DESIGN.zh-CN.md](DESIGN.zh-CN.md)。  
运维语义见 [OPERATIONS.zh-CN.md](OPERATIONS.zh-CN.md)。

## 端口

默认分成两个本地端口：

- 业务端口：`127.0.0.1:18039`
- 管理端口：`127.0.0.1:18040`

业务端口只暴露给下游客户端。  
管理端口只暴露本地控制面。

## 业务接口

### `GET /health`

用途：

- 只检查本地 HTTP 服务是否还活着

请求：

```bash
curl -sS http://127.0.0.1:18039/health | jq
```

响应：

```json
{
  "ok": true
}
```

说明：

- 不表示上游可用
- 不表示账号池里一定有可用账号

### `GET /v1/models`

用途：

- 缓存冷时按当前路由逻辑选一个可用账号
- 惰性刷新一份上游 `GET /backend-api/codex/models` 快照
- 根据 `originator` 返回不同响应形状

请求：

```bash
curl -sS http://127.0.0.1:18039/v1/models | jq
```

非 Codex 调用方的响应样例：

```json
{
  "object": "list",
  "data": [
    {
      "id": "gpt-5.4",
      "object": "model",
      "created": 0,
      "owned_by": "openai"
    },
    {
      "id": "gpt-5.4-mini",
      "object": "model",
      "created": 0,
      "owned_by": "openai"
    }
  ]
}
```

Codex 调用方的响应样例：

```json
{
  "models": [
    {
      "slug": "gpt-5.4",
      "supports_parallel_tool_calls": true
    },
    {
      "slug": "gpt-5.4-mini",
      "supports_parallel_tool_calls": true
    }
  ]
}
```

说明：

- `originator` 明确是 Codex 时返回 Codex 形状
- 其他调用方返回 OpenAI 兼容的 `object/data` 形状
- 网关会缓存一份上游 Codex models 快照，并用它推导默认的 `parallel_tool_calls`

### `POST /v1/responses`

用途：

- 主业务接口
- 返回 `text/event-stream`
- 在 HTTP 路径下对下游表现为 Codex 风格的 Responses SSE

请求样例：

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

请求说明：

- `normalize` 模式下，网关会按 Codex 习惯补齐稳定字段，例如：
  - `store: false`
  - `instructions` 缺失或为 `null` 时补成 `""`
  - 按模型推导的 `parallel_tool_calls`
- 请求体里允许带一个私有 `_gateway` 对象；它只在本地消费，转发前会被剥离

带 `_gateway` 的样例：

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

成功响应样例：

```text
event: response.created
data: {"type":"response.created","sequence_number":1,"response":{"id":"resp_123","object":"response","created_at":1770000000,"status":"in_progress","background":false,"error":null}}

event: response.output_text.delta
data: {"type":"response.output_text.delta","sequence_number":2,"item_id":"item_123","output_index":0,"content_index":0,"delta":"OK"}

event: response.completed
data: {"type":"response.completed","sequence_number":3,"response":{"id":"resp_123","object":"response","created_at":1770000000,"status":"completed","background":false}}
```

Codex 调用方的建流前失败样例：

```text
event: response.failed
data: {"type":"response.failed","sequence_number":1,"response":{"id":"resp_123","object":"response","created_at":1770000000,"status":"failed","background":false,"error":{"code":"rate_limit_exceeded","message":"Rate limit reached for gpt-5.4. Please try again in 11.5s."}}}
```

非 Codex 调用方的建流前失败样例：

```json
{
  "error": {
    "message": "Rate limit reached for gpt-5.4. Please try again in 11s.",
    "code": "rate_limit_exceeded"
  }
}
```

说明：

- `/v1/responses` 的建流前失败只对 Codex 调用方包装成 synthetic SSE
- 非 Codex 调用方会直接收到普通 HTTP JSON 错误
- 这样 Codex 仍能走它已支持的 Responses 错误解析链，而其他客户端不用处理 Codex 专用的 SSE 失败面

### `GET /v1/responses` websocket

用途：

- 对支持 websocket 的下游提供 Responses websocket 通道
- 当前主要用于 Codex `exec`

建立连接样例：

```bash
curl --http1.1 \
  -H 'Connection: Upgrade' \
  -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' \
  -H 'Sec-WebSocket-Key: SGVsbG9Xb3JsZDEyMzQ1Ng==' \
  http://127.0.0.1:18039/v1/responses
```

实际使用时通常不是手工 `curl`，而是由 Codex 或 websocket 客户端发起 upgrade。

首帧请求样例：

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

成功上游消息样例：

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

错误消息样例：

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

说明：

- 对 `previous_response_not_found` 这类 pre-commit websocket 错误，网关会改写成可触发下游 reset/重连语义的 retryable 错误
- 这是 websocket 特有行为，不适用于普通 HTTP `/v1/responses`

### `POST /v1/responses/compact`

用途：

- unary JSON 版本的 responses

请求样例：

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

成功响应样例：

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

说明：

- 这是普通 JSON 接口，不是 SSE
- 当前实现不会像 `/v1/responses` 那样为它发明 `store`
- `parallel_tool_calls` 只会在 `normalize` 模式下按模型规则补齐

### `POST /v1/memories/trace_summarize`

用途：

- 薄兼容接口
- 当前直接反代上游 `memories/trace_summarize`

请求样例：

```bash
curl -sS http://127.0.0.1:18039/v1/memories/trace_summarize \
  -H 'Content-Type: application/json' \
  -d '{
    "trace": "hello"
  }' | jq
```

上游可用时的响应样例：

```json
{
  "summary": "hello"
}
```

当前上游尚未开放时的常见响应：

```json
{
  "detail": "Not Found"
}
```

说明：

- 这个接口是否可用，主要取决于上游
- 网关只负责路由和转发，不会本地实现 trace summarize

## 管理接口

### `POST /admin/accounts/import`

用途：

- 导入一个 refresh token 账号
- 重复导入按当前 refresh token 去重
- 唯一必填字段是 `refresh_token`；`label` 和 `email` 只是可选元数据

请求样例：

```bash
curl -sS http://127.0.0.1:18040/admin/accounts/import \
  -H 'Content-Type: application/json' \
  -d '{
    "refresh_token": "rt_xxx"
  }' | jq
```

创建成功响应样例：

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

重复导入响应样例：

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

非法 refresh token 响应样例：

```json
{
  "error": {
    "message": "refresh_token must not be empty"
  }
}
```

可选元数据更新样例：

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

用途：

- 查看当前活动账号池

请求：

```bash
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

响应样例：

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

说明：

- 这里只列活动账号池，不列 `trash/` 中的无效账号文件

### `POST /admin/accounts/{id}/wake`

用途：

- 清掉单个账号的 block 状态
- `auth_invalid` 不会被 `wake`

请求样例：

```bash
curl -sS -X POST \
  http://127.0.0.1:18040/admin/accounts/a5bbe57811399ad3e973551fe6ac1f48/wake | jq
```

响应样例：

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

账号不存在时：

```json
{
  "error": {
    "message": "unknown account `a5bbe57811399ad3e973551fe6ac1f48`"
  }
}
```

### `POST /admin/accounts/wake`

用途：

- 批量清掉所有可唤醒账号的 block 状态

请求样例：

```bash
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/wake | jq
```

响应样例：

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

用途：

- 从活动池里删除账号
- 对应账号文件也会被删除

请求样例：

```bash
curl -i -X DELETE \
  http://127.0.0.1:18040/admin/accounts/a5bbe57811399ad3e973551fe6ac1f48
```

成功响应：

```text
HTTP/1.1 204 No Content
```

账号不存在时：

```json
{
  "error": {
    "message": "unknown account `a5bbe57811399ad3e973551fe6ac1f48`"
  }
}
```

### `GET /admin/routing/policy`

用途：

- 查看当前路由策略

请求：

```bash
curl -sS http://127.0.0.1:18040/admin/routing/policy | jq
```

响应样例：

```json
{
  "routing_policy": "least_in_flight"
}
```

### `PUT /admin/routing/policy`

用途：

- 动态修改路由策略

支持值：

- `round_robin`
- `least_in_flight`
- `fill_first`

请求样例：

```bash
curl -sS -X PUT http://127.0.0.1:18040/admin/routing/policy \
  -H 'Content-Type: application/json' \
  -d '{
    "routing_policy": "fill_first"
  }' | jq
```

成功响应：

```json
{
  "routing_policy": "fill_first"
}
```

缺字段时：

```json
{
  "error": {
    "message": "missing routing_policy"
  }
}
```

不支持的值：

```json
{
  "error": {
    "message": "unsupported routing policy `bad`; expected one of: round_robin, least_in_flight, fill_first"
  }
}
```

## 通用错误样例

### 账号池当前没有可用账号

业务接口常见响应：

```json
{
  "error": {
    "code": "server_is_overloaded",
    "message": "No account available right now. Try again later."
  }
}
```

说明：

- 这是普通 JSON 调用方会看到的一种下游错误形状
- 不会把某个具体账号的额度恢复时间直接暴露给下游

### 上游 refresh 失败

如果失败发生在 refresh 阶段，而且不是 `/v1/responses` 的 SSE 建流前失败包装场景，通常会把上游 JSON 错误直接回给调用方。

样例：

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

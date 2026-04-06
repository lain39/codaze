[English](OPERATIONS.md) | [简体中文](OPERATIONS.zh-CN.md)

# Codaze 运维与排障文档

这份文档面向运行和维护 `Codaze` 的人，关注的是：

- 账号状态怎么看
- 常见错误怎么理解
- 什么情况下账号会进入 `trash/`
- `wake` 到底会清什么
- 哪些接口适合做巡检

开发细节见 [DEVELOPMENT.zh-CN.md](DEVELOPMENT.zh-CN.md)。

## 1. 服务端口

默认有两个本地端口：

- 业务口：`127.0.0.1:18039`
- 管理口：`127.0.0.1:18040`

业务口：

- `GET /health`
- `GET /v1/models`
- `POST /v1/responses`
- `GET /v1/responses` websocket
- `POST /v1/responses/compact`
- `POST /v1/memories/trace_summarize`

管理口：

- `POST /admin/accounts/import`
- `POST /admin/accounts/wake`
- `POST /admin/accounts/{id}/wake`
- `GET /admin/accounts`
- `DELETE /admin/accounts/{id}`
- `GET /admin/routing/policy`
- `PUT /admin/routing/policy`

## 2. 最基础的巡检

进程活着：

```bash
curl -sS http://127.0.0.1:18039/health | jq
```

当前实现的 `/health` 只返回：

```json
{ "ok": true }
```

它的语义很窄：

- 只表示本地 HTTP 服务在响应
- 不表示上游可用
- 不表示账号池里一定有可用账号
- 不表示 refresh 或 websocket 当前正常

看账号池：

```bash
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

看当前路由策略：

```bash
curl -sS http://127.0.0.1:18040/admin/routing/policy | jq
```

## 3. 账号视图字段

`GET /admin/accounts` 返回的每个账号，重点看这些字段：

- `id`
  - 本地账号 ID
  - 当前实现是由 refresh token 稳定导出的短 hash
- `label`
  - 可选的人类标签
- `email`
  - 已知邮箱，通常在 refresh 成功后补齐
- `routing_state`
  - 当前路由状态
- `blocked_reason`
  - 为什么本地暂时不该再选它
- `blocked_source`
  - block 时间是来自上游 `retry-after`，还是本地退避策略，还是固定策略
- `blocked_until`
  - 本地调度层面的“最早再试时间”
- `refresh_in_flight`
  - 当前是否正有 refresh 在进行
- `in_flight_requests`
  - 当前正在跑的请求数
- `access_token_expires_at`
  - 当前缓存 access token 的过期时间
- `last_refresh_at`
  - 最近一次 refresh 成功时间
- `last_success_at`
  - 最近一次成功请求时间
- `last_error_at`
  - 最近一次失败记录时间
- `last_error`
  - 最近一次失败细节

## 4. routing_state 含义

### `cold`

含义：

- 当前没有可用 access token
- 需要在真正路由到它时 refresh

常见来源：

- 刚导入
- access token 被判定失效后被清空
- block 过期后回到未刷新态

### `warming`

含义：

- 正在 refresh
- 或刚被选中、处于建链中

这一般是短暂状态。

### `ready`

含义：

- 有 access token
- 没有有效 block
- 可参与路由

### `cooldown`

含义：

- 账号暂时不应参与路由
- 通常是 rate limit 或 quota exhausted

恢复方式：

- 等 `blocked_until`
- 或手工 `wake`

### `risk_controlled`

含义：

- 账号触发了风控类问题
- 比普通临时失败更严重

当前固定冷却：

- 30 分钟

### `temporarily_unavailable`

含义：

- 暂时不可用
- 可能是上游 5xx、网络异常、access token 被拒、websocket 临时异常等

### `auth_invalid`

含义：

- refresh token 已被判定为无效
- 这是最接近“账号实体不可用”的状态

这个状态不会被 `wake` 清掉。

## 5. blocked_reason / blocked_source

### `blocked_reason`

当前主要有：

- `rate_limited`
- `quota_exhausted`
- `risk_controlled`
- `temporarily_unavailable`
- `auth_invalid`

### `blocked_source`

当前主要有：

- `upstream_retry_after`
  - 上游给了明确 `retry-after` 或等价 `resets_*`
- `local_backoff`
  - 上游没给具体时间，网关自己做指数退避
- `fixed_policy`
  - 本地固定策略，比如风控 30 分钟、临时失败 60 秒

## 6. `blocked_until` 怎么理解

`blocked_until` 是本地调度提示，不是上游官方额度时间的权威镜像。

它的来源有三种：

- 上游 `retry-after`
- 上游结构化错误里的 `resets_at` / `resets_in_seconds`
- 网关本地退避或固定冷却

因此：

- 它适合用于“当前什么时候再试”
- 不适合当成账号真实额度重置时间的权威来源

## 7. 冷却时间规则

当前实现里几个重要值：

- 临时失败固定冷却：60 秒
- 风控固定冷却：30 分钟
- 本地指数退避：
  - 从 1 秒开始
  - 每次翻倍
  - 上限 30 分钟

本地指数退避用于这类场景：

- rate limit 但上游没给具体 `retry-after`
- quota exhausted 但上游没给具体 `retry-after`

## 8. 常见失败如何落到账号状态

### access token 被拒

典型结果：

- 清掉当前 access token
- 进入 `temporarily_unavailable`
- 默认冷却 60 秒

这不进 `trash/`。

原因：

- access token 失效不代表 refresh token 失效

### refresh token 无效

典型结果：

- 账号进入 `auth_invalid`
- 对应账号文件移到 `trash/`
- 记录会从活动池中脱离

这是唯一会自动进 `trash/` 的硬失效路径。

### rate limit

典型结果：

- `routing_state = cooldown`
- `blocked_reason = rate_limited`
- 如果上游给时间，就用上游时间
- 否则走本地指数退避

### usage limit / quota exhausted

典型结果：

- `routing_state = cooldown`
- `blocked_reason = quota_exhausted`
- 如果上游带 `resets_at` / `resets_in_seconds` / `retry-after`，就写进 `blocked_until`
- 否则走本地指数退避

### unusual activity / arkose / turnstile

典型结果：

- `routing_state = risk_controlled`
- `blocked_reason = risk_controlled`
- 固定冷却 30 分钟

### 5xx / timeout / network error

典型结果：

- `routing_state = temporarily_unavailable`
- 通常是短期 block

### 请求体本身非法

例如：

- `invalid_prompt`
- `context_length_exceeded`
- 其他 request rejected

这类错误通常：

- 会记录 `last_error`
- 但不会把账号判死

## 9. `trash/` 的语义

`trash/` 只用于放已经确认无效的 refresh token 账号文件。

当前语义：

- access token 过期：不进 `trash/`
- 请求被限流：不进 `trash/`
- 配额用完：不进 `trash/`
- 风控：不进 `trash/`
- 网络异常：不进 `trash/`
- refresh token 明确无效：进 `trash/`

也就是说，`trash/` 不是“失败账号回收站”，而是“refresh token 硬失效回收站”。

## 10. `wake` 的作用边界

全量唤醒：

```bash
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/wake | jq
```

唤醒单个：

```bash
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/<id>/wake | jq
```

`wake` 会做的事：

- 清掉 `blocked_reason`
- 清掉 `blocked_source`
- 清掉 `blocked_until`
- 清零本地 backoff level
- 如果账号已有 access token，则恢复成 `ready`
- 如果没有 access token，则恢复成 `cold`
- 如果 refresh 还在进行，则保持 `warming`

`wake` 不会做的事：

- 不会恢复 `auth_invalid`
- 不会把 `trash/` 里的账号搬回来
- 不会立即 refresh
- 不会清空账号文件问题

## 11. 客户端看到的错误，和 admin 看到的错误，不是同一层

这是刻意设计。

对客户端：

- 尽量返回统一、可重试、不会泄露单个账号细节的错误
- 例如账号池整体无可用账号时，客户端看到的是通用的：
  - `No account available right now. Try again later.`

对 admin：

- `last_error` 会尽量保留真实失败原因
- 用于判断到底是 rate limit、quota、风控、refresh token 失效，还是 websocket 上游异常

所以：

- 客户端错误是面向“调用面”的
- admin 错误是面向“运维排障”的

不要期待两者完全一致。

## 12. 客户端断开时会发生什么

当前实现中：

- SSE 流在下游连接 drop 时，会释放对应账号占用
- websocket 代理在客户端断开或升级失败时，也会关闭上游 websocket 并结算账号状态

这意味着：

- 正常的客户端中断不会长期占住 `in_flight_requests`
- 账号会被释放回路由池

但如果你怀疑有异常占用，还是应该直接看：

```bash
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

重点观察：

- `in_flight_requests`
- `refresh_in_flight`
- `last_error`

## 13. 优雅停机的边界

收到 `SIGINT` / `SIGTERM` 时：

- public listener 停止接新请求
- admin listener 停止接新请求
- 账号目录重扫任务退出

当前没有额外的“停机时统一落盘运行态”阶段。

这是刻意设计：

- 账号实体变更平时就即时写盘
- `blocked_*`、`last_error` 这类热状态本来就只打算保存在内存

## 14. 常见排障路径

### 看起来服务活着，但 Codex 一直失败

先看：

```bash
curl -sS http://127.0.0.1:18039/health | jq
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

如果 `/health` 正常但账号都在 block：

- 看 `blocked_reason`
- 看 `blocked_until`
- 看 `last_error`

### 某个账号反复失败

先看它的：

- `routing_state`
- `blocked_reason`
- `blocked_source`
- `last_error`

再判断：

- access token 问题：通常等 refresh 或再选中
- refresh token 失效：会进 `trash/`
- rate limit / quota：等 `blocked_until` 或手工 `wake`
- 风控：通常不要频繁强行 `wake`

### 全部账号都不可用

客户端通常只会看到统一错误，不会告诉你具体是哪一个账号的额度时间或风控细节。

这时应该以 `/admin/accounts` 为准，而不是以客户端表面的统一错误为准。

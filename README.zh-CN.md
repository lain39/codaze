[English](README.md) | [简体中文](README.zh-CN.md)

<div align="center">
  <h1>Codaze☆</h1>
  <p><strong>把多个 ChatGPT 账号汇聚成一个尽量贴近官方 Codex 客户端行为的本地网关。</strong></p>
</div>

<p align="center">
  <a href="https://github.com/lain39/codaze/releases"><img src="https://img.shields.io/github/v/release/lain39/codaze?style=flat-square&label=release&sort=semver" alt="Release" /></a>
  <a href="https://github.com/lain39/codaze/actions/workflows/release.yml"><img src="https://img.shields.io/github/actions/workflow/status/lain39/codaze/release.yml?style=flat-square&label=release%20pipeline" alt="Release Pipeline" /></a>
  <a href="https://github.com/lain39/codaze/blob/main/LICENSE"><img src="https://img.shields.io/github/license/lain39/codaze?style=flat-square" alt="License" /></a>
  <a href="https://github.com/lain39/codaze/releases"><img src="https://img.shields.io/badge/support-linux%20%7C%20macOS%20%7C%20windows%20%C2%B7%20amd64%20%7C%20arm64-0A7EA4?style=flat-square" alt="Support Matrix" /></a>
  <a href="docs/DESIGN.zh-CN.md"><img src="https://img.shields.io/badge/docs-DESIGN.zh--CN.md-1F6FEB?style=flat-square" alt="Design Docs" /></a>
</p>

文档索引：[docs/README.zh-CN.md](docs/README.zh-CN.md)  

---

## 为什么选择 Codaze

- **高贴近指纹 (Fingerprint Alignment)**： 复用 Codex 原生的 Rust 传输栈。出站网络行为、HTTP 头和 Websocket 特征尽量贴近官方客户端，同时明确保留轻量网关的设计边界。
- **故障切换与协议整形 (Failover & Protocol Surgery)**： 在账号限流或特定失败场景下切换账号，并对部分关键错误做协议层改写，例如把 `previous_response_not_found` 转成可触发客户端 reset 的错误，引导上游客户端平滑重置。
- **惰性刷新 (Lazy Initialization)**： 账号按需刷新，避免启动时批量请求暴露特征。
- **物理隔离与零配置 (Zero-Bloat & Secure)**： 没有复杂的 YAML，以目录结构作为天然数据库；业务流与管理 API 双端口物理隔离，且强制绑定本地回环。

## 极速启动

从 [GitHub Releases](https://github.com/lain39/codaze/releases) 下载对应平台的 `codaze` 二进制后直接运行（开箱即用）：

```bash
./codaze
```

> [!NOTE]
> 当前发布的 Linux 二进制是 `x86_64-unknown-linux-gnu` 和 `aarch64-unknown-linux-gnu`，面向较新的 `glibc` 系发行版。
> 不保证覆盖所有 Linux 发行版；`musl` 环境（例如 Alpine）请自行构建。

这会使用默认值启动：

- 业务口监听：`127.0.0.1:18039`
- 管理口监听：`127.0.0.1:18040`
- 账号目录：类 Unix 为 `$HOME/.codaze`，Windows 为 `%USERPROFILE%\\.codaze`
- Codex 版本：`0.118.0` （指定UA等地方的 Codex 版本号）
- 路由策略：`least_in_flight` （最少并发优先）
- 指纹模式：`normalize`

如果你想覆盖默认值，再显式带参数：

```bash
./codaze \
  --listen 127.0.0.1:18039 \
  --admin-listen 127.0.0.1:18040 \
  --codex-version 0.118.0 \
  --routing-policy least_in_flight \
  --fingerprint-mode normalize
```

> [!NOTE]
> 网关只允许监听 loopback 地址，不会绑定到非本地地址。
> 默认情况下，业务接口和管理接口分属两个不同的本地端口。

进程收到 `SIGINT` 或 `SIGTERM` 时会优雅停机。

如果你是项目维护者，想本地构建或用 `cargo run`，见 [docs/DEVELOPMENT.zh-CN.md](docs/DEVELOPMENT.zh-CN.md)。

## 后台服务运行

如果你希望 `codaze` 在后台长期运行，不要依赖一直挂着的 shell，会更适合用现成的服务模板：

- Linux systemd：[docs/PACKAGING.zh-CN.md](docs/PACKAGING.zh-CN.md)
- macOS launchd：[docs/PACKAGING.zh-CN.md](docs/PACKAGING.zh-CN.md)

实际模板文件在：

- [packaging/systemd/codaze.service](packaging/systemd/codaze.service)
- [packaging/launchd/io.github.lain39.codaze.plist](packaging/launchd/io.github.lain39.codaze.plist)

## 导入账号

最简单的方式是直接把账号文件放进 `--accounts-dir`。Codaze 会周期性无感重扫目录，无需重启服务。

最小账号文件示例：

```json
{
  "refresh_token": "rt_xxx"
}
```

`label` 和 `email` 都是可选元数据；不写也可以，后续可由 refresh 结果补齐。

运行机制说明：

- refresh 是惰性的，导入后不会立刻 refresh，而是在第一次真正被路由到时刷新
- 重复导入会按当前 refresh token 去重
- 非法 refresh token 会被移动到 `trash/`
- 运行时以内存中的 `refresh_token` 状态为准；不支持通过手工修改已加载文件里的 `refresh_token` 来控制运行时行为
- 不存在单独的“停机落盘运行态”阶段；账号文件的变更会在导入、refresh 成功、删除、移入 `trash/` 时即时写盘，`blocked_*`、`last_error` 这类运行态只保存在内存

如果你想在运行中 **动态导入** ，也可以走 HTTP：

```bash
curl -X POST http://127.0.0.1:18040/admin/accounts/import \
  -H 'Content-Type: application/json' \
  -d '{"refresh_token":"rt_xxx","label":"main","email":"user@example.com"}'
```

## 代理环境变量

`Codaze` 的出站客户端默认会遵循标准的进程级代理环境变量；只有在特殊的 `CODEX_SANDBOX=seatbelt` 路径下，才会显式关闭代理自动发现。

常见写法：

```bash
export HTTP_PROXY="http://127.0.0.1:3128"
export HTTPS_PROXY="http://127.0.0.1:3128"
export ALL_PROXY="socks5h://127.0.0.1:1080"
export NO_PROXY="127.0.0.1,localhost"
```

说明：

- 普通 HTTP(S) 上游代理，优先用 `HTTP_PROXY` / `HTTPS_PROXY`
- 如果你要走带远程 DNS 解析的 SOCKS5，最稳的是 `ALL_PROXY=socks5h://host:port`
- 如果你通过 systemd 或 launchd 运行 Codaze，也要把这些环境变量写进服务定义里

## 接口概览

| 模块 | 方法 | 路径 | 描述 |
| :--- | :--- | :--- | :--- |
| **业务流 (18039)** | `GET` | `/v1/models` | 获取可用模型 |
| | `POST` | `/v1/responses` | responses（ SSE ） |
| | `GET` | `/v1/responses` | responses（ Websocket ） |
| | `POST` | `/v1/responses/compact` | compact |
| | `POST` | `/v1/memories/trace_summarize` | （官方未开放） |
| | `GET` | `/health` | 网关基础健康检查 |
| **管理面 (18040)** | `GET` | `/admin/accounts` | 获取账号池状态 |
| | `POST` | `/admin/accounts/import` | 导入/更新账号 |
| | `POST` | `/admin/accounts/wake` | 唤醒所有处于冷却期的账号 |
| | `POST` | `/admin/accounts/:id/wake` | 唤醒特定冷却账号 |
| | `DELETE`| `/admin/accounts/:id` | 移除账号 |
| | `GET/PUT`| `/admin/routing/policy` | 查看/动态切换路由策略 |

> 详细入参和返回值规范，请参阅 [API 文档](docs/API.zh-CN.md) 

## Codex 兼容性

`Codaze` 刻意复用了 Codex 的 Rust 传输栈，而不是重新发明一套独立的上游客户端：

- `codex_login::default_client::build_reqwest_client`
- `codex_client::ReqwestTransport`
- Codex 风格请求头和 endpoint 路径

默认指纹模式是 `normalize`。它只会补齐那些真实 Codex 请求稳定携带的字段，例如：

- `/v1/responses` 的 `store: false`
- 按模型推导出的 `parallel_tool_calls`
- 能从 `x-codex-session-source` 明确推导出来的 identity headers，例如 `x-openai-subagent` 和 `x-codex-parent-thread-id`

`passthrough` 只影响出站请求指纹整形，不会关闭路由、refresh、错误分类或本地管理行为。

一个重要边界：

- 如果调用方本来就提供了 `x-codex-window-id`，`Codaze` 会透传
- 对非 Codex 调用方，`Codaze` 不会凭空生成 `x-codex-window-id`，也不会凭空生成 websocket `response.create.client_metadata` 里的 identity key

完整设计取舍见 [docs/DESIGN.zh-CN.md](docs/DESIGN.zh-CN.md)。

## 可选 `_gateway` 控制字段

`POST /v1/responses`、`POST /v1/responses/compact`、`POST /v1/memories/trace_summarize` 支持一个私有 `_gateway` 字段。它只在本地消费，转发前会被剥离。

示例：

```json
{
  "_gateway": {
    "session_source": "exec"
  }
}
```

`_gateway.session_source` 必须符合 `codex_protocol::SessionSource` 的 JSON 表示。

## 让 Codex 接到 Codaze

使用内建 `openai` provider：

这里的 `OPENAI_API_KEY=dummy` 不会被网关拿去调用上游；它只是为了满足 Codex 的 provider 配置校验。

```bash
OPENAI_API_KEY=dummy \
codex \
  -c 'openai_base_url="http://127.0.0.1:18039/v1"' \
  -c 'model_provider="openai"'
```

使用自定义 provider：

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

如果你用的是自定义 provider，而漏掉了 `supports_websockets = true`，Codex 会回退到 HTTP/SSE。

管理接口默认保留在 `http://127.0.0.1:18040`，不会通过业务 `/v1` 监听端口暴露出去。

## 许可证与文档

- 许可证：`Apache-2.0`
- 归属说明文件：[NOTICE](NOTICE)
- 开发和本地构建：[docs/DEVELOPMENT.zh-CN.md](docs/DEVELOPMENT.zh-CN.md)
- 发布流程：[docs/RELEASE.zh-CN.md](docs/RELEASE.zh-CN.md)
- 服务模板：[docs/PACKAGING.zh-CN.md](docs/PACKAGING.zh-CN.md)
- GitHub Releases：https://github.com/lain39/codaze/releases

项目维护相关文档：

- [CHANGELOG.md](CHANGELOG.md)
- [CONTRIBUTING.md](CONTRIBUTING.md)
- [SECURITY.md](SECURITY.md)
- [docs/README.zh-CN.md](docs/README.zh-CN.md)
- [docs/RELEASE.zh-CN.md](docs/RELEASE.zh-CN.md)

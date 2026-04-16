[English](DEVELOPMENT.md) | [简体中文](DEVELOPMENT.zh-CN.md)

# Codaze 开发文档

这份文档面向项目维护者，不重复解释产品目标，而是说明如何在本地开发、调试和扩展 `Codaze`。

设计原则见 [DESIGN.zh-CN.md](DESIGN.zh-CN.md)。
运维排障见 [OPERATIONS.zh-CN.md](OPERATIONS.zh-CN.md)。

## 1. 本地环境

建议环境：

- Rust stable
- macOS / Linux
- `cargo`
- `curl`
- `jq`

项目根目录：

```bash
cd <repo-root>
```

## 2. 常用命令

格式化：

```bash
cargo fmt --all
```

编译：

```bash
cargo build
cargo build --release
```

运行：

```bash
cargo run --release -- \
  --listen 127.0.0.1:18039 \
  --admin-listen 127.0.0.1:18040 \
  --codex-version 0.121.0 \
  --routing-policy least_in_flight \
  --fingerprint-mode normalize
```

跑测试：

```bash
cargo test -q
```

按模块或关键字跑测试：

```bash
cargo test wake -q
cargo test responses -q
cargo test classifier -q
```

开启更多日志：

```bash
RUST_LOG=debug cargo run --release -- ...
```

默认日志会压低 `codex_client::custom_ca` 的噪声。

CI / release 说明：

- 仓库自带 [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) 和 [`.github/workflows/release.yml`](../.github/workflows/release.yml)
- 默认依赖图现在是自包含的；CI 和 release 只 checkout `codaze` 本身
- 如果以后升级 Codex 参考版本，更新 [Cargo.toml](../Cargo.toml) 里固定的 Git revision 即可

## 2.1 可选的本地 Codex Override

默认情况下，`codaze` 会从 `Cargo.toml` 里固定的 Git revision 拉取 Codex Rust crate。

如果你想直接联调本地 Codex 工作区：

1. 把 `openai/codex` clone 到 `local/codex`
2. 执行 `bash scripts/use-local-codex.sh`
3. 重新编译 `codaze`

示例：

```bash
git clone https://github.com/openai/codex.git local/codex
bash scripts/use-local-codex.sh
cargo build
```

说明：

- `local/codex` 已在 `.gitignore` 中忽略，只用于本地开发
- `.cargo/config.toml` 也是本地文件，不应提交
- `.cargo/config.toml` 里的 patch 路径是相对工作区根目录解析的，所以应当写成 `local/codex/...`，而不是 `../local/codex/...`
- `scripts/use-local-codex.sh` 采用严格托管模式：如果 `.cargo/config.toml` 已存在，脚本会直接拒绝启用，不会尝试和现有配置合并
- 如果你本来就在用自定义 `.cargo/config.toml`，请先临时移走它，或者在干净工作树里启用 local Codex override
- `scripts/use-local-codex.sh` 会在 `Cargo.lock` 已有本地修改时拒绝启用，避免后续 reset 把无关的锁文件改动一起丢掉
- 本地 override 生效期间，Cargo 可能会把 `Cargo.lock` 从固定 git source 改写成按本地 path 解析后的结果
- 开 PR 或推 release 相关改动前，先执行 `bash scripts/reset-local-codex.sh`
- `scripts/reset-local-codex.sh` 只会移除受管 override，并且只在检测到本地 Codex override 污染时用本地备份恢复 `Cargo.lock`

## 3. 目录结构

`src/` 当前按责任拆分：

- `main.rs`
  - 进程入口
  - 日志初始化
  - 双端口监听
  - 优雅停机
  - 账号目录重扫后台任务
- `config.rs`
  - CLI 参数解析
  - 默认值
  - loopback 监听约束
- `app/`
  - 本地 HTTP 路由入口
  - `api.rs` 负责业务 `/v1/*`
  - `admin.rs` 负责 `/admin/*`
- `accounts/`
  - 账号数据模型
  - 账号文件读写
  - 路由候选选择
  - block / wake / trash / refresh 后状态迁移
- `upstream/`
  - 上游 HTTP / websocket / refresh 调用
  - 上游请求头拼装
  - 请求发送与响应接收
- `responses/`
  - `/v1/responses` 相关逻辑
  - SSE 流包装
  - pre-stream failure 合成
  - websocket responses 代理
- `classifier.rs`
  - 上游失败分类
- `failover.rs`
  - 同一次请求内的账号切换和重试编排
- `router.rs`
  - 路由策略枚举和选择入口
- `request_normalization.rs`
  - 指纹整形
  - `store: false`
  - `parallel_tool_calls`
  - `_gateway` 私有字段消费
- `gateway_errors.rs`
  - 对下游暴露的网关级错误
- `error_semantics.rs`
  - 错误语义辅助

## 4. 运行时模型

服务有两组本地端口：

- 业务口：默认 `127.0.0.1:18039`
- 管理口：默认 `127.0.0.1:18040`

业务口暴露：

- `GET /health`
- `GET /v1/models`
- `POST /v1/responses`
- `GET /v1/responses` websocket upgrade
- `POST /v1/responses/compact`
- `POST /v1/memories/trace_summarize`

管理口暴露：

- `POST /admin/accounts/import`
- `POST /admin/accounts/wake`
- `POST /admin/accounts/{id}/wake`
- `GET /admin/accounts`
- `DELETE /admin/accounts/{id}`
- `GET /admin/routing/policy`
- `PUT /admin/routing/policy`

## 5. 账号数据和持久化

当前持久化边界很刻意：

- 账号文件会持久化
- 运行态不会单独落盘

会即时写盘的动作：

- HTTP 导入账号
- refresh 成功后 refresh token / metadata 更新
- 删除账号
- refresh token 确认无效后移入 `trash/`

只保存在内存的运行态：

- `blocked_until`
- `blocked_source`
- `blocked_reason`
- `last_error`
- `in_flight_requests`
- 各种热路径的选择状态

不要在运行中手工改已加载账号文件里的 `refresh_token`。当前语义是“运行态以内存为准，磁盘主要用于冷启动和持久化账号实体”。

## 6. 请求流

典型 `/v1/responses` 流程：

1. 下游请求进入 `app/api.rs`
2. `request_normalization.rs` 处理 `store: false`、`parallel_tool_calls` 和 `_gateway`
3. `failover.rs` 选择账号并执行一次尝试
4. `upstream/` 发起 HTTP 或 websocket 请求
5. `responses/` 把上游结果包装成下游需要的 SSE / WS 形态
6. 成功或失败后回写账号运行态

需要特别注意的行为：

- `normalize` 模式下，请求归一化还会把缺失或为 `null` 的 `instructions` 统一补成 `""`
- 路由级失败整形按接口和 response shape 选择：`/v1/models`、`/v1/responses/compact` 始终走 unary JSON，`/v1/responses` 则有自己的建流前失败规则
- 在 `/v1/responses` 上，Codex 调用方会在建流前收到 synthetic SSE，其他调用方拿到的是普通 HTTP JSON 错误
- 公开业务接口现在会把账号路由相关失败统一收口成网关级不可用；只有“请求本身非法”这类 caller error 还保留相对直接的下游语义
- `previous_response_not_found` 会按现有约定转换成可触发下游 reset 的失败路径
- `passthrough` 只是不做出站指纹整形，不影响本地路由和账号管理

## 7. 指纹约束

本项目目标不是“像 OpenAI API”，而是“尽量像 Codex”。

修改出站行为时优先检查：

- 真实 Codex 是否会发送这个 header
- 真实 Codex 是否会发送这个 body 字段
- 这是稳定指纹，还是某个环境特有字段

当前实践：

- 优先复用 Codex Rust crate 的 transport / login / protocol
- 不凭空生成 `x-codex-window-id`
- 不凭空生成那些依赖下游客户端本地线程状态的 websocket `client_metadata` identity key
- `x-codex-installation-id` 单独处理：`normalize` 模式下，它按当前选中的上游账号派生；如果 websocket 在 pre-commit 阶段 failover 重放，也允许按新上游连接重写
- 只补真实 Codex 稳定存在的字段，如 `store: false`

如果某个兼容行为只是为了“兼容旧调用方”而不是为了“贴近 Codex”，默认不应加入。

## 8. 调试建议

看服务端日志：

```bash
RUST_LOG=info cargo run --release -- ...
RUST_LOG=debug cargo run --release -- ...
```

导入账号：

```bash
curl -sS http://127.0.0.1:18040/admin/accounts/import \
  -H 'Content-Type: application/json' \
  -d '{"refresh_token":"rt_xxx"}' | jq
```

查看账号：

```bash
curl -sS http://127.0.0.1:18040/admin/accounts | jq
```

唤醒被本地 block 的账号：

```bash
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/wake | jq
curl -sS -X POST http://127.0.0.1:18040/admin/accounts/<id>/wake | jq
```

直接打业务接口：

```bash
curl -N http://127.0.0.1:18039/v1/responses \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "gpt-5.4",
    "stream": true,
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

用 Codex 连本地：

```bash
OPENAI_API_KEY=dummy \
codex \
  -c 'openai_base_url="http://127.0.0.1:18039/v1"' \
  -c 'model_provider="openai"'
```

如果你使用自定义 provider，确认 `supports_websockets = true`，否则 Codex 会回退到 SSE。

## 9. 改代码时的最小检查清单

改动完成前，至少检查这些：

- `cargo fmt --all`
- `cargo test -q` 或至少跑受影响模块测试
- `cargo build --release`
- 至少验证一个正常路径
- 至少验证一个失败分类路径

如果改了这些区域，还要额外确认：

- 改 `request_normalization.rs`
  - 确认没有发明 Codex 不会发的字段
- 改 `upstream/headers.rs`
  - 确认 header 名称、出现条件和大小写语义没有偏离 Codex
- 改 `responses/`
  - 确认 SSE / websocket 下游消费面没有变化
- 改 `classifier.rs`
  - 确认账号 block / cooldown / trash 迁移还符合现有策略

## 10. 不要做的事

- 不要把项目扩展成多上游通用代理
- 不要绕开 Codex Rust transport 另写一套上游客户端
- 不要把账号运行态扩展成单独的状态文件系统
- 不要为了兼容未知客户端而凭空增加“猜测型字段”
- 不要在没有明确依据时更改 Codex 指纹

## 11. 后续补文档时的放置建议

建议保持：

- `README*.md`
  - 面向使用者
- `docs/DESIGN.zh-CN.md`
  - 面向设计原则和取舍
- `docs/DEVELOPMENT.zh-CN.md`
  - 面向维护者和开发流程

不要把开发流程、设计备忘录、对外使用说明混在一个文件里。

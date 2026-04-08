[English](FAQ.md) | [简体中文](FAQ.zh-CN.md)

# FAQ

## 为什么非 Codex 调用方必须显式带 `stream: true`

`Codaze` 当前的 `POST /v1/responses` 兼容路径，本质上是在对接现有的 Codex upstream Responses 流式路径。

对非 Codex 调用方，网关会做一小组请求归一化，例如删除少量当前上游明确拒绝的字段；但它不会擅自把一个普通 JSON 请求改造成流式请求语义。

因此，如果非 Codex 调用方直接请求 `POST /v1/responses`，应显式带上：

```json
{
  "stream": true
}
```

否则当前常见返回是：

```json
{
  "detail": "Stream must be set to true"
}
```

这不是 `Codaze` 自己随意附加的限制，而是当前这条上游路径的行为边界。

相关文档：

- [API.zh-CN.md](API.zh-CN.md)

## 为什么某些 `400` 不会自动切账号

`Codaze` 并不是看到所有失败都切账号。

自动 failover 主要针对“换一个账号可能会好”的失败类型，例如：

- access token 被拒
- refresh / auth 失效
- rate limit
- quota exhausted
- 风控
- 临时性网络或上游故障

但如果某个错误被判定为“请求本身有问题”，它就会被归类为 `RequestRejected`，不会自动切到下一个账号。因为这类问题换账号通常也不会消失。

典型例子：

- 请求体缺少当前路径要求的 `stream: true`
- `invalid_prompt`
- `context_length_exceeded`
- 其他明显的请求参数或请求形状错误

一个重要边界：

- 不是所有 `400` 都一刀切视为不可 failover
- 某些有明确语义的错误会被特殊处理，例如 websocket 的 `previous_response_not_found` 会被改写成更适合触发下游 reset / 重连的错误形状

如果你看到的是“某个 `400` 没有切账号”，更准确的判断方式不是只看状态码，而是看它在语义上是不是“请求被拒绝”。

相关文档：

- [API.zh-CN.md](API.zh-CN.md)
- [OPERATIONS.zh-CN.md](OPERATIONS.zh-CN.md)
- [DESIGN.zh-CN.md](DESIGN.zh-CN.md)

## Cherry Studio App 的模型健康检查为什么会返回 `400`

这个问题本质上就是上一条和第一条叠加出来的结果。

Cherry Studio App 的模型健康是直接发一次真实的模型探测请求。它的这条探测链路默认会走非流式检查；而 `Codaze` 当前对非 Codex 调用方的 `POST /v1/responses` 路径要求显式带 `stream: true`。

所以当 Cherry Studio App 用这条非流式健康检查去探测 `Codaze` 时，常见结果就是：

```json
{
  "detail": "Stream must be set to true"
}
```

## 为什么 Codex 在自定义 provider 下通常不会请求 `codaze` 的 `/v1/models`

这基本不会影响 `Codaze` 的实际使用。

当前 Codex 里的模型目录刷新逻辑，对自定义 provider 有额外条件：通常只有走 ChatGPT auth，或者显式配置了 `[model_providers.<id>.auth]` 这类 command auth，Codex 才会主动去拉远端 `/models`。

因此，如果你只是把 Codex 的 `Responses` 请求转到 `Codaze`，但没有给这个自定义 provider 配 `auth`，常见结果是：

- `Responses` 请求仍然可以正常走
- 但 Codex 不会主动请求 `codaze` 的 `/v1/models`
- 模型列表更多依赖本地/缓存侧，而不是从 `codaze` 动态拉取

如果你的目标是“让 Codex 在自定义 provider 下也主动使用 `/v1/models`”，可以用类似方式给该 provider 增加 command auth 来触发这条路径。

但这不属于 `Codaze` 主要解决的问题。`Codaze` 的定位是做 Codex 请求转发、账号管理和 failover，而不是去扩展 Codex 自定义 provider 的模型目录机制，所以当前不会专门为这件事增加额外实现。

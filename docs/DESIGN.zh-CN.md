[English](DESIGN.md) | [简体中文](DESIGN.zh-CN.md)

# Codaze 设计备忘录

这份文档只记录设计原则、边界和取舍，不承担快速上手说明的职责。运行方法、接口示例和配置入口继续以 README 为准。

## 1. 产品定位

`Codaze` 是一个本地运行的、面向 `chatgpt.com/backend-api/codex` 的多账号网关。

它的目标不是做一个“通用 LLM 网关”，而是做一个尽量贴近 Codex 客户端行为的 OpenAI/Codex 专用中继层。

更具体地说，这个项目要解决的是三件事：

- 用本地统一入口承接 Codex 风格请求
- 在多个 ChatGPT 账号之间做账号池路由和恢复
- 尽量保持出站请求路径、头部和传输栈接近真实 Codex

## 2. 非目标

下面这些事情都不是当前项目的目标：

- 多 provider 抽象
- Claude / Gemini / Amp / 自定义协议矩阵
- 把网关做成远程多租户 SaaS
- 在网关里重建一整套 Codex session runtime
- 在网关里替代 Codex 的本地线程、compact、resume、fork 状态机

项目保持“Codex 专用”和“本地优先”是刻意选择，不是阶段性缺失。

## 3. 总体结构

项目结构上只有三个核心面：

- 下游入口
  - 提供 `/v1/models`、`/v1/responses`、`/v1/responses/compact`、`/v1/memories/trace_summarize`
  - 承接本地客户端的 HTTP / websocket 请求
- 路由与账号池
  - 管理 refresh token 持久化
  - 管理 access token 刷新、账号状态、冷却和恢复
  - 为每次请求选择一个当前可用账号
- 上游出站
  - 用 Codex 的 Rust transport 和请求构造能力访问 `chatgpt.com/backend-api/codex`

因此，这个项目不是“业务逻辑很多的应用层服务”，而是一个刻意收窄能力边界的代理层。

## 4. 为什么必须贴近 Codex

`Codaze` 不把“能通”视为充分条件，而把“尽量像真实 Codex”视为更重要的约束。

原因有三个：

- 上游风控、能力开关和行为细节越来越依赖真实客户端路径
- 越贴近 Codex，后续随上游变化做同步时越容易判断
- 如果网关自己发明太多行为，问题会变成“是上游改了，还是我们自己偏了”

所以项目有几个硬约束：

- 出站 HTTP client 复用 Codex Rust 栈，不重写另一套独立 transport
- 优先复用 Codex 的 endpoint 路径和请求头约定
- 不随意补不存在于真实 Codex 的指纹字段
- 对无法轻量维护的客户端内部状态，不在网关里瞎猜

## 5. 指纹策略

项目支持两种出站指纹模式：

- `normalize`
- `passthrough`

### 5.1 normalize

`normalize` 是默认模式，含义是：

- 把下游请求整形成“更接近 Codex”的形状
- 但只补那些真实 Codex 本来就稳定携带的字段

它不是“看到缺字段就补”，而是有限整形。

当前属于可整形范围的典型例子：

- `/v1/responses` 的 `store: false`
- `parallel_tool_calls`
- 根据 `x-codex-session-source` 推导 `x-openai-subagent`
- 根据 `x-codex-session-source` 推导 `x-codex-parent-thread-id`

### 5.2 passthrough

`passthrough` 的含义是：

- 下游客户端的请求指纹尽量原样往上游带
- 只减少“为了模拟 Codex 而额外整形”的部分

它不意味着：

- 停用账号池
- 停用 refresh
- 停用错误分类
- 停用响应兼容性改写
- 停用本地 admin 行为

也就是说，`passthrough` 只作用于“请求指纹整形”，不是让整个网关变成无脑 TCP 隧道。

## 6. 哪些东西不能凭空捏造

这是整个项目最重要的设计边界之一。

### 6.1 `x-codex-window-id`

最新 Codex 会发送 `x-codex-window-id`，但它不是静态值，而是：

`conversation_id:generation`

其中 generation 会随着这些行为变化：

- 新会话：`0`
- compact 后递增
- resume 继承
- fork 重置

这说明它依赖 Codex 客户端内部线程状态，而不是单条请求本身。

因此当前原则是：

- 如果下游是真实 Codex，并且它自己带了这个头，网关透传
- 如果下游不是 Codex，网关不为它凭空合成

### 6.2 websocket `response.create.client_metadata`

Codex websocket `response.create` 里的 `client_metadata` 现在也会带 identity 信息，比如：

- `x-codex-window-id`
- `x-openai-subagent`
- `x-codex-parent-thread-id`
- `x-codex-turn-metadata`

这里的原则和上面一样：

- 有真实来源时透传
- 需要依赖客户端内部 session 状态的值，不在网关里瞎编

## 7. 为什么不在网关里重建完整会话状态机

从技术上说，网关当然可以维护更多状态；但这会明显改变项目性质。

一旦要完整生成 `x-codex-window-id`，至少要在网关里知道：

- 当前逻辑线程是谁
- 哪次 compact 真正成功了
- 什么时候是 resume
- 什么时候是 fork
- 哪些请求属于同一条线程

这会把项目从“轻量 Codex 网关”推向“半个 Codex runtime”。

这个方向目前不成立，原因是：

- 状态复杂度显著升高
- 与 Codex 客户端本地状态重复
- 更容易和真实 Codex 语义漂移
- 维护成本和收益不成比例

所以当前的选择是：宁可承认边界，也不为了表面一致去造一套容易错的状态机。

## 8. 账号系统设计

账号系统以 refresh token 为中心，不以 access token 为中心。

核心原则：

- refresh token 是持久化根凭据
- access token 是运行时缓存
- access token 失效不代表账号失效
- refresh token 确认无效时，账号才进入 `trash/`

当前形态：

- 每个账号一个 JSON 文件
- 网关启动和运行中都可以从账号目录重扫
- HTTP 导入和手工文件放入两条路径都支持

### 8.1 为什么状态只保存在内存

账号运行态不单独落盘，原因是：

- 状态文件会显著增加复杂度
- 很多状态只是调度提示，不是长期事实
- 进程重启后允许重新惰性探测

当前只有账号文件持久化，运行态仍以内存为准。

### 8.2 为什么 refresh 是惰性的

项目刻意没有采用“导入后立刻 refresh 一次”的策略。

原因：

- 简化主流程
- 降低无意义 refresh 次数
- 让“路由到账号时再统一刷新”成为唯一入口
- 让错误分类和账号状态更新集中在真正请求路径上

## 9. 路由设计

路由的目标不是“固定把一个客户端绑定到一个账号”，而是：

- 尽量选当前可用账号
- 在额度和风控约束下维持整体可用性
- 给不同账号提供基本均衡

当前支持的策略：

- `round_robin`
- `least_in_flight`
- `fill_first`

其中 `fill_first` 的语义是：

- 优先把流量灌到第一个可用账号
- 它是额度消耗策略，不是 session stickiness

项目不把“HTTP 层会话粘性”作为默认设计，因为那会和账号池的均衡目标冲突。

## 10. 错误分类与恢复

上游错误不会被当成一团原始文本直接传递和处理，而是先分类，再决定：

- 账号是否冷却
- 是否需要切换账号
- 是否可以懒恢复
- 是否要永久失效

大方向上：

- access token 失效：刷新，不进入 `trash/`
- refresh token 确认无效：移入 `trash/`
- rate limit / usage limit：设置 `blocked_until`
- 风控：长时间冷却
- 临时上游失败：本地 backoff，不永久禁用账号

一个重要原则是：

`blocked_until` 是本地调度提示，不是上游官方额度时间戳的权威副本。

## 11. websocket 兼容性策略

当前 websocket 侧有一条刻意的兼容性改写：

- 如果上游返回 `previous_response_not_found`
- 网关会把它改写成 `websocket_connection_limit_reached`

原因不是为了掩盖错误，而是为了触发 Codex 已有的 reset/retry 路径，使它在下一次 `response.create` 时丢掉 `previous_response_id`，改发完整请求。

这条改写是当前项目里少数“明确为兼容 Codex 行为而改写错误形状”的地方。

它属于有意设计，不是临时修补。

## 12. `/models` 的设计选择

项目没有把 `/models` 做成账号池推断结果，也没有额外拼装复杂状态。

当前原则是：

- `/models` 和其他上游接口一样走正常上游路径
- 不为了“看起来聪明”去对其做特殊缓存或特殊聚合

原因：

- 这个接口请求频率通常不高
- 上游本来就知道哪些模型可见
- 过度本地化会制造更多偏差点

## 13. 本地优先与安全边界

当前项目有意只支持 loopback 监听，并默认不对 admin 接口再做 token 鉴权。

为了降低“别人只想反代业务端口，却不小心把 `/admin/*` 也带出去”的风险，业务接口和管理接口默认分属两个不同的 loopback 端口。

这不是因为 admin 不敏感，而是因为当前产品形态明确是“本地工具”。

如果未来真的要变成远程服务，安全模型需要重新设计，不能直接沿用现在这套默认值。

## 14. 后续维护原则

后续如果继续迭代，建议坚持下面几条：

- 优先对照 Codex 源码和真实流量，而不是凭印象补字段
- 能透传就透传，不能轻量维护的状态不要假装能维护
- 让错误分类逻辑集中，而不是散落在 transport 细节里
- 让 README 保持上手导向，把设计原则收敛在单独文档中
- 如果某个行为是“为了兼容 Codex 的既有客户端路径”而做的改写，要在代码和文档里明确写出来

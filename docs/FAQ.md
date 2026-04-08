[English](FAQ.md) | [简体中文](FAQ.zh-CN.md)

# FAQ

## Why must non-Codex callers explicitly send `stream: true`

Codaze's current `POST /v1/responses` compatibility path is effectively aligned to the existing Codex upstream streaming Responses path.

For non-Codex callers, the gateway applies a small set of request normalizations, such as stripping a few fields that the current upstream explicitly rejects. What it does not do is silently change an ordinary JSON request into streaming request semantics.

So if a non-Codex caller directly uses `POST /v1/responses`, it should explicitly send:

```json
{
  "stream": true
}
```

Otherwise, a common current response is:

```json
{
  "detail": "Stream must be set to true"
}
```

This is not an arbitrary Codaze-only rule; it reflects the behavior of the current upstream path.

Related docs:

- [API.md](API.md)

## Why do some `400` errors not trigger automatic account failover

Codaze does not switch accounts for every failure.

Automatic failover is primarily for failures where trying another account may help, for example:

- access token rejected
- refresh or auth invalid
- rate limit
- quota exhausted
- risk control
- temporary network or upstream failure

But when an error is classified as a request-shape problem, it is treated as `RequestRejected` and does not automatically fail over to the next account. In those cases, switching accounts usually would not fix anything.

Typical examples:

- missing `stream: true` on a path that currently requires it
- `invalid_prompt`
- `context_length_exceeded`
- other obvious request-parameter or request-shape errors

One important boundary:

- not every `400` is treated the same way
- some errors have special handling; for example, websocket `previous_response_not_found` is rewritten into a shape that better triggers downstream reset or reconnect behavior

So when you see a `400` that did not switch accounts, the right question is not only "was it a 400", but "was it semantically a request rejection".

Related docs:

- [API.md](API.md)
- [OPERATIONS.md](OPERATIONS.md)
- [DESIGN.md](DESIGN.md)

## Why does Cherry Studio App model health check return `400`

This is essentially the combination of the first two questions above.

Cherry Studio App's model health check  sends a real model probe request. That probe path defaults to a non-streaming check, while Codaze's current `POST /v1/responses` path for non-Codex callers requires an explicit `stream: true`.

So when Cherry Studio App uses that non-streaming health check against Codaze, a common result is:

```json
{
  "detail": "Stream must be set to true"
}
```

## Why does Codex usually not request `codaze`'s `/v1/models` when using a custom provider

In practice, this has little to no impact on normal Codaze usage.

The current Codex model-catalog refresh logic applies an extra gate for custom providers: in practice, Codex usually only fetches remote `/models` when it is using ChatGPT auth or when the provider explicitly configures command-backed auth under `[model_providers.<id>.auth]`.

So if you only point Codex's `Responses` traffic at Codaze but do not configure provider auth for that custom provider, the common result is:

- `Responses` requests still work normally
- but Codex does not proactively request `codaze`'s `/v1/models`
- and the model list relies more on the local or cached side instead of being dynamically fetched from `codaze`

If your goal is to make Codex actively use `/v1/models` under a custom provider, you can trigger that path with a similar command-auth setup on the provider.

That said, this is outside Codaze's main scope. Codaze is meant to handle Codex request forwarding, account management, and failover, not to extend Codex's custom-provider model-catalog mechanism, so there is no plan to add special support for this in Codaze itself.

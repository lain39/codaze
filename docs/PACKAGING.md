[English](PACKAGING.md) | [简体中文](PACKAGING.zh-CN.md)

# Packaging Templates

This document describes the deployment-oriented templates for running `codaze` as a background service.

Template files:

- [../packaging/systemd/codaze.service](../packaging/systemd/codaze.service)
  - systemd unit template for Linux
- [../packaging/launchd/io.github.lain39.codaze.plist](../packaging/launchd/io.github.lain39.codaze.plist)
  - launchd plist template for macOS

These files are templates, not drop-in artifacts. You are expected to edit at least:

- the `codaze` binary path
- optional flags such as `--listen`, `--admin-listen`, or `--codex-version`
- the `HOME` environment when your service manager does not provide a usable one
- proxy environment variables such as `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and `NO_PROXY` when you need upstream proxying

## systemd

Typical install path:

- system-wide: `/etc/systemd/system/codaze.service`
- user service: `~/.config/systemd/user/codaze.service`

Basic flow:

```bash
sudo cp packaging/systemd/codaze.service /etc/systemd/system/codaze.service
sudo systemctl daemon-reload
sudo systemctl enable --now codaze
sudo systemctl status codaze
```

If you use the user-service form:

```bash
mkdir -p ~/.config/systemd/user
cp packaging/systemd/codaze.service ~/.config/systemd/user/codaze.service
systemctl --user daemon-reload
systemctl --user enable --now codaze
systemctl --user status codaze
```

If you need upstream proxying, add environment lines such as:

```ini
Environment=HTTP_PROXY=http://127.0.0.1:3128
Environment=HTTPS_PROXY=http://127.0.0.1:3128
Environment=ALL_PROXY=socks5h://127.0.0.1:1080
Environment=NO_PROXY=127.0.0.1,localhost
```

## launchd

Typical install path:

- per-user agent: `~/Library/LaunchAgents/io.github.lain39.codaze.plist`

Basic flow:

```bash
mkdir -p ~/Library/LaunchAgents
cp packaging/launchd/io.github.lain39.codaze.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.lain39.codaze.plist
launchctl print gui/$(id -u)/io.github.lain39.codaze
```

To restart after editing:

```bash
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/io.github.lain39.codaze.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.lain39.codaze.plist
```

If you want to pass CLI flags to `codaze`, add them as separate entries under `ProgramArguments` instead of one joined string. For example:

```xml
<key>ProgramArguments</key>
<array>
  <string>/Users/you/.local/bin/codaze</string>
  <string>--listen</string>
  <string>127.0.0.1:18039</string>
  <string>--admin-listen</string>
  <string>127.0.0.1:18040</string>
  <string>--accounts-dir</string>
  <string>/Users/you/.codaze</string>
  <string>--codex-version</string>
  <string>0.121.0</string>
</array>
```

`codaze` does not currently depend on its current working directory for runtime files, so `WorkingDirectory` is usually unnecessary.

If you need upstream proxying, add the variables under `EnvironmentVariables`, for example:

```xml
<key>HTTP_PROXY</key>
<string>http://127.0.0.1:3128</string>
<key>HTTPS_PROXY</key>
<string>http://127.0.0.1:3128</string>
<key>ALL_PROXY</key>
<string>socks5h://127.0.0.1:1080</string>
<key>NO_PROXY</key>
<string>127.0.0.1,localhost</string>
```

## Notes

- `codaze` is loopback-only by default, so binding to non-local addresses still requires explicit CLI changes and is rejected by the binary.
- The default accounts directory depends on `HOME`, so service-manager environments should set `HOME` explicitly if that value is missing or wrong.

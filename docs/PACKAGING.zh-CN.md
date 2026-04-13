[English](PACKAGING.md) | [简体中文](PACKAGING.zh-CN.md)

# 打包与服务模板

这份文档说明如何使用仓库里的后台服务模板来运行 `codaze`。

模板文件：

- [../packaging/systemd/codaze.service](../packaging/systemd/codaze.service)
  - Linux 的 systemd unit 模板
- [../packaging/launchd/io.github.lain39.codaze.plist](../packaging/launchd/io.github.lain39.codaze.plist)
  - macOS 的 launchd plist 模板

这些文件是模板，不是可以直接原样落地的产物。至少要按你的环境修改：

- `codaze` 二进制路径
- 是否追加 `--listen`、`--admin-listen`、`--codex-version` 等参数
- 当服务管理器没有提供正确 `HOME` 时，手动设置 `HOME`
- 如果需要上游代理，把 `HTTP_PROXY`、`HTTPS_PROXY`、`ALL_PROXY`、`NO_PROXY` 等环境变量一起写进去

## systemd

常见安装位置：

- 系统级：`/etc/systemd/system/codaze.service`
- 用户级：`~/.config/systemd/user/codaze.service`

基本流程：

```bash
sudo cp packaging/systemd/codaze.service /etc/systemd/system/codaze.service
sudo systemctl daemon-reload
sudo systemctl enable --now codaze
sudo systemctl status codaze
```

如果你走用户级服务：

```bash
mkdir -p ~/.config/systemd/user
cp packaging/systemd/codaze.service ~/.config/systemd/user/codaze.service
systemctl --user daemon-reload
systemctl --user enable --now codaze
systemctl --user status codaze
```

如果你需要上游代理，可以补这样的环境变量：

```ini
Environment=HTTP_PROXY=http://127.0.0.1:3128
Environment=HTTPS_PROXY=http://127.0.0.1:3128
Environment=ALL_PROXY=socks5h://127.0.0.1:1080
Environment=NO_PROXY=127.0.0.1,localhost
```

## launchd

常见安装位置：

- 当前用户：`~/Library/LaunchAgents/io.github.lain39.codaze.plist`

基本流程：

```bash
mkdir -p ~/Library/LaunchAgents
cp packaging/launchd/io.github.lain39.codaze.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.lain39.codaze.plist
launchctl print gui/$(id -u)/io.github.lain39.codaze
```

修改后重启：

```bash
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/io.github.lain39.codaze.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.github.lain39.codaze.plist
```

如果要给 `codaze` 传启动参数，不要写成一整个字符串；应当在 `ProgramArguments` 里一项一项追加。例如：

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
  <string>0.120.0</string>
</array>
```

`codaze` 目前不依赖工作目录读取运行时文件，所以通常不需要设置 `WorkingDirectory`。

如果你需要上游代理，把这些变量加到 `EnvironmentVariables` 里，例如：

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

## 说明

- `codaze` 默认只监听 loopback，所以即使作为服务运行，不显式改参数也不会暴露到非本地地址。
- 默认账号目录依赖 `HOME`，如果服务管理器给出的环境不完整，建议显式设置 `HOME`。

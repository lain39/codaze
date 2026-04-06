[English](RELEASE.md) | [简体中文](RELEASE.zh-CN.md)

# Codaze 发布说明

- 仓库主页：https://github.com/lain39/codaze
- GitHub Releases：https://github.com/lain39/codaze/releases

## 发布检查清单

1. 先把待发布内容整理到 [CHANGELOG.md](../CHANGELOG.md) 的 `Unreleased` 段落。
2. 更新 crate 版本，并把 `Unreleased` 中本次发布的内容切出来：

```bash
bash scripts/bump-version.sh X.Y.Z
```

也可以直接走一条命令：

```bash
make release-prep VERSION=X.Y.Z
```

如果你只是想先打一版预发布 tag 做试跑，比如 `vX.Y.Z-rcN`，当前元数据校验和 release notes 渲染也支持这种 tag；它仍然会读取 `X.Y.Z` 对应的 changelog 段落。

3. 确认 [Cargo.toml](../Cargo.toml) 里固定的 Codex Git revision 仍然是你想要的。
4. 执行发布前检查：

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test -q
cargo deny check
cargo about -L off generate --output-file THIRD_PARTY_LICENSES.md about.hbs
bash scripts/check-release-metadata.sh vX.Y.Z
bash scripts/render-release-notes.sh vX.Y.Z > dist/RELEASE_NOTES.md
```

如果你只想先做一轮不修改 `Cargo.toml` 或 `CHANGELOG.md` 的完整预检，用：

```bash
make release-precheck VERSION=X.Y.Z
```

5. 如果需要，先在本地编译 release 二进制：

```bash
mkdir -p dist
cargo build --release
```

6. 创建并推送 tag：

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

7. 确认 GitHub Release 产物至少包含：

- `codaze` 压缩包
- `LICENSE`
- `NOTICE`
- `THIRD_PARTY_LICENSES.md`
- 从 `CHANGELOG.md` 渲染出来的发布说明

## 发布说明模板

```md
# Codaze X.Y.Z

### Added

- ...

### Changed

- ...

### Fixed

- ...
```

## 备注

- `scripts/bump-version.sh` 会更新 `Cargo.toml`，并把当前 `Unreleased` 的内容切成新的已发布段落。
- 脚本会把 `Unreleased` 重置回标准空模板。
- 这个脚本是保守设计：如果目标版本段落已经存在，它会拒绝覆盖。
- `scripts/check-release-metadata.sh` 和 `scripts/render-release-notes.sh` 既支持 `vX.Y.Z`，也支持 `vX.Y.Z-rc1` 这类预发布 tag；如果 `Cargo.toml` 还是正式版 `X.Y.Z`，它们会继续读取 `## X.Y.Z - YYYY-MM-DD` 这一段 changelog。
- `make release-precheck` 不会修改 `Cargo.toml` 或 `CHANGELOG.md`；它只会生成 `THIRD_PARTY_LICENSES.md`、`dist/RELEASE_NOTES.md` 这类临时产物。
- `make release-prep` 会先跑工具和构建检查，再修改 `Cargo.toml` / `CHANGELOG.md`，最后再做 release 元数据校验和 release notes 渲染。
- `make verify` 只负责日常开发检查，和 release/compliance 工具链分开。
- release matrix 里每个平台都会用 `cargo about --target <matrix-target>` 单独生成自己的 `THIRD_PARTY_LICENSES.md`，再随该平台归档一起打包。
- GitHub Release 现在允许“部分平台先发布”：只要已经成功构建出的平台归档存在，就会先附加到 Release，失败的平台可以后续补。

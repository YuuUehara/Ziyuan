# 发布

本页说明如何用标签发布紫垣，以及 crates.io 的发布流程。只描述仓库内已经配置的步骤，不含令牌、密钥或其他凭据。

## 触发条件

- 标签须与 `Cargo.toml` 的版本一致，例如版本 `1.2.0` 对应标签 `v1.2.0`；`release.yml` 会先比对，不一致即失败。
- 带连字符的版本（如 `1.0.0-rc.1`）按预发布处理，不标为 latest。
- 发布说明取自 `CHANGELOG.md` 中标题为 `## <版本>`（或版本后接空格及说明）的一节；找不到时由 GitHub 自动生成。发版前把该版本一节写进 CHANGELOG。

```bash
git tag v1.2.0
git push origin v1.2.0
```

推送标签会触发 `.github/workflows/release.yml`。不要手工改已公开 Release 的资产来“修”构建问题；有缺陷应发新版本。

## release.yml 做什么

1. **create**：检查标签与版本一致，建一个草稿 Release（附 CHANGELOG 说明）。
2. **build**：四个平台各自在原生 runner 上构建并打包：
   - `x86_64-unknown-linux-gnu`（Ubuntu 22.04，构建后检查 glibc 2.35 基线与启动）；
   - `x86_64-apple-darwin`；
   - `aarch64-apple-darwin`；
   - `x86_64-pc-windows-msvc`。
   每个平台上传压缩包后，会**重新下载刚上传的资产**、解压、运行 `scripts/check-package.sh` 验收，Linux 另查 glibc 兼容；验收的是下载包，不是构建目录里的二进制。
3. **publish**：全部平台验收通过后，把草稿 Release 公开（预发布不标 latest）。
4. **crates**：Release 公开后才运行，向 crates.io 发布十个 crate。它需要 `id-token: write` 权限，用 `rust-lang/crates-io-auth-action@v1` 换取短期令牌；步骤失败不影响已发布的二进制包。

build 或 publish 任一步失败，Release 保持草稿或不被公开，crates 作业也不会运行；crates 作业失败只影响 crate 发布，已公开的二进制包和 Release 不受影响。

## crates.io 可信发布

发布顺序按依赖排列：

```text
ziyuan-core → ziyuan-governance → ziyuan-storage → ziyuan-runtime
→ ziyuan-github → ziyuan-llm → ziyuan-app → ziyuan-web → ziyuan-tui → ziyuan-cli
```

作业会先查询 crates.io，HTTP 200 表示已有当前版本，直接跳过；其他状态都会尝试 `cargo publish --locked -p <crate>`，并非只在确认不存在时发布。重跑可以跳过已成功发布的版本；网络或服务错误仍可能导致作业失败。

### 第一步：手工发布各 crate 的首个版本（1.1.0）

可信发布要求 crate 已在 crates.io 存在，所以**每个 crate 的首个版本必须手工发布一次**。本项目以 1.1.0 建立这十个 crate：

1. 在独立目录使用 **1.1.0 对应的源码**，确认根 `Cargo.toml` 版本及内部依赖版本均为 1.1.0。不要在当前 1.2.0 源码中执行命令，却以为发布的是 1.1.0。
2. 在本机配置有发布权限的 crates.io API token，按上面的十项顺序，逐一执行 `cargo publish --locked -p <crate>`；把 `<crate>` 换成实际名称。每一步确认成功且依赖版本已可查询后，再发布下一项。已有 1.1.0 的 crate 不重复发布。
3. 十个 crate 都存在后，再逐个登记下面的可信发布配置，之后才推送 1.2.0 标签。

该作业只发布**当前标签**的版本，不回溯补发旧版本。若 1.1.0 从未发布，须先手工补发；这是建立 crate 和配置可信发布的步骤，不是因为 1.2.0 依赖 1.1.0（1.2.0 的内部依赖均固定为 `=1.2.0`）。

不要把 token 写进仓库、脚本或日志。若用 `cargo login` 配置 token，它会保存到本机 Cargo 凭据配置；使用其他凭据方式时，按该方式的规则保管。

### 第二步：逐 crate 登记可信发布

以下配置须对上面列出的**全部十个 crate** 分别完成：

1. 打开该 crate 的页面 → Settings → Trusted Publishing。
2. 仓库填 `YuuUehara/Ziyuan`。
3. workflow 文件名填 `release.yml`。
4. environment 留空（本仓库的发布作业不使用 GitHub Environment）。
5. 保存。

配置是逐 crate 的，漏配一个，发布到它时就会失败。全部登记完成后，后续版本才能交给 `release.yml` 的 `crates` 作业；仓库配置不会自动替你创建 crates.io 上的登记。

### 不能撤回

不要把 crates.io 发布当作可删除的草稿；发布后通常只能 yank（撤下但不删除，已有 `Cargo.lock` 可继续使用，新依赖解析不再选中）。因此发布前先核对版本号、依赖与许可证；发错了应发新版本修正，不能当没发生过。

### 重跑

`crates` 作业失败后可以重跑（GitHub Actions 的 re-run failed jobs）：已发布成功的 crate 会被识别并跳过，未发布的继续。不要为了“重来”而去 yank 已发布的版本，那会影响已有用户。

## 发布前核对

- `Cargo.toml` 与各 crate 的版本一致，标签与之相符。
- `CHANGELOG.md` 有对应版本的一节，措辞与实际改动一致；未验证的项目照实写在验收记录里。
- 本地通过 `cargo fmt --all -- --check`、`cargo check --workspace --all-targets --all-features`、`cargo test --workspace` 与 `bash scripts/smoke.sh`。
- 已知风险与未测项目写进 Release 说明或链接的验收记录，不把候选版或模拟测试当作正式包验收。

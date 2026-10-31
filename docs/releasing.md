# 发布

本页说明如何用标签发布紫垣，以及 crates.io 的发布流程。只描述仓库内已经配置的步骤，不含令牌、密钥或其他凭据。

## 触发条件

- 标签须与 `Cargo.toml` 的版本一致，例如版本 `1.3.0` 对应标签 `v1.3.0`；`release.yml` 会先比对，不一致即失败。1.3 尚有下文列出的发布阻塞，不要先推标签再补准备。
- 带连字符的版本（如 `1.0.0-rc.1`）按预发布处理，不标为 latest。
- 发布说明取自 `CHANGELOG.md` 中标题为 `## <版本>`（或版本后接空格及说明）的一节；找不到时由 GitHub 自动生成。发版前把该版本一节写进 CHANGELOG。

```bash
git tag v1.3.0
git push origin v1.3.0
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
4. **crates**：Release 公开后才运行，按依赖顺序向 crates.io 发布十一个 crate（含新增的 `ziyuan-memory`）。它需要 `id-token: write` 权限，用 `rust-lang/crates-io-auth-action@v1` 换取短期令牌；步骤失败不影响已发布的二进制包。新 crate 仍须先完成下文的首次发布与配置。

build 或 publish 任一步失败，Release 保持草稿或不被公开，crates 作业也不会运行；crates 作业失败只影响 crate 发布，已公开的二进制包和 Release 不受影响。

## crates.io 可信发布

1.3 共十一个 crate，workflow 按以下依赖顺序发布；`ziyuan-memory` 在依赖它的 `ziyuan-app` 之前：

```text
ziyuan-core → ziyuan-governance → ziyuan-storage → ziyuan-runtime
→ ziyuan-github → ziyuan-llm → ziyuan-memory → ziyuan-app → ziyuan-web → ziyuan-tui → ziyuan-cli
```

作业会先查询 crates.io，HTTP 200 表示已有当前版本，直接跳过；其他状态都会尝试 `cargo publish --locked -p <crate>`，并非只在确认不存在时发布。重跑可以跳过已成功发布的版本；网络或服务错误仍可能导致作业失败。

### 1.3 发布阻塞

当前已知 `ziyuan-memory` 的 crates.io 注册表查询返回 404，尚未建立；本轮没有可用的 crates.io 发布凭据，**没有完成手工首发（bootstrap）或可信发布登记**。GitHub OIDC 的可信发布只能给已存在并配置好的 crate 发后续版本，不能建立首个 crate，也不会因为重跑作业而解除这个阻塞。

发布前须由有权限的维护者完成两件事：

1. 按下节手工发布 `ziyuan-memory` 的首个版本，再为它登记可信发布；不能拿已有十个 crate 的登记代替。
2. 确认 `release.yml` 包含上面的十一项顺序；本轮已补入 `ziyuan-memory`，但这不替代它的首次发布与配置。

已发布的 1.2.3 main／标签与资产保留，不重打标签、不覆盖历史包。四平台包验收与 crates.io 是不同结果：Release 已公开也不等于十一个 crate 都已发布。

### 第一步：手工发布尚不存在的 crate 的首个版本

可信发布要求 crate 已在 crates.io 存在，所以**每个新 crate 的首个版本必须手工发布一次**。原有十个 crate 是否存在及可信发布是否配置，要逐个核对，不要求为了 1.3 重发或改写 1.1.0 历史。对 1.3 新增的 `ziyuan-memory`：

1. 使用经过审阅的 **1.3.0 对应源码**，确认根 `Cargo.toml` 版本、内部依赖、锁文件、许可证与打包内容正确。先检查 `cargo package --locked -p ziyuan-memory`，不要把打包检查当作注册表发布成功。
2. 在有权限的本机配置 crates.io API token，执行 `cargo publish --locked -p ziyuan-memory`。等待并确认 `ziyuan-memory` 的 `1.3.0` 已可在注册表查询，再继续；这一步会真正公开 crate，须由维护者明确批准，不能用可信发布代替。
3. 为新 crate 完成下节登记。其余 crate 的 1.3.0 仍按上面的依赖顺序发布；已经存在的相同版本跳过，依赖版本可查询后才进行下一项。尚不存在的其他 crate 也必须先完成各自的手工首发。

该作业只发布**当前标签**的版本，不回溯补发旧版本。1.3.0 的内部依赖固定为 `=1.3.0`，发布应用层前必须先让它依赖的本版 crate 在注册表可用；补发旧版本不能代替这一步。

不要把 token 写进仓库、脚本或日志。若用 `cargo login` 配置 token，它会保存到本机 Cargo 凭据配置；使用其他凭据方式时，按该方式的规则保管。

### 第二步：逐 crate 登记可信发布

以下配置须对上面列出的**全部十一个 crate** 分别核对；已经正确配置的旧 crate 不必重复登记，新 `ziyuan-memory` 不能漏掉：

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
- `release.yml` 的循环已覆盖十一个 crate，`ziyuan-memory` 位于 `ziyuan-app` 之前；新 crate 手工首发成功、注册表版本可查询，且逐 crate 的可信发布配置已核对。当前这些阻塞未全部解除，不推 1.3 标签。
- `CHANGELOG.md` 有对应版本的一节，措辞与实际改动一致；未验证的项目照实写在验收记录里。
- 本地通过 `cargo fmt --all -- --check`、`cargo check --workspace --all-targets --all-features`、`cargo test --workspace` 与 `bash scripts/smoke.sh`。
- 已知风险与未测项目写进 Release 说明或链接的验收记录，不把候选版或模拟测试当作正式包验收。

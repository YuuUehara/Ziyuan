# 发布指南

本页说明紫垣的标签发布流程、四平台下载包验收及 crates.io 可信发布约定，不含令牌、密钥或其他凭据。最新检查进度见 [1.15.2 验收记录](acceptance/1.15.2.md)，已发布版本记录见各版验收归档。

## 触发条件

- 标签必须与 `Cargo.toml` 版本一致（例如版本 `1.15.1` 对应标签 `v1.15.1`）；`.github/workflows/release.yml` 会先核对，不一致即失败。
- 带连字符的预发布版本（如 `1.0.0-rc.1`）不标为 `latest`。
- 发布说明取自 `CHANGELOG.md` 中 `## <版本>` 一节；找不到时由 GitHub 自动生成。发版前须先整理对应版本章节。

本仓库使用 Jujutsu 管理发布提交。候选提交的常规 CI 全部通过后，再把标签指向该提交：

```bash
jj tag set v1.15.1 -r <已通过检查的候选提交>
jj git push --remote origin --tag v1.15.1
```

已公开的标签不得重复覆盖；不要手工替换已公开 Release 内的资产来“修补”构建问题，发现缺陷应发新版本修正。

## release.yml 流程

1. **create**：校验标签与 `Cargo.toml` 版本一致，创建草稿 Release 并附上 `CHANGELOG.md` 说明。
2. **build**：四个平台分别在原生 runner 上构建并打包（含 Windows／macOS／Linux 启动脚本与 `JetBrainsMono-OFL.txt`）：
   - `x86_64-unknown-linux-gnu`（Ubuntu 22.04，检查 glibc 2.35 基线）
   - `x86_64-apple-darwin`
   - `aarch64-apple-darwin`
   - `x86_64-pc-windows-msvc`
   每个平台上传压缩包后都会**重新下载已上传资产**、解压并执行 `scripts/check-package.sh` 验收，确认实际分发包可用。
3. **publish**：全部平台验收通过后公开 Release。
4. **crates**：Release 公开后运行，借助 `rust-lang/crates-io-auth-action@v1` 通过 GitHub OIDC 换取短期令牌，按依赖顺序向 crates.io 发布十一个生产 crate（已存在版本跳过）。

任一构建或下载包验收失败时，Release 保持草稿且不会触发 crate 发布；`crates` 作业失败则不影响已公开的二进制下载包，可在修正配置后重跑失败作业。

## crates.io 可信发布

工作区发布的十一个生产 crate 顺序如下（`ziyuan-sandbox-windows` 为未发布实验成员，不在发布循环中）：

```text
ziyuan-core → ziyuan-governance → ziyuan-storage → ziyuan-runtime
→ ziyuan-github → ziyuan-llm → ziyuan-memory → ziyuan-app → ziyuan-web → ziyuan-tui → ziyuan-cli
```

### 登记与首发要求

- **首个版本须手工首发**：crates.io 的 Trusted Publishing 要求 crate 已存在于注册表，因此日后若新增对外发布的 crate，须先由维护者检查 `cargo package --locked -p <crate>` 并手工发布首个版本，不能直接交给工作流首发。
- **逐 crate 配置 Trusted Publishing**：每个 crate 页面均需在 `Settings → Trusted Publishing` 填写仓库 `YuuUehara/Ziyuan`、workflow 文件名 `release.yml`，`environment` 留空。
- **不可撤回**：crates.io 发布后无法删除版本，仅可 `yank`。重跑失败的 `crates` 作业会自动跳过已发布 crate，切勿为重来而轻易 yank 已发布版本。
- 不要将任何 API token 写入仓库、脚本或日志。

## 发布前核对清单

- 候选提交先推送至 `codex/**`、`work/**` 或 PR 运行 `.github/workflows/ci.yml`，逐项确认检查全绿。
- `Cargo.toml` 版本与 工作区依赖、`Cargo.lock` 与 `ziyuan-storage::RELEASES` 版本表、目标标签一致；`CHANGELOG.md` 已写入该版说明。
- 本地通过 `cargo fmt --all -- --check`、`cargo check --workspace --all-targets --all-features`、`cargo test --workspace`、`node --test crates/ziyuan-web/src/*.test.mjs`、`python scripts/check-doc-links.py` 及 `bash scripts/smoke.sh`。
- 实际测试范围与未测边界照实记录在对应验收档案中。

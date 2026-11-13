# 文档目录

本文档对应 **1.15.0** 源码。初次使用先看[使用指南](guide.md)，译书操作看[文本项目指南](text-projects.md)和[小说翻译指南](novel-translation.md)。本版可以追加、修订 Markdown、DOCX 和 EPUB，终端可以写人工译稿，远程嵌入须先按项目授权。

发布与验证进度见 [1.15.0 验收记录](acceptance/1.15.0.md)，已公开下载包以 [GitHub Releases](https://github.com/YuuUehara/Ziyuan/releases) 为准。版本变化与兼容性看 [CHANGELOG](../CHANGELOG.md)，发布流程看 [releasing](releasing.md)。历史验收记录只记录当时实际做过的检查。

连续翻译操作与支持边界见[备份、原文更新、跨章待查与交稿](translation-continuity.md)；既有译稿版本、文档回填和人工译稿操作见[文本项目指南](text-projects.md)。

## 使用

| 文档 | 内容 |
| --- | --- |
| [使用指南](guide.md) | 命令、Web、终端界面和常见操作 |
| [文本整理与检查](text-projects.md) | 导入原文、编辑人工译稿、检查并导出成稿 |
| [连续翻译与交稿](translation-continuity.md) | 备份与恢复、更新原文、跨章待查、通读、检索替换及记忆交换 |
| [作品更新提醒](subscriptions.md) | 关注 syosetu 作品、检查新章节 |
| [小说翻译指南](novel-translation.md) | 逐块初译、审校、朱批、修订与御览 |
| [译文记忆](memory.md) | 本地建库、查询与人工选用参考 |
| [DAG 排程速查](dag.md) | 拆分任务、设置依赖 |

本地资料、段旁提示与复核关联的用法见 [README](../README.md) 的「本地资料与段旁提示」。

## 约定与发布

| 文档 | 内容 |
| --- | --- |
| [稳定性约定](stability.md) | 哪些接口只增不改，哪些仍可能调整 |
| [设计文档](architecture.md) | 内部分层、事件簿和权限边界 |
| [ADR-0001：Windows 号舍](adr/0001-windows-sandbox.md) | Windows 上为什么默认拒绝需要隔离的命令 |
| [路线图](roadmap.md) | 已完成的版本和后续计划 |
| [发布](releasing.md) | 标签发布、四平台验收与 crates.io 可信发布 |

## 验收记录（历史归档）

每份只记录当时实际做过的检查和未测项目。最新一份可能仍在验收，是否已公开发行请看记录中的状态；历史版本的结果不代替当前版本检查。

- [1.15.0](acceptance/1.15.0.md)
- [1.14.1](acceptance/1.14.1.md)
- [1.14.0](acceptance/1.14.0.md)
- [1.13.1](acceptance/1.13.1.md)
- [1.13.0](acceptance/1.13.0.md)
- [1.12.0](acceptance/1.12.0.md)
- [1.11.0](acceptance/1.11.0.md)
- [1.10.0](acceptance/1.10.0.md)
- [1.9.0](acceptance/1.9.0.md)
- [1.8.0](acceptance/1.8.0.md)
- [1.7.0](acceptance/1.7.0.md)
- [1.6.0](acceptance/1.6.0.md)
- [1.5.x（1.5.0–1.5.3）](acceptance/1.5.x.md)
- [1.4.1](acceptance/1.4.1.md)
- [1.4.0 汇总](acceptance/1.4.0.md)
- [1.4 第一批](acceptance/1.4-first-batch.md)
- [1.4 第三批 3A](acceptance/1.4-third-batch-3a.md)、[3B](acceptance/1.4-third-batch-3b.md)、[3C](acceptance/1.4-third-batch-3c.md)
- [1.3.0](acceptance/1.3.0.md)
- [1.2.3](acceptance/1.2.3.md)
- [1.2.2](acceptance/1.2.2.md)
- [1.2.2 临时租约可行性报告](acceptance/1.2.2-temp-lease-feasibility.md)
- [1.2.1](acceptance/1.2.1.md)
- [1.2.0](acceptance/1.2.0.md)
- [1.1.0](acceptance/1.1.0.md)
- [1.0.0-rc.4 发布包](acceptance/1.0.0-rc.4.md)
- [1.0.0-rc.2](acceptance/1.0.0-rc.2.md)
- [1.0.0-rc.1 验收清单](acceptance/1.0.0-rc.1.md)

## 内部资料

以下文件保留给维护者，不是用户操作指南，也不代表当前功能：工作台设计/验收、历史开发计划与写入安全决策。它们不在上表列出，避免与现行用法混淆。

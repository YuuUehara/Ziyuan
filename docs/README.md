# 文档目录

先读 [README](../README.md) 的安装和快速开始，再按需要查下面的文档。

## 使用

| 文档 | 内容 |
| --- | --- |
| [使用指南](guide.md) | 命令、Web、终端界面和常见操作 |
| [文本整理与检查](text-projects.md) | 导入原文、分块、检查候选译稿（1.1.0 起） |
| [作品更新提醒](subscriptions.md) | 关注 syosetu 作品、检查新章节（1.1.0 起），新章节自动入队（1.2.0 起） |
| [小说翻译指南](novel-translation.md) | 逐块初译、审校、朱批、修订与御览（1.2.0 起），以及版权与上传边界 |
| [DAG 排程速查](dag.md) | 拆分任务、设置依赖；输入示例见 [examples/dag-plan.json](examples/dag-plan.json) |

## 约定与设计

| 文档 | 内容 |
| --- | --- |
| [稳定性约定](stability.md) | 哪些接口只增不改，哪些仍可能调整 |
| [设计文档](architecture.md) | 内部分层、事件簿和权限边界 |
| [ADR-0001：Windows 号舍](adr/0001-windows-sandbox.md) | Windows 上为什么默认拒绝需要隔离的命令 |
| [路线图](roadmap.md) | 已完成的版本和后续计划 |
| [发布](releasing.md) | 标签发布、四平台验收与 crates.io 可信发布 |

## 验收记录

每份只记录当时实际做过的检查和未测项目。

- [1.2.0](acceptance/1.2.0.md)
- [1.1.0](acceptance/1.1.0.md)
- [1.0.0-rc.4 发布包](acceptance/1.0.0-rc.4.md)
- [1.0.0-rc.2](acceptance/1.0.0-rc.2.md)
- [1.0.0-rc.1 验收清单](acceptance/1.0.0-rc.1.md)

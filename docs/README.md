# 文档目录

先读 [README](../README.md) 的安装和快速开始，再按需要查下面的文档。

当前已发布 **1.4.1**，版本核对见[1.4.1 验收](acceptance/1.4.1.md)。操作看指南，交付事实看验收，待开发事项看正式计划；不要把旧设计目标当成现有功能。

## 使用

| 文档 | 内容 |
| --- | --- |
| [使用指南](guide.md) | 命令、Web、终端界面和常见操作 |
| [文本整理与检查](text-projects.md) | 导入原文、分块、检查候选译稿（1.1.0 起） |
| [作品更新提醒](subscriptions.md) | 关注 syosetu 作品、检查新章节（1.1.0 起），新章节自动入队（1.2.0 起） |
| [小说翻译指南](novel-translation.md) | 逐块初译、审校、朱批、修订与御览（1.2.0 起），以及版权与上传边界 |
| [译文记忆](memory.md) | 本地建库、查询与人工选用参考；后续远程嵌入的授权与评测门槛 |
| [DAG 排程速查](dag.md) | 拆分任务、设置依赖；输入示例见 [examples/dag-plan.json](examples/dag-plan.json) |

## 约定与设计

| 文档 | 内容 |
| --- | --- |
| [稳定性约定](stability.md) | 哪些接口只增不改，哪些仍可能调整 |
| [设计文档](architecture.md) | 内部分层、事件簿和权限边界 |
| [ADR-0001：Windows 号舍](adr/0001-windows-sandbox.md) | Windows 上为什么默认拒绝需要隔离的命令 |
| [路线图](roadmap.md) | 已完成的版本和后续计划 |
| [代码任务正式分批计划](code-task-next-batch.md) | 闭环补强、Windows 原生号舍与代码索引的范围和验收门槛 |
| [译书写入安全](translation-write-safety.md) | 安全决策及材料事务等未完成要求，不是已交付承诺 |
| [临时文件清理设计](temp-cleanup-design.md) | 租约状态机、锁顺序、删除不变量与已知限制 |
| [发布](releasing.md) | 标签发布、四平台验收与 crates.io 可信发布 |

1.4 的[需求记录](novel-translation-1.4.md)和[第三批计划](novel-translation-1.4-third-batch.md)保留连续两章实测依据与独立验收要求，只作历史资料。早期逐步实施脚本、重复代码片段和工具专用流程已撤下；其中独立的早期检查结果归入[1.2.1 验收](acceptance/1.2.1.md#早期分块验证的独立记录2026-10-31)，未交付的写入安全要求及记忆库授权门槛保留在上表对应文档。

## 验收记录

每份只记录当时实际做过的检查和未测项目。

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

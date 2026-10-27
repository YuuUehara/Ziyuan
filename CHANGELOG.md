# Changelog

## 0.5.0

v0.5 让紫垣从演示走向日常可用：自己转得起来、记得住前事、接得上外界。

### 新增

- 常朝：`ziyuan run --watch` 常驻执行，入队与驿传的诏令随到随办，办完自动请都察院复核（`--no-censor` 可关）。
- 起居注可查：`ziyuan history` 按诏令、官员、事件类型与时间筛选，支持 `--json`；Web 起居注可筛选，TUI 显示时间；模型调用的 trace ID 记入事件簿。
- 翰林院修史：已结诏令归档为实录，`ziyuan records [检索词]` 可查；承办官员可用只读工具 `search_records` 查前例，票拟与廷推附上相关实录。
- 刑部复勘与平反：`ziyuan recheck` 请刑部官员复勘关键词检出（维持或误报），`ziyuan acquit` 平反后不再妨碍司礼监代批；Web 密报框可复勘、逐处平反。
- 兵部对接 GitHub：`ziyuan github pull` 让带标签的 issue/PR 与成员的 `/ziyuan` 评论入队，`github reply` 把已评定的回奏写回评论，`github pr` 把准奏的改动开为草稿 PR。
- 礼部修 CHANGELOG：`ziyuan changelog` 据提交与实录起草，写权限只限 `CHANGELOG.md`，准奏后落盘。
- 外藩朝贡：`.ziyuan/mcp.json` 登记 stdio MCP 服务，`--mcp` 按服务或逐件授权，默认不授，受官职上限约束。
- CI 与发版：GitHub Actions 跑 fmt、clippy、测试与 smoke，检查 Rust 1.89，覆盖 macOS 与 Windows；推送标签即发 Release 并附 Linux、macOS、Windows 预编译二进制。

### 改进

- 小模型适配：授了工具却一次未查就回奏的，发回重办一次；系统提示要求先查后奏。
- 驿站名上限放宽到 80 字。

### 兼容性

- v0.5 写入的事件簿含时辰等新事件，v0.4 及更早版本读取时会提示升级；v0.5 可直接读旧事件簿。

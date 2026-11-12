# 连续翻译：备份、原文更新、跨章待查与交稿

本页对应 1.14.1；发布与验证进度见[验收记录](acceptance/1.14.1.md)。1.14.0 已能导出保留文学标记的 HTML 成稿，1.14.1 在网页「章节导出」补上同一选项。这些本地整理入口不调用模型，不自动批准、定稿或推送文件。

## 完整备份与迁机恢复

Web 译书页展开「备份与恢复」，先预览全部项目清单，再确认下载。由于审批共用事件簿，备份包含全部文本项目及共用朝廷历史（可能含代码任务正文），不是单本书的分享包。原件、译稿、草稿、标记、上下文、人物术语、版本、评论和待查历史随对应数据目录保存；登录令牌、模型凭据配置文件、缓存及锁文件不打包。

```text
ziyuan text backup
ziyuan text backup --output D:/backups/books.tar
ziyuan text restore D:/backups/books.tar D:/books-restored
ziyuan text restore D:/backups/books.tar D:/books-restored --yes
```

不指定输出时只预览。恢复默认检查清单与文件哈希，`--yes` 才恢复；目标必须是尚不存在的新目录，父目录须已存在。恢复会暂存并验证项目及事件回放，成功后发布目录；失败可能留下 `.ziyuan-restore-*` 暂存目录用于诊断，不会覆盖目标。

Web 可上传备份，先校验，再输入英文新目录名确认恢复到当前工作区之下；随后从新目录启动紫垣继续使用。备份上限 256 MiB／20000 个数据文件；链接、危险路径、大小写冲突、损坏项目及未收尾写入拒绝。只读观政不能导出完整备份或恢复。

## 原文追加与修订

Web 展开「追加与修订原文」，选 TXT 文件；默认追加新章，勾选「修订当前章」则修订当前所选块的章节。先检查段落差异、受影响块与阻塞原因，再确认更新。

```text
ziyuan text source-update <项目ID> next.txt
ziyuan text source-update <项目ID> next.txt --confirm <预览指纹>
ziyuan text source-update <项目ID> revised.txt --chapter c0001
ziyuan text source-update <项目ID> revised.txt --chapter c0001 --confirm <预览指纹>
```

每次预览绑定原项目、输入和引用材料；过时或重复确认拒绝。旧原件不会覆盖，历史项目清单存于项目的 `source-history/`。旧译仍保留；受影响块的送审、勘校和定稿需要重新核对。仅变动空白而不改变翻译正文时不强行作废原本相同的块。

本批支持 UTF-8 TXT，单次最多 8 MiB。单章修订须保持段数及分块归属；不自动处理拆段、合段、章节重排或删除。破坏已批准结构化证据，或受影响块有未完成模型任务时先拒绝。Markdown／DOCX／EPUB 的原文增量更新不在本批范围；原有首次导入入口继续使用。

## 跨章待查

Web 展开「跨章待查」，读取当前项目段落，选择问题所在段及相关前后文，填写核对事项和问题说明后登记。可以从问题跳回相关编辑段，并填写理由，将其标为待查、确认问题、有意异译或已解决。

记录绑定项目、相关原译文及批准上下文，保留历次依据和理由。材料变化后旧处置标为过期，必须先「重新核对并转待查」，再作本轮处置。重复登记相同材料和问题不会重复创建。不因未登记问题就宣称全书一致，也不会自动推断人物、改稿或准奏。

```text
ziyuan text issue list <项目ID>
ziyuan text issue add <项目ID> issue.json --confirm <列表中的fingerprint>
ziyuan text issue review <项目ID> <问题数字ID> review.json
```

登记文件示例：

```json
{"paragraph":"c0001-p0001","related":["c0002-p0001"],"rule":"人物称呼","body":"两处不同称呼是否有意保留？"}
```

复核文件需填写列表中的最新 `record.version` 和该问题的 `fingerprint`：

```json
{"version":1,"fingerprint":"替换为当前问题指纹","state":"intentional","reason":"说话者与关系不同","recheck":false}
```

`state` 可为 `pending`、`confirmed`、`intentional`、`resolved`。过期记录先使用 `state=pending`、`recheck=true` 明确重新核对。关联段最多 16 项，历史最多 10000 条。

## 通读与交稿预检

Web「整章通读与交稿预检」连续显示全书或所选章的原译文，可定位回编辑段。译文注音、着重、粗斜体沿用现有标记渲染。显示的是磁盘现行译稿，不表示该稿已经准奏或定稿。

```text
ziyuan text read <项目ID> --chapter c0001
ziyuan text preflight <项目ID> --format html --chapter c0001
```

预检支持 TXT／Markdown／HTML，复用实际成稿导出资格和格式检查，列出缺稿、失效、未定稿、无法保留的标记及过期／未处置待查。待查提醒与导出阻塞分开：标为已解决不授予定稿资格，存在提醒也不自行改变既有审批规则。实际导出仍使用原入口，并再次校验材料。

## 全文检索与草稿替换

网页「全文检索与草稿替换」按原文、现行译文、人工草稿或历史译稿范围查字面片段。原文检索也覆盖尚未翻译的段落；结果可定位回编辑段。历史译稿只是参考，不因检索命中而成为当前译稿。

只对人工草稿提供替换：在命中项填写替换文字，先检查本段替换前后内容，再明确确认保存。材料或修订变化后须重新预览；与文学标记相交的替换会拒绝，其他标记按正文变化调整位置。不修改批准稿，不自动送审、准奏或定稿。

## 翻译记忆与评论管理

「翻译记忆」可新增、修改、删除原译文对及记录采纳。采纳只是本地记录，不会替换正在编辑的译文，也不增加外发资格。评论可回复、指派、标记解决或重新打开；这些操作都不改译稿和审批。

「翻译记忆交换」支持纯文本 TMX：先选文件，预览来源／目标语种、重复项和导入数量，再确认合并。导入不覆盖已有记忆或译稿；复杂行内标记、DTD 等不能安全表示的内容明确拒绝，不能把纯文本交换称为完整排版交换。

## 文档与字幕入口

网页「文档与字幕」可向空项目导入 DOCX、EPUB 或 SRT，导入前须选择对应格式并确认。原件上限 16 MiB。导出使用有效定稿；目标格式无法保留译文标记时拒绝，避免下载看似成功却丢失标记。需要保留完整译文标记时，在原成稿导出入口选择 HTML。

HTML 成稿是可离线打开的正文文件，保留译文注音、着重、粗斜体和同范围叠加样式。它不承诺照搬原 DOCX／EPUB 的分页、脚注或浮动对象；原排版仍保存在归档原件中。

## 命令行检索与记忆交换

```text
ziyuan text search <项目ID> 片段 --scope all
ziyuan text search <项目ID> 片段 --scope draft
ziyuan text tmx export <项目ID> --output memory.tmx
ziyuan text tmx preview <项目ID> memory.tmx
ziyuan text tmx import <项目ID> memory.tmx --confirm <预览指纹>
```

检索范围可选 `all`、`source`、`translation`、`draft`、`history`，最多返回200处，结果截断时须缩小范围。命令行草稿替换用 `text draft-replace <项目ID> request.json`：请求含 `block`、`paragraph`、`from`、`to`；默认只预览，确认时补上预览返回的 `fingerprint` 与 `confirm: true`。替换只针对该段，不能修改批准稿；文学标记相交时须回编辑器手工核对。

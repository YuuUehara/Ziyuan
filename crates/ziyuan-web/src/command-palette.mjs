// Pure command palette builder and multi-term fuzzy filter.
// Shared between `/assets/command-palette.mjs` and `node --test`.

export function buildPaletteSpecs({ viewer = false, tasks = [], statusLabel = {} } = {}) {
  const items = [
    { id: "hall:translation", label: "前往 · 翰林院（翻译与译稿）", meta: "殿阁", keys: "hanlin translation 翰林院 翻译", action: { type: "hall", hall: "#translation" } },
    { id: "hall:fengtian", label: "前往 · 奉天殿（诏令）", meta: "殿阁", keys: "fengtian edict task 奉天殿 诏令", action: { type: "hall", hall: "#fengtian" } },
    { id: "hall:huagai", label: "前往 · 华盖殿（朝堂制度）", meta: "殿阁", keys: "huagai court mode 华盖殿 朝堂", action: { type: "hall", hall: "#huagai" } },
    { id: "hall:wenhua", label: "前往 · 文华殿（百官与度支）", meta: "殿阁", keys: "wenhua officials duzhi 文华殿 百官 度支", action: { type: "hall", hall: "#wenhua" } },
    { id: "hall:wuying", label: "前往 · 武英殿（实录·驿站·关注）", meta: "殿阁", keys: "wuying records follow 武英殿 实录 关注", action: { type: "hall", hall: "#wuying" } },
    { id: "hall:qiju", label: "前往 · 起居注（事件簿）", meta: "殿阁", keys: "qiju history 起居注 事件簿", action: { type: "hall", hall: "#qiju" } },
    { id: "view:duzhi", label: "查看 · 户部度支奏销", meta: "只读", keys: "duzhi budget 度支 奏销 花费", action: { type: "duzhi" } },
    { id: "export:csv", label: "导出 · 户部度支报表（CSV / Excel BOM）", meta: "导出", keys: "duzhi export csv excel 度支 导出", action: { type: "export", format: "csv", bom: true } },
    { id: "export:json", label: "导出 · 户部度支报表（JSON）", meta: "导出", keys: "duzhi export json 度支 导出", action: { type: "export", format: "json", bom: false } },
    { id: "view:kaocheng", label: "查看 · 考成簿", meta: "只读", keys: "kaocheng 考成 政绩", action: { type: "kaocheng" } },
    { id: "view:doctor", label: "自检 · 环境与配置诊断（ziyuan doctor）", meta: "只读", keys: "doctor diagnose health 自检 诊断 健康 沙箱 模型", action: { type: "doctor" } },
    { id: "ui:theme", label: "切换主题 · 汝窑天青 / 墨分五色 / 青绿山水", meta: "界面", keys: "theme tianqing mo qinglv 主题 配色", action: { type: "theme" } },
  ];
  if (!viewer) {
    items.push(
      { id: "op:edict", label: "下诏 · 打开新诏令表单", meta: "操作", keys: "edict new 下诏 新建", action: { type: "new-edict" } },
      { id: "op:run", label: "执行 · 执行待办诏令", meta: "操作", keys: "run execute 执行 待办", action: { type: "run" } },
    );
  }
  if (Array.isArray(tasks)) {
    for (const t of tasks.slice(0, 20)) {
      const snippet = String(t.text || "").replace(/\s+/g, " ").slice(0, 42);
      items.push({
        id: `task:${t.id}`,
        label: `诏令 #${t.id} · ${snippet}`,
        meta: statusLabel[t.status] || t.status || "",
        keys: `#${t.id} ${t.id} ${t.department || ""} ${snippet}`,
        action: { type: "task", taskId: t.id },
      });
    }
  }
  return items;
}

export function filterPaletteItems(items, query) {
  const q = String(query || "").trim().toLowerCase();
  if (!q) return items.slice();
  const terms = q.split(/\s+/);
  return items.filter((item) => {
    const hay = `${item.label} ${item.meta} ${item.keys}`.toLowerCase();
    return terms.every((term) => hay.includes(term));
  });
}

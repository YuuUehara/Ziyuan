// Translation block version snapshots: list, read-only diff against current/draft, manual snapshot, and explicit prune.
// Shared between `/assets/text-versions.mjs` and `node --test`.

export const VERSION_STATE_LABELS = Object.freeze({
  draft: "草稿",
  approved: "准奏",
  finalized: "定稿",
  manual: "手工",
});

export const VERSION_CHANGE_LABELS = Object.freeze({
  added: "新增",
  removed: "删除",
  changed: "改写",
  markup: "标记",
});

export function versionStateLabel(kind) {
  return VERSION_STATE_LABELS[kind] || String(kind || "");
}

export function versionTime(ms) {
  try {
    return new Date(ms).toLocaleString();
  } catch {
    return String(ms);
  }
}

export function formatVersionOptionLabel(version) {
  const state = versionStateLabel(version?.state);
  const label = version?.label ? ` · ${version.label}` : "";
  return `${state}${label} · ${versionTime(version?.created_ms)}`;
}

export function formatVersionDiffChange(change, formatLocation = (id) => String(id || "")) {
  const loc = formatLocation(change?.paragraph);
  const kind = VERSION_CHANGE_LABELS[change?.kind] || String(change?.kind || "");
  let text = `${loc} · ${kind}`;
  if (change?.kind === "changed") {
    text += `：${change.old || ""} → ${change.new || ""}`;
  } else if (change?.kind === "added") {
    text += `：${change.new || ""}`;
  } else if (change?.kind === "removed") {
    text += `：${change.old || ""}`;
  }
  return text;
}

export function renderVersionDiffView({
  doc = globalThis.document,
  host,
  statusEl,
  result,
  formatLocation = (id) => String(id || ""),
}) {
  if (!host) return;
  host.replaceChildren();
  const changes = Array.isArray(result?.changes) ? result.changes : [];
  if (statusEl) {
    statusEl.textContent = changes.length
      ? `${changes.length} 处差异（对比${result?.against === "draft" ? "草稿" : "现行译稿"}；标记仅指译文相同、标记有变）。`
      : "没有差异。";
  }
  for (const change of changes) {
    const row = doc.createElement("p");
    row.textContent = formatVersionDiffChange(change, formatLocation);
    host.append(row);
  }
}

export function createTextVersionsController({
  doc = globalThis.document,
  getScope = () => ({ project: null, block: null }),
  isViewer = () => true,
  apiCall,
  confirmAction = (msg) => globalThis.confirm(msg),
  formatLocation = (id) => String(id || ""),
  getElement = (id) => doc.getElementById(id),
}) {
  let versions = [];
  let requestSeq = 0;
  let bound = false;

  function versionBase(project, block) {
    return `/text/${encodeURIComponent(project || "")}/blocks/${encodeURIComponent(block || "")}`;
  }

  function setSelectOptions(listEl, items, placeholder) {
    listEl.replaceChildren();
    if (!items.length) {
      const opt = doc.createElement("option");
      opt.value = "";
      opt.textContent = placeholder;
      listEl.append(opt);
      return;
    }
    for (const v of items) {
      const opt = doc.createElement("option");
      opt.value = String(v.id || "");
      opt.textContent = formatVersionOptionLabel(v);
      listEl.append(opt);
    }
  }

  function renderButtons() {
    const { block } = getScope();
    const viewer = Boolean(isViewer());
    const has = versions.length > 0;
    for (const [id, disabled] of [
      ["version-compare-current", !has],
      ["version-compare-draft", !has],
      ["version-save", viewer || !block],
      ["version-prune", viewer || !has],
    ]) {
      const btn = getElement(id);
      if (btn) btn.disabled = disabled;
    }
  }

  async function load() {
    const { project, block } = getScope();
    const ticket = ++requestSeq;
    const list = getElement("version-list");
    const status = getElement("version-status");
    const diff = getElement("version-diff");
    if (!list) return;
    if (!project || !block) {
      versions = [];
      list.replaceChildren();
      list.disabled = true;
      if (status) status.textContent = "选中一组译稿后，可查看它的版本记录。";
      diff?.replaceChildren();
      renderButtons();
      return;
    }
    list.disabled = true;
    if (status) status.textContent = "正在读取快照…";
    try {
      const result = await apiCall(`${versionBase(project, block)}/versions?t=${Date.now()}`);
      const cur = getScope();
      if (ticket !== requestSeq || project !== cur.project || block !== cur.block) return;
      versions = Array.isArray(result?.versions) ? result.versions : [];
      if (versions.length) {
        setSelectOptions(list, versions.slice().reverse(), "");
      } else {
        setSelectOptions(list, [], "（尚无快照）");
      }
      list.disabled = !versions.length;
      if (status) {
        status.textContent = versions.length
          ? `共 ${versions.length} 份；自动快照每态留最新 3 份。`
          : "尚无快照。保存草稿或准奏／定稿后留存，也可点「保存当前快照」。";
      }
      diff?.replaceChildren();
      renderButtons();
    } catch (e) {
      if (ticket !== requestSeq) return;
      versions = [];
      setSelectOptions(list, [], "（读取失败）");
      list.disabled = true;
      if (status) status.textContent = "读取快照失败：" + (e?.body?.error || e?.message || e);
      renderButtons();
    }
  }

  async function compare(against) {
    const { project, block } = getScope();
    const list = getElement("version-list");
    const status = getElement("version-status");
    const id = list?.value;
    if (!project || !block || !id) {
      if (status) status.textContent = "没有可比较的快照。";
      return;
    }
    if (status) status.textContent = "正在比较…";
    try {
      const result = await apiCall(`${versionBase(project, block)}/versions/${encodeURIComponent(id)}?against=${against}`);
      const cur = getScope();
      if (project !== cur.project || block !== cur.block) return;
      renderVersionDiffView({
        doc,
        host: getElement("version-diff"),
        statusEl: status,
        result,
        formatLocation,
      });
    } catch (e) {
      if (status) status.textContent = "比较失败：" + (e?.body?.error || e?.message || e);
    }
  }

  async function save() {
    const { project, block } = getScope();
    const status = getElement("version-status");
    if (!project || !block || isViewer()) return;
    try {
      const result = await apiCall(`${versionBase(project, block)}/versions`, { label: "" });
      if (status) status.textContent = `已保存快照 ${result?.version?.id || ""}（手工）。`;
      await load();
    } catch (e) {
      if (status) status.textContent = "保存快照失败：" + (e?.body?.error || e?.message || e);
    }
  }

  async function prune() {
    const { project, block } = getScope();
    const status = getElement("version-status");
    if (!project || !block || isViewer()) return;
    try {
      const preview = await apiCall(`${versionBase(project, block)}/versions/prune`, { keep: 3, dry_run: true });
      const candidates = Array.isArray(preview?.candidates) ? preview.candidates : [];
      if (!candidates.length) {
        if (status) status.textContent = "没有可清理的快照（自动快照每态留最新 3 份，手工快照默认保留）。";
        return;
      }
      const ok = confirmAction(
        `将删除 ${candidates.length} 份旧快照（自动快照每态留最新 3 份，手工快照默认保留）：\n\n` +
        candidates.join("\n") + "\n\n继续删除？"
      );
      if (!ok) {
        if (status) status.textContent = "已取消清理，未删除任何快照。";
        return;
      }
      const done = await apiCall(`${versionBase(project, block)}/versions/prune`, { keep: 3, dry_run: false });
      const deletedCount = Array.isArray(done?.deleted) ? done.deleted.length : 0;
      if (status) status.textContent = `已删除 ${deletedCount} 份，保留 ${done?.kept ?? 0} 份。`;
      await load();
    } catch (e) {
      if (status) status.textContent = "清理失败：" + (e?.body?.error || e?.message || e);
    }
  }

  function bindEvents() {
    if (bound) return;
    bound = true;
    getElement("version-list")?.addEventListener("change", () => {
      getElement("version-diff")?.replaceChildren();
      const status = getElement("version-status");
      if (status) status.textContent = "选择快照后点「对比现行译稿」或「对比草稿」。";
    });
    getElement("version-compare-current")?.addEventListener("click", () => compare("current"));
    getElement("version-compare-draft")?.addEventListener("click", () => compare("draft"));
    getElement("version-save")?.addEventListener("click", save);
    getElement("version-prune")?.addEventListener("click", prune);
  }

  return {
    bindEvents,
    load,
    compare,
    save,
    prune,
    renderButtons,
    getVersions: () => versions.slice(),
  };
}

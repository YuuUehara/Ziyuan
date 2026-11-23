// Read-only environment & workspace diagnostics renderer for `GET /api/doctor`.
// Shared between `/assets/doctor-panel.mjs` and `node --test`.

export const SEVERITY_LABELS = {
  ok: "正常",
  warn: "提示",
  error: "错误",
};

export function severityLabel(severity) {
  return SEVERITY_LABELS[severity] || String(severity || "");
}

export const VERSION_MARK = "（1）1.45.1（4）";
export const VERSION_MARK_LINE = "好时代，来临罢！";

export function buildDoctorSummaryLine(report) {
  if (!report || typeof report !== "object") return "尚未载入诊断结果。";
  const ver = report.version ? `v${report.version}` : "";
  const plat = [report.os, report.arch].filter(Boolean).join("/");
  const overall = severityLabel(report.overall);
  const ok = Number(report.ok_count || 0);
  const warn = Number(report.warn_count || 0);
  const err = Number(report.error_count || 0);
  return `紫垣 ${ver}${plat ? `（${plat}）` : ""} · 总评：${overall}（正常 ${ok} · 提示 ${warn} · 错误 ${err}）`;
}

function appendText(doc, parent, text) {
  const span = doc.createElement("span");
  span.textContent = text;
  parent.appendChild(span);
}

export function renderDoctorReport(container, report, { doc = container?.ownerDocument || globalThis.document } = {}) {
  if (!container || !doc) return;
  while (container.firstChild) container.removeChild(container.firstChild);

  if (!report || typeof report !== "object") {
    const empty = doc.createElement("p");
    empty.className = "muted";
    empty.textContent = "暂无诊断数据。";
    container.appendChild(empty);
    return;
  }

  const header = doc.createElement("div");
  header.className = "doctor-summary";
  header.dataset.overall = String(report.overall || "ok");
  appendText(doc, header, "紫垣 ");
  if (report.version) {
    const version = doc.createElement("button");
    version.type = "button";
    version.className = "link-button doctor-version";
    version.textContent = `v${report.version}`;
    version.setAttribute("aria-pressed", "false");
    const era = doc.createElement("p");
    era.className = "doctor-era";
    era.hidden = true;
    era.textContent = VERSION_MARK_LINE;
    version.addEventListener("click", () => {
      version.textContent = VERSION_MARK;
      version.setAttribute("aria-pressed", "true");
      era.hidden = false;
    });
    header.appendChild(version);
    const plat = [report.os, report.arch].filter(Boolean).join("/");
    const overall = severityLabel(report.overall);
    const ok = Number(report.ok_count || 0);
    const warn = Number(report.warn_count || 0);
    const err = Number(report.error_count || 0);
    appendText(
      doc,
      header,
      `${plat ? `（${plat}）` : ""} · 总评：${overall}（正常 ${ok} · 提示 ${warn} · 错误 ${err}）`,
    );
    header.appendChild(era);
  } else {
    header.textContent = buildDoctorSummaryLine(report);
  }
  container.appendChild(header);

  if (report.workspace) {
    const ws = doc.createElement("div");
    ws.className = "muted doctor-workspace";
    ws.textContent = `工作区：${report.workspace}`;
    container.appendChild(ws);
  }

  const list = doc.createElement("ul");
  list.className = "doctor-check-list";
  const checks = Array.isArray(report.checks) ? report.checks : [];
  for (const check of checks) {
    const li = doc.createElement("li");
    li.className = `doctor-check doctor-${check.severity || "ok"}`;
    li.dataset.checkId = String(check.id || "");

    const head = doc.createElement("div");
    head.className = "doctor-check-head";

    const badge = doc.createElement("span");
    badge.className = `badge doctor-badge-${check.severity || "ok"}`;
    badge.textContent = severityLabel(check.severity);
    head.appendChild(badge);

    const title = doc.createElement("strong");
    title.textContent = ` ${check.title || check.id || ""}`;
    head.appendChild(title);

    const summary = doc.createElement("span");
    summary.className = "doctor-check-summary";
    summary.textContent = ` — ${check.summary || ""}`;
    head.appendChild(summary);

    li.appendChild(head);

    if (check.detail) {
      const detail = doc.createElement("pre");
      detail.className = "doctor-check-detail";
      detail.textContent = String(check.detail);
      li.appendChild(detail);
    }
    list.appendChild(li);
  }
  container.appendChild(list);
}

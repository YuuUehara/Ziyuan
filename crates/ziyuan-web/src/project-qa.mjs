// Book-wide deterministic QA dashboard renderer & state controller.
// Shared between `/assets/project-qa.mjs` and `node --test`.

export const QA_CODE_LABELS = Object.freeze({
  no_translation: "没有有效译稿",
  missing_paragraph: "缺段",
  paragraph_id_mismatch: "段号不一致",
  extra_paragraph: "多段",
  duplicate_paragraph: "段号重复",
  empty_translation: "空译",
  untranslated: "疑似未翻译",
  number_mismatch: "数字不一致",
  duplicate_text: "重复译文",
  term_inconsistent: "术语不一致",
  source_script_leak: "源语字符残留",
  punctuation_mismatch: "标点异常",
});

export function qaCodeLabel(code) {
  return QA_CODE_LABELS[code] || String(code || "");
}

export function buildProjectQaSummaryChips(report) {
  const s = (report && report.summary) || {};
  return [
    [`总块数 ${s.total_blocks ?? 0}`, false],
    [`已译 ${s.translated_blocks ?? 0}`, false],
    [`未译 ${s.untranslated_blocks ?? 0}`, false],
    ...(report && report.founded ? [[`已定稿 ${s.finalized_blocks ?? 0}`, false]] : []),
    [`无问题 ${s.clean_blocks ?? 0}`, false],
    [`有问题块 ${s.blocks_with_issues ?? 0}`, (s.blocks_with_issues ?? 0) > 0],
    [`问题总数 ${s.total_issues ?? 0}`, (s.total_issues ?? 0) > 0],
  ];
}

export function filterProjectQaIssues(report, filterCode = "") {
  const issues = (report && Array.isArray(report.issues)) ? report.issues : [];
  if (!filterCode) return issues.slice();
  return issues.filter((issue) => issue.code === filterCode);
}

export function formatProjectQaStatus(report) {
  const s = (report && report.summary) || {};
  return `已完成整书扫描（共 ${s.total_blocks ?? 0} 块，已核对 ${s.checked_blocks ?? 0} 块，未译 ${s.untranslated_blocks ?? 0} 块；不代表语义正确）。`;
}

export function renderProjectQaDashboard({
  doc = globalThis.document,
  report,
  filter = "",
  summaryHost,
  filtersHost,
  issuesHost,
  formatLocation = (id) => String(id || ""),
  onFilterChange = () => {},
  onNavigate = () => {},
}) {
  if (!summaryHost || !filtersHost || !issuesHost) return;
  summaryHost.replaceChildren();
  filtersHost.replaceChildren();
  issuesHost.replaceChildren();
  if (!report) return;

  const s = report.summary || {};
  for (const [label, warn] of buildProjectQaSummaryChips(report)) {
    const chip = doc.createElement("span");
    chip.className = warn ? "qa-chip warn" : "qa-chip";
    chip.textContent = label;
    summaryHost.append(chip);
  }

  const byCode = report.by_code || {};
  const codeEntries = Object.entries(byCode);
  if (codeEntries.length) {
    const addFilterBtn = (code, label) => {
      const btn = doc.createElement("button");
      btn.type = "button";
      btn.className = "small";
      btn.dataset.view = "";
      btn.setAttribute("aria-pressed", String(filter === code));
      btn.textContent = label;
      btn.addEventListener("click", () => onFilterChange(code));
      filtersHost.append(btn);
    };
    addFilterBtn("", `全部 (${s.total_issues ?? 0})`);
    for (const [code, count] of codeEntries) {
      addFilterBtn(code, `${qaCodeLabel(code)} (${count})`);
    }
  }

  const list = filterProjectQaIssues(report, filter);
  if (!list.length) {
    const p = doc.createElement("p");
    p.className = "hint";
    p.textContent = (s.translated_blocks ?? 0) === 0
      ? "当前项目尚无已译块，无需核对问题。"
      : "当前筛选条件下未发现确定性检查问题（不代表语义正确）。";
    issuesHost.append(p);
    return;
  }

  const ul = doc.createElement("ul");
  ul.className = "qa-issue-list";
  for (const issue of list) {
    const li = doc.createElement("li");
    li.className = "qa-issue-item";
    const head = doc.createElement("div");
    head.className = "qa-issue-head";
    const title = doc.createElement("span");
    const loc = issue.paragraph
      ? `${formatLocation(issue.block, false)} · ${formatLocation(issue.paragraph, true)}`
      : formatLocation(issue.block, false);
    title.textContent = `[${qaCodeLabel(issue.code)}] ${loc} — ${issue.message}`;
    const go = doc.createElement("button");
    go.type = "button";
    go.className = "small";
    go.dataset.view = "";
    go.textContent = "定位编辑";
    go.addEventListener("click", () => {
      onNavigate({ block: issue.block, paragraph: issue.paragraph || null, kind: "draft" });
    });
    head.append(title, go);
    li.append(head);
    if (issue.source_excerpt || issue.target_excerpt) {
      const ex = doc.createElement("div");
      ex.className = "qa-issue-excerpts";
      if (issue.source_excerpt) {
        const sEl = doc.createElement("div");
        sEl.textContent = "原：" + issue.source_excerpt;
        ex.append(sEl);
      }
      if (issue.target_excerpt) {
        const tEl = doc.createElement("div");
        tEl.textContent = "译：" + issue.target_excerpt;
        ex.append(tEl);
      }
      li.append(ex);
    }
    ul.append(li);
  }
  issuesHost.append(ul);
}

export function createProjectQaController({
  doc = globalThis.document,
  getProjectId,
  fetchReport,
  formatLocation,
  formatError = (e) => String(e?.message || e || "未知错误"),
  onNavigate,
  statusEl,
  summaryHost,
  filtersHost,
  issuesHost,
}) {
  let state = { project: null, report: null, filter: "", request: 0 };

  function render() {
    renderProjectQaDashboard({
      doc,
      report: state.report,
      filter: state.filter,
      summaryHost,
      filtersHost,
      issuesHost,
      formatLocation,
      onFilterChange(code) {
        state.filter = code;
        render();
      },
      onNavigate,
    });
  }

  function reset(projectId) {
    if (state.project === projectId) return;
    state = { project: projectId, report: null, filter: "", request: state.request + 1 };
    if (statusEl) {
      statusEl.textContent = projectId
        ? "点「运行整书质检」查看当前项目的全书质量总览。"
        : "选择项目后，点「运行整书质检」查看全书质量总览。";
    }
    summaryHost?.replaceChildren();
    filtersHost?.replaceChildren();
    issuesHost?.replaceChildren();
  }

  async function run() {
    const project = getProjectId();
    if (!project) {
      if (statusEl) statusEl.textContent = "请先选择项目。";
      return null;
    }
    const req = ++state.request;
    if (statusEl) statusEl.textContent = "正在汇总全书确定性检查…";
    try {
      const report = await fetchReport(project);
      if (req !== state.request || project !== getProjectId()) return null;
      state = { project, report, filter: "", request: req };
      if (statusEl) statusEl.textContent = formatProjectQaStatus(report);
      render();
      return report;
    } catch (e) {
      if (req !== state.request) return null;
      if (statusEl) statusEl.textContent = "整书质检失败：" + formatError(e);
      return null;
    }
  }

  return {
    reset,
    run,
    render,
    getState: () => ({ ...state }),
  };
}

// Read-only per-project four-stage translation usage & multi-currency estimate ledger.
// Shared between `/assets/project-usage.mjs` and `node --test`.

export const STAGE_LABELS = Object.freeze({
  translate: "初译",
  fidelity_review: "忠实审校",
  style_review: "文风审校",
  revise: "修订",
});

export const TASK_STATUS_LABELS = Object.freeze({
  pending: "待办",
  assigned: "执行中",
  completed: "已完成",
  failed: "失败",
  cancelled: "已撤销",
});

export const CHARGE_PURPOSE_LABELS = Object.freeze({
  execution: "承办",
  draft: "票拟",
  split: "分办",
  censure: "都察院审查",
  recheck: "复勘",
  debate: "廷议",
});

const fmtInt = (n) => Number(n || 0).toLocaleString("zh-CN");

export function stageLabel(kind) {
  return STAGE_LABELS[kind] || String(kind || "");
}

export function taskStatusLabel(status) {
  return TASK_STATUS_LABELS[status] || String(status || "");
}

export function chargePurposeLabel(purpose) {
  return CHARGE_PURPOSE_LABELS[purpose] || String(purpose || "");
}

export function normalizeUsageNote(note) {
  return String(note || "")
    .replaceAll("缺失 usage 为未知", "未报告用量视为未知")
    .replaceAll("有 usage 的任务", "有用量记录的任务")
    .replaceAll("usage 事件数", "用量记录数")
    .replaceAll("usage", "用量")
    .replaceAll("trace", "追踪标识")
    .replaceAll("全局/官员余额", "朝廷或官员剩余额度")
    .replaceAll("操作者用时", "人工用时");
}

function tokenTotal(usage) {
  if (!usage || typeof usage !== "object") return 0;
  return Number(usage.input || 0) + Number(usage.output || 0);
}

export function formatTokenUsage(usage) {
  if (!usage || typeof usage !== "object") return "未知（未报告）";
  const input = Number(usage.input || 0);
  const output = Number(usage.output || 0);
  const total = input + output;
  const cacheRead = Number(usage.cache_read || 0);
  const cachePart = cacheRead > 0 ? ` · 缓存命中 ${fmtInt(cacheRead)}` : "";
  return `${fmtInt(total)} tokens（入 ${fmtInt(input)} / 出 ${fmtInt(output)}${cachePart}）`;
}

export function formatCurrencyEstimates(estimates) {
  if (!estimates || typeof estimates !== "object") return [];
  return Object.entries(estimates)
    .filter(([, amount]) => Number.isFinite(Number(amount)))
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([currency, amount]) => ({
      currency,
      amount: Number(amount),
      label: `估算 ${currency} ${Number(amount).toFixed(6)}（非账单）`,
    }));
}

export function summarizeProjectUsage(report) {
  if (!report || typeof report !== "object") {
    return null;
  }
  const founded = report.founded !== false;
  const totals = report.totals || {};
  const totalTasks = Number(totals.tasks || 0);
  const recorded = totals.recorded_usage || null;
  const hasRecorded = Boolean(recorded && typeof recorded === "object");
  const input = hasRecorded ? Number(recorded.input || 0) : 0;
  const output = hasRecorded ? Number(recorded.output || 0) : 0;
  const cacheRead = hasRecorded ? Number(recorded.cache_read || 0) : 0;
  const recordedTotal = hasRecorded ? input + output : null;
  const missingExecTasks = Number(totals.tasks_without_execution_usage || 0);
  const estimates = formatCurrencyEstimates(totals.estimates);
  const unpricedTotal = tokenTotal(totals.unpriced_usage);
  const tasks = Array.isArray(report.tasks) ? report.tasks : [];
  const unreconciledCount = tasks.filter((t) => t && t.usage_reconciled === false).length;

  let compactText;
  if (!founded) {
    compactText = "尚未建朝（ziyuan init），手工译稿无模型 token 用量";
  } else if (totalTasks === 0) {
    compactText = "尚无关联模型诏令（手工译稿不计模型 token）";
  } else {
    const tokenText = recordedTotal === null
      ? "用量未知（未报告）"
      : `已记录 ${fmtInt(recordedTotal)} tokens`;
    const estText = estimates.length
      ? estimates.map((e) => `${e.currency} ${e.amount.toFixed(4)}`).join(" · ") + "（估算非账单）"
      : (unpricedTotal > 0 ? `未定价 ${fmtInt(unpricedTotal)} tokens` : "暂无已定价调用");
    compactText = `关联诏令 ${totalTasks} 件 · ${tokenText} · ${estText}`;
  }

  return {
    project: report.project || "",
    founded,
    totalTasks,
    hasRecorded,
    recordedTotal,
    input,
    output,
    cacheRead,
    missingExecTasks,
    unpricedTotal,
    unreconciledCount,
    estimates,
    compactText,
  };
}

export function formatProjectUsageStatus(report) {
  const s = summarizeProjectUsage(report);
  if (!s) return "选择项目后，点「查阅度支账本」汇总本项目四阶段模型用量。";
  if (!s.founded) {
    return "当前工作区尚未建朝（ziyuan init），暂无关联诏令事件簿；纯手工译稿不产生模型用量。";
  }
  if (s.totalTasks === 0) {
    return "当前项目尚无关联的初译、审校或修订诏令记录（已记录小计为空，不代表零调用）。";
  }
  const tokenDesc = s.hasRecorded
    ? `已记录小计 ${fmtInt(s.recordedTotal)} tokens`
    : "尚未收到已报告的模型用量（记为未知，不按零计）";
  return `已核对关联诏令 ${s.totalTasks} 件 · ${tokenDesc}（估价按当前单价分开币种测算，非服务商账单）。`;
}

export function renderProjectUsageLedger({
  doc = globalThis.document,
  report,
  summaryHost,
  stagesHost,
  tasksHost,
  notesHost,
  formatLocation = (id) => String(id || ""),
  onInspectTask = null,
  onOpenBlock = null,
}) {
  if (!summaryHost || !stagesHost || !tasksHost || !notesHost) return;
  summaryHost.replaceChildren();
  stagesHost.replaceChildren();
  tasksHost.replaceChildren();
  notesHost.replaceChildren();
  if (!report) return;

  const summary = summarizeProjectUsage(report);
  const totals = report.totals || {};

  const makeChip = (text, variant = "") => {
    const chip = doc.createElement("span");
    chip.className = variant ? `usage-chip ${variant}` : "usage-chip";
    chip.textContent = text;
    return chip;
  };

  summaryHost.append(makeChip(`关联诏令 ${summary.totalTasks} 件`));
  const statusEntries = Object.entries(totals.statuses || {});
  if (statusEntries.length) {
    const statusText = statusEntries
      .map(([k, v]) => `${taskStatusLabel(k)} ${v}`)
      .join(" · ");
    summaryHost.append(makeChip(statusText));
  }
  if (summary.totalTasks > 0) {
    if (summary.hasRecorded) {
      summaryHost.append(
        makeChip(
          `已记录 ${fmtInt(summary.recordedTotal)} tokens（入 ${fmtInt(summary.input)} / 出 ${fmtInt(summary.output)}${summary.cacheRead > 0 ? ` · 缓存 ${fmtInt(summary.cacheRead)}` : ""}）`,
          "seal",
        ),
      );
    } else {
      summaryHost.append(makeChip("已记录 tokens：未知（未报告）", "warn"));
    }
  }
  for (const est of summary.estimates) {
    summaryHost.append(makeChip(est.label, "gold"));
  }
  if (summary.unpricedTotal > 0) {
    summaryHost.append(makeChip(`未定价 ${fmtInt(summary.unpricedTotal)} tokens`));
  }
  if (summary.missingExecTasks > 0) {
    summaryHost.append(makeChip(`缺承办用量 ${summary.missingExecTasks} 件`, "warn"));
  }
  if (summary.unreconciledCount > 0) {
    summaryHost.append(makeChip(`账目待核 ${summary.unreconciledCount} 件`, "warn"));
  }

  // Four-stage Song-style ledger cards
  const stages = Array.isArray(report.stages) && report.stages.length
    ? report.stages
    : ["translate", "fidelity_review", "style_review", "revise"].map((kind) => ({
        kind,
        totals: { tasks: 0 },
      }));

  for (const stage of stages) {
    const st = stage.totals || {};
    const count = Number(st.tasks || 0);
    const card = doc.createElement("article");
    card.className = count > 0 ? "usage-stage-card is-active" : "usage-stage-card";

    const head = doc.createElement("div");
    head.className = "usage-stage-head";
    const title = doc.createElement("b");
    title.className = "usage-stage-title";
    title.textContent = stageLabel(stage.kind);
    const badge = doc.createElement("span");
    badge.className = count > 0 ? "badge assigned" : "badge";
    badge.textContent = `${count} 件`;
    head.append(title, badge);

    const value = doc.createElement("div");
    value.className = "usage-stage-value";
    if (count === 0) {
      value.textContent = "尚无诏令";
    } else if (st.recorded_usage) {
      value.textContent = `${fmtInt(tokenTotal(st.recorded_usage))} tokens`;
    } else {
      value.textContent = "用量未知";
    }

    const meta = doc.createElement("div");
    meta.className = "usage-stage-meta";
    const lines = [];
    if (st.recorded_usage) {
      const u = st.recorded_usage;
      lines.push(
        `入 ${fmtInt(u.input)} · 出 ${fmtInt(u.output)}${u.cache_read ? ` · 缓存 ${fmtInt(u.cache_read)}` : ""}`,
      );
    } else if (count > 0) {
      lines.push("未收到执行用量事件（不按零计）");
    } else {
      lines.push("未下发此阶段模型诏令");
    }
    const stageEst = formatCurrencyEstimates(st.estimates);
    if (stageEst.length) {
      lines.push(stageEst.map((e) => `${e.currency} ${e.amount.toFixed(6)}`).join(" · ") + "（非账单）");
    } else if (tokenTotal(st.unpriced_usage) > 0) {
      lines.push(`未定价 ${fmtInt(tokenTotal(st.unpriced_usage))} tokens`);
    }
    if (Number(st.tool_calls || 0) > 0 || Number(st.missingExecTasks || st.tasks_without_execution_usage || 0) > 0) {
      const bits = [];
      if (Number(st.tool_calls || 0) > 0) {
        bits.push(`工具 ${st.tool_calls} 次${st.tool_failures ? `（败 ${st.tool_failures}）` : ""}`);
      }
      if (Number(st.tasks_without_execution_usage || 0) > 0) {
        bits.push(`缺承办记录 ${st.tasks_without_execution_usage} 件`);
      }
      lines.push(bits.join(" · "));
    }
    for (const line of lines) {
      const p = doc.createElement("span");
      p.textContent = line;
      meta.append(p);
    }

    card.append(head, value, meta);
    stagesHost.append(card);
  }

  // Collapsible per-task breakdown
  const tasks = Array.isArray(report.tasks) ? report.tasks : [];
  if (tasks.length > 0) {
    const details = doc.createElement("details");
    details.className = "continuity-tools usage-tasks-drawer";
    const sum = doc.createElement("summary");
    sum.textContent = `关联诏令逐件明细（共 ${tasks.length} 件 · 含重试与旧稿）`;
    details.append(sum);

    const ul = doc.createElement("ul");
    ul.className = "usage-task-list";
    for (const item of tasks) {
      const li = doc.createElement("li");
      li.className = "usage-task-item";

      const top = doc.createElement("div");
      top.className = "usage-task-head";

      const left = doc.createElement("div");
      left.className = "usage-task-ident";
      const taskIdNum = typeof item.task === "object" && item.task !== null ? (item.task[0] ?? item.task.id) : item.task;
      if (typeof onInspectTask === "function" && taskIdNum != null) {
        const taskBtn = doc.createElement("button");
        taskBtn.type = "button";
        taskBtn.className = "small";
        taskBtn.dataset.view = "";
        taskBtn.textContent = `#${taskIdNum}`;
        taskBtn.title = "到奉天殿查看此诏令";
        taskBtn.addEventListener("click", () => onInspectTask(taskIdNum));
        left.append(taskBtn);
      } else {
        const span = doc.createElement("b");
        span.textContent = `#${taskIdNum}`;
        left.append(span);
      }

      const stageBadge = doc.createElement("span");
      stageBadge.className = "badge seal";
      stageBadge.textContent = stageLabel(item.kind);
      left.append(stageBadge);

      const stBadge = doc.createElement("span");
      stBadge.className = `badge ${item.status || ""}`;
      stBadge.textContent = taskStatusLabel(item.status);
      left.append(stBadge);

      if (item.block) {
        if (typeof onOpenBlock === "function") {
          const blkBtn = doc.createElement("button");
          blkBtn.type = "button";
          blkBtn.className = "link-button";
          blkBtn.dataset.view = "";
          blkBtn.textContent = formatLocation(item.block);
          blkBtn.addEventListener("click", () => onOpenBlock(item.block));
          left.append(blkBtn);
        } else {
          const blkSpan = doc.createElement("span");
          blkSpan.className = "hint";
          blkSpan.textContent = formatLocation(item.block);
          left.append(blkSpan);
        }
      }

      const right = doc.createElement("div");
      right.className = "usage-task-stats";
      const budgetText = item.budget != null ? `本件限额 ${fmtInt(item.budget)}` : "未设本件限额";
      const execText = item.recorded_execution_usage
        ? formatTokenUsage(item.recorded_execution_usage)
        : "承办用量未知";
      const statsSpan = doc.createElement("span");
      statsSpan.className = "hint";
      statsSpan.textContent = `指派 ${item.attempts ?? 0} 次 · ${budgetText} · ${execText}`;
      right.append(statsSpan);

      if (item.usage_reconciled === false) {
        const warn = doc.createElement("span");
        warn.className = "badge failed";
        warn.textContent = "事件与累计不符";
        right.append(warn);
      }

      top.append(left, right);
      li.append(top);

      const charges = Array.isArray(item.charges) ? item.charges : [];
      if (charges.length > 0) {
        const chargeList = doc.createElement("div");
        chargeList.className = "usage-charge-list";
        for (const ch of charges) {
          const row = doc.createElement("div");
          row.className = "usage-charge-row";
          const model = ch.model || "未知模型";
          const ratePart = ch.rate
            ? `费率 入 ${ch.rate.input_per_million} / 出 ${ch.rate.output_per_million} 每百万（${ch.rate.currency} · ${ch.rate.source}）`
            : "未定价";
          const costPart = ch.estimated_cost != null && ch.rate
            ? ` · 估算 ${Number(ch.estimated_cost).toFixed(6)} ${ch.rate.currency}`
            : "";
          row.textContent = `${chargePurposeLabel(ch.purpose)} · ${model} · ${formatTokenUsage(ch.usage)} · ${ratePart}${costPart}`;
          chargeList.append(row);
        }
        li.append(chargeList);
      }
      ul.append(li);
    }
    details.append(ul);
    tasksHost.append(details);
  }

  // Accounting boundary notes
  const notes = Array.isArray(report.notes) ? report.notes : [];
  if (notes.length > 0) {
    const box = doc.createElement("details");
    box.className = "usage-notes-box";
    const sum = doc.createElement("summary");
    sum.textContent = `记账口径与度支边界说明（${notes.length} 条）`;
    box.append(sum);
    const ul = doc.createElement("ul");
    ul.className = "usage-notes-list";
    for (const raw of notes) {
      const li = doc.createElement("li");
      li.textContent = normalizeUsageNote(raw);
      ul.append(li);
    }
    box.append(ul);
    notesHost.append(box);
  }
}

export function createProjectUsageController({
  doc = globalThis.document,
  getProjectId,
  fetchUsage,
  formatLocation,
  formatError = (e) => String(e?.message || e || "未知错误"),
  onInspectTask = null,
  onOpenBlock = null,
  onReportLoaded = null,
  statusEl,
  summaryHost,
  stagesHost,
  tasksHost,
  notesHost,
}) {
  let state = { project: null, report: null, request: 0 };

  function render() {
    renderProjectUsageLedger({
      doc,
      report: state.report,
      summaryHost,
      stagesHost,
      tasksHost,
      notesHost,
      formatLocation,
      onInspectTask,
      onOpenBlock,
    });
  }

  function reset(projectId) {
    if (state.project === projectId) return;
    state = { project: projectId, report: null, request: state.request + 1 };
    if (statusEl) {
      statusEl.textContent = projectId
        ? "点「查阅度支账本」按事件簿汇总本项目四阶段模型用量与估价。"
        : "选择项目后，点「查阅度支账本」查看四阶段模型用量与估价。";
    }
    summaryHost?.replaceChildren();
    stagesHost?.replaceChildren();
    tasksHost?.replaceChildren();
    notesHost?.replaceChildren();
  }

  async function run() {
    const project = getProjectId();
    if (!project) {
      if (statusEl) statusEl.textContent = "请先选择项目。";
      return null;
    }
    const req = ++state.request;
    if (statusEl) statusEl.textContent = "正在核对本项目关联诏令与事件簿用量…";
    try {
      const report = await fetchUsage(project);
      if (req !== state.request || project !== getProjectId()) return null;
      state = { project, report, request: req };
      if (statusEl) statusEl.textContent = formatProjectUsageStatus(report);
      render();
      if (typeof onReportLoaded === "function") onReportLoaded(project, report);
      return report;
    } catch (e) {
      if (req !== state.request) return null;
      if (statusEl) statusEl.textContent = "读取度支账本失败：" + formatError(e);
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

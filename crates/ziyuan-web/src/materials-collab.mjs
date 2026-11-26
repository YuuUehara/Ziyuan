// Local materials collaboration helpers: local TM CRUD, review comments, single-block QA,
// draft-vs-production diff formatting, visibility report, and paragraph associations.
// Shared between `/assets/materials-collab.mjs` and `node --test`.

import { QA_CODE_LABELS } from "./project-qa.mjs";

const DIFF_FIELD_NAMES = Object.freeze({
  allowed: "允许译法",
  source: "原文",
  category: "类别",
  note: "备注",
  scope: "范围",
  available_from: "可用起点",
  evidence: "证据",
  status: "状态",
  name: "姓名",
  reading: "读音",
  aliases: "别名",
  terms: "关联术语",
  address_rules: "称谓规则",
  voice: "声线",
});

const DIFF_KIND_NAMES = Object.freeze({
  added: "新增",
  removed: "删除",
  changed: "改动",
});

export function formatMaterialAdvice(message) {
  const msg = String(message || "");
  if (/请求身份已被不同内容使用|修订冲突|冲突/.test(msg)) {
    return msg + " 下一步：重新读取服务器资料；当前输入可先导出备份，再决定保留哪一份。";
  }
  if (/尚无本地资料草稿/.test(msg)) {
    return msg + " 下一步：先新增一条术语或人物，保存草稿，再准备生产资料。";
  }
  if (/不是有效 JSON|JSON 无效|格式不对/.test(msg)) {
    return msg + " 下一步：检查括号和逗号；可先导出备份再修改。";
  }
  if (/准备.*失败|准备生产资料/.test(msg)) {
    return msg + " 下一步：先保存并通过校验，再点「准备生产资料」。";
  }
  return msg;
}

export function formatMaterialsDiffReport(diff) {
  const lines = [];
  lines.push(
    `草稿修订 ${diff?.draft_revision ?? ""} · 已批准 ${diff?.approved_version ?? "无"}（历史 ${(diff?.approved_versions || []).join("、")}）`,
  );
  if (diff?.legacy_production) {
    lines.push("生产仍是旧数组资料，结构对比不可用。");
  }
  const counts = {};
  const allChanges = [...(diff?.terms || []), ...(diff?.persons || [])];
  for (const change of allChanges) {
    counts[change.kind] = (counts[change.kind] || 0) + 1;
    const kind = DIFF_KIND_NAMES[change.kind] || change.kind;
    const fields = (change.fields || []).map((f) => DIFF_FIELD_NAMES[f] || f);
    lines.push(`${change.id}：${kind}${fields.length ? `（${fields.join("、")}）` : ""}`);
  }
  if (Object.keys(counts).length) {
    lines.splice(
      1,
      0,
      "摘要：" + Object.entries(counts).map(([k, v]) => `${DIFF_KIND_NAMES[k] || k} ${v}`).join(" · "),
    );
  }
  if (!allChanges.length) {
    lines.push("草稿与已批准资料一致。");
  }
  return lines.join("\n");
}

export function parseMaterialsAssociations(assocMap) {
  const out = [];
  if (!assocMap) return out;
  for (const [paragraph, value] of assocMap) {
    const persons = String(value?.persons || "")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const targets = String(value?.targets || "")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const speakerText = String(value?.speaker || "").trim();
    let speaker;
    if (speakerText === "narration") speaker = { kind: "narration" };
    else if (speakerText === "unknown") speaker = { kind: "unknown" };
    else if (speakerText) speaker = { kind: "person", person_id: speakerText };
    if (persons.length || targets.length || speaker) {
      out.push({ paragraph, persons, targets, speaker });
    }
  }
  return out;
}

export function renderMaterialsVisibilityReport({
  doc = globalThis.document,
  host,
  result,
  formatLocation = (id) => String(id || ""),
  sectionTitles = {
    applicable: "适用（applicable）",
    automatic: "自动候选（automatic）",
    diagnostics: "诊断（diagnostics）",
  },
}) {
  if (!host) return;
  host.replaceChildren();
  const report = result?.report;
  if (!report) {
    host.textContent = "报告为空。";
    return;
  }
  const addSection = (title, items) => {
    const box = doc.createElement("div");
    box.className = "text-material";
    const h = doc.createElement("b");
    h.textContent = title;
    box.append(h);
    const list = doc.createElement("ul");
    list.className = "text-problems";
    for (const item of items) {
      const li = doc.createElement("li");
      li.textContent = item;
      list.append(li);
    }
    box.append(list);
    host.append(box);
  };
  const statusText = (s) => ({ tentative: "暂定", confirmed: "已确认" }[s] || s);
  const kindText = (k) => ({ term: "术语", person: "人物" }[k] || k);
  const parasText = (ps) => (ps || []).map((p) => formatLocation(p)).join("、");
  const formatSelection = (selection) => {
    const items = [];
    for (const term of selection?.terms || []) {
      items.push(`术语 ${term.term_id}（${statusText(term.status)}）：${parasText(term.paragraphs)}`);
    }
    for (const person of selection?.persons || []) {
      items.push(`人物 ${person.person_id}（${statusText(person.status)}）：${parasText(person.paragraphs)}`);
    }
    for (const rule of selection?.address_rules || []) {
      items.push(`称呼 ${rule.person_id}#${rule.rule_index}：${parasText(rule.paragraphs)}`);
    }
    return items;
  };
  addSection(sectionTitles.applicable, formatSelection(report.applicable));
  addSection(sectionTitles.automatic, formatSelection(report.automatic));
  addSection(
    sectionTitles.diagnostics,
    (report.diagnostics || []).map(
      (d) => `${kindText(d.identity?.kind || "")} ${d.identity?.id || ""} — ${d.reason || ""}：${parasText(d.paragraphs)}`,
    ),
  );
}

export function renderBlockQaIssues({
  doc = globalThis.document,
  host,
  issues = [],
  filter = "",
  formatLocation = (id) => String(id || ""),
  onNavigate = () => {},
}) {
  if (!host) return;
  host.replaceChildren();
  const filtered = (issues || []).filter((i) => !filter || i.code === filter);
  if (!filtered.length) {
    host.textContent = "没有符合筛选的确定性检查问题（不代表语义正确）。";
    return;
  }
  for (const issue of filtered) {
    const row = doc.createElement("label");
    row.className = "hint";
    const box = doc.createElement("input");
    box.type = "checkbox";
    const text = doc.createElement("span");
    const codeLabel = QA_CODE_LABELS[issue.code] || issue.code;
    const loc = issue.paragraph ? `（${formatLocation(issue.paragraph)}）` : "";
    text.textContent = ` ${codeLabel}${loc}：${issue.message} `;
    row.append(box, text);
    if (issue.paragraph) {
      const go = doc.createElement("button");
      go.type = "button";
      go.className = "link-button";
      go.textContent = "定位";
      go.addEventListener("click", () => onNavigate(issue.paragraph));
      row.append(go);
    }
    host.append(row);
  }
}

export function renderLocalTmPanel({
  doc = globalThis.document,
  host,
  statusEl,
  project,
  entries = [],
  getCurrentProject = () => project,
  apiCall,
  confirmAction = (msg) => globalThis.confirm(msg),
  onReload = () => {},
}) {
  if (!host) return;
  host.replaceChildren();
  const check = () => {
    if (project !== getCurrentProject()) {
      throw new Error("项目已变，请重新读取翻译记忆");
    }
  };
  const runTask = async (fn) => {
    try {
      check();
      await fn();
      if (statusEl) statusEl.textContent = "翻译记忆已更新；未修改译稿。";
      if (project === getCurrentProject()) onReload();
    } catch (e) {
      if (statusEl) statusEl.textContent = e?.message || String(e);
    }
  };
  const makeInput = (label, value = "") => {
    const e = doc.createElement("input");
    e.type = "text";
    e.placeholder = label;
    e.setAttribute("aria-label", label);
    e.value = value;
    return e;
  };
  const makeBtn = (label, fn) => {
    const e = doc.createElement("button");
    e.type = "button";
    e.className = "small write";
    e.textContent = label;
    e.addEventListener("click", () => runTask(fn));
    return e;
  };

  const base = `/text/${encodeURIComponent(project)}`;
  const form = doc.createElement("div");
  form.className = "row write";
  const source = makeInput("记忆原文");
  const target = makeInput("认可译法");
  form.append(
    source,
    target,
    makeBtn("新增记忆", () => apiCall(`${base}/tm`, { source: source.value, target: target.value })),
    makeBtn("从当前有效定稿建库", () => apiCall(`${base}/tm/build`, {})),
  );
  host.append(form);

  if (!entries.length) {
    const empty = doc.createElement("p");
    empty.className = "hint";
    empty.textContent = "翻译记忆为空，可新增原译文对或从定稿建库。";
    host.append(empty);
    return;
  }

  for (const entry of entries) {
    const entryBase = `${base}/tm/${encodeURIComponent(entry.id)}`;
    const row = doc.createElement("article");
    row.className = "continuity-change";
    const prose = doc.createElement("p");
    prose.textContent = `${entry.source} → ${entry.target} · 采纳 ${entry.adopted}`;
    row.append(prose);

    const controls = doc.createElement("div");
    controls.className = "row write";
    controls.append(makeBtn("采纳", () => apiCall(`${entryBase}/adopt`, {})));

    const edit = doc.createElement("details");
    edit.className = "write";
    const summary = doc.createElement("summary");
    summary.textContent = "修改这条记忆";
    const original = makeInput("记忆原文", entry.source);
    const translation = makeInput("认可译法", entry.target);
    const actions = doc.createElement("div");
    actions.className = "row";
    actions.append(
      makeBtn("保存记忆", () => apiCall(`${entryBase}/update`, { source: original.value, target: translation.value })),
      makeBtn("删除记忆", async () => {
        if (!confirmAction("删除这条翻译记忆？译稿不受影响。")) return;
        await apiCall(`${entryBase}/delete`, {});
      }),
    );
    edit.append(summary, original, translation, actions);
    row.append(controls, edit);
    host.append(row);
  }
}

export function renderMaterialsCommentsList({
  doc = globalThis.document,
  host,
  statusEl,
  project,
  comments = [],
  getCurrentProject = () => project,
  formatLocation = (id) => String(id || ""),
  promptText = (msg, def = "") => globalThis.prompt(msg, def),
  apiCall,
  onReload = () => {},
}) {
  if (!host) return;
  host.replaceChildren();
  if (!comments.length) {
    host.textContent = "暂无评论。";
    return;
  }
  const base = `/text/${encodeURIComponent(project)}/comments`;
  for (const c of comments) {
    const row = doc.createElement("p");
    row.className = "hint";
    row.textContent =
      `${c.id}${c.parent_id ? " ↳" + c.parent_id : ""} · ${formatLocation(c.paragraph || c.block)} · ` +
      `${c.resolved ? "已解决" : "未解决"}${c.assignee ? " · 指派 " + c.assignee : ""} · ${c.body} `;

    const cBase = `${base}/${encodeURIComponent(c.id)}`;
    const done = doc.createElement("button");
    done.type = "button";
    done.className = "link-button write";
    done.textContent = c.resolved ? "重新打开" : "标记解决";
    done.addEventListener("click", async () => {
      if (project !== getCurrentProject()) return;
      try {
        await apiCall(cBase + (c.resolved ? "/reopen" : "/resolve"), {});
        if (statusEl) statusEl.textContent = "评论状态已更新；未修改译稿。";
        await onReload();
      } catch (e) {
        if (statusEl) statusEl.textContent = "评论操作失败：" + (e?.body?.error || e?.message || e);
      }
    });

    const reply = doc.createElement("button");
    reply.type = "button";
    reply.className = "link-button write";
    reply.textContent = "回复";
    reply.addEventListener("click", async () => {
      const text = promptText("回复内容");
      if (!text || project !== getCurrentProject()) return;
      try {
        await apiCall(`${cBase}/reply`, { body: text });
        if (statusEl) statusEl.textContent = "已回复。";
        await onReload();
      } catch (e) {
        if (statusEl) statusEl.textContent = "回复失败：" + (e?.body?.error || e?.message || e);
      }
    });

    const assign = doc.createElement("button");
    assign.type = "button";
    assign.className = "link-button write";
    assign.textContent = "指派";
    assign.addEventListener("click", async () => {
      const who = promptText("负责人", c.assignee || "");
      if (who === null || project !== getCurrentProject()) return;
      try {
        await apiCall(`${cBase}/assign`, { assignee: who });
        if (statusEl) statusEl.textContent = "已指派（空字符串表示取消）。";
        await onReload();
      } catch (e) {
        if (statusEl) statusEl.textContent = "指派失败：" + (e?.body?.error || e?.message || e);
      }
    });

    row.append(done, reply, assign);
    host.append(row);
  }
}

const TERM_CATEGORY_OPTIONS = Object.freeze([
  ["common", "通用"],
  ["person_name", "人名"],
  ["place", "地名"],
  ["organization", "组织"],
  ["title", "官职 / 头衔"],
  ["culture", "文化专词"],
]);

function defaultCategoryForReason(reason) {
  const text = String(reason || "");
  if (text.includes("片假名") || text.includes("专名")) return "person_name";
  return "common";
}

export function renderTermProposalsPanel({
  doc = globalThis.document,
  host,
  statusEl,
  report,
  project,
  viewer = false,
  getCurrentProject = () => project,
  formatLocation = (id) => String(id || ""),
  onNavigate = () => {},
  apiCall,
  onMerged = () => {},
}) {
  if (!host) return;
  host.replaceChildren();
  if (!report) return;

  const list = Array.isArray(report.candidates) ? report.candidates : [];
  const panel = doc.createElement("div");
  panel.className = "proposal-panel";

  const head = doc.createElement("div");
  head.className = "proposal-head";
  const title = doc.createElement("span");
  title.className = "proposal-head-title";
  title.textContent = `候选术语审定（共 ${list.length} 条）`;
  head.append(title);

  const rowRecords = [];

  if (!viewer && list.length > 0 && typeof apiCall === "function") {
    const batchBtn = doc.createElement("button");
    batchBtn.type = "button";
    batchBtn.className = "small primary write";
    batchBtn.textContent = "批量收入已填译法的候选词";
    batchBtn.addEventListener("click", async () => {
      if (project !== getCurrentProject()) return;
      const pending = rowRecords.filter((r) => !r.merged && r.targetInput.value.trim());
      if (!pending.length) {
        if (statusEl) statusEl.textContent = "请先在至少一条候选词后填写认可译法，再点批量收入。";
        return;
      }
      batchBtn.disabled = true;
      let count = 0;
      try {
        for (const r of pending) {
          if (project !== getCurrentProject()) break;
          const receipt = await r.submitSingle(false);
          if (receipt) count += 1;
        }
        if (count > 0) {
          if (statusEl) {
            statusEl.textContent = `已批量收入 ${count} 条候选术语到本地资料草稿（暂定状态，未批准生效）。`;
          }
          await onMerged();
        }
      } finally {
        batchBtn.disabled = false;
      }
    });
    head.append(batchBtn);
  }
  panel.append(head);

  const boundary = doc.createElement("p");
  boundary.className = "hint";
  boundary.textContent = report.written
    ? `已写入 ${report.path}。候选词不会自动进入生产上下文；收入草稿后仍为「暂定」本地草稿，须经校验、准备并「批准上下文」才生效。`
    : "只读预览当前块的候选术语（未写入磁盘，也不改动任何草稿或生产上下文）。";
  panel.append(boundary);

  if (!list.length) {
    const empty = doc.createElement("p");
    empty.className = "hint";
    empty.textContent = "当前块未发现尚未收录的新候选术语。";
    panel.append(empty);
    host.append(panel);
    return;
  }

  const ul = doc.createElement("ul");
  ul.className = "proposal-list";

  for (const cand of list) {
    const li = doc.createElement("li");
    li.className = "proposal-card";

    const meta = doc.createElement("div");
    meta.className = "proposal-meta";
    const src = doc.createElement("span");
    src.className = "proposal-source";
    src.textContent = cand.source;
    const countBadge = doc.createElement("span");
    countBadge.className = "badge";
    countBadge.textContent = `出现 ${cand.count ?? 1} 次`;
    const reasonBadge = doc.createElement("span");
    reasonBadge.className = "badge seal";
    reasonBadge.textContent = cand.reason || "候选专名";
    meta.append(src, countBadge, reasonBadge);

    const paras = Array.isArray(cand.paragraphs) ? cand.paragraphs : [];
    if (paras.length) {
      const paraWrap = doc.createElement("span");
      paraWrap.className = "proposal-paras";
      for (const pid of paras) {
        const jump = doc.createElement("button");
        jump.type = "button";
        jump.className = "link-button";
        jump.dataset.view = "";
        jump.textContent = formatLocation(pid, true);
        jump.title = `定位到 ${formatLocation(pid, false)}`;
        jump.addEventListener("click", () => onNavigate(pid));
        paraWrap.append(jump);
      }
      meta.append(paraWrap);
    }

    const stateBadge = doc.createElement("span");
    stateBadge.className = "badge accept";
    stateBadge.hidden = true;
    meta.append(stateBadge);
    li.append(meta);

    if (!viewer && typeof apiCall === "function") {
      const controls = doc.createElement("div");
      controls.className = "proposal-controls write";

      const targetInput = doc.createElement("input");
      targetInput.type = "text";
      targetInput.placeholder = "填写认可译法（必填）";
      targetInput.setAttribute("aria-label", `${cand.source} 的认可译法`);

      const catSelect = doc.createElement("select");
      catSelect.setAttribute("aria-label", `${cand.source} 的术语分类`);
      const defCat = defaultCategoryForReason(cand.reason);
      for (const [val, label] of TERM_CATEGORY_OPTIONS) {
        const opt = doc.createElement("option");
        opt.value = val;
        opt.textContent = label;
        if (val === defCat) opt.selected = true;
        catSelect.append(opt);
      }
      catSelect.value = defCat;

      const noteInput = doc.createElement("input");
      noteInput.type = "text";
      noteInput.placeholder = "备注（可选）";
      noteInput.setAttribute("aria-label", `${cand.source} 的备注`);

      const addBtn = doc.createElement("button");
      addBtn.type = "button";
      addBtn.className = "small primary write";
      addBtn.textContent = "收入草稿";

      const record = {
        merged: false,
        targetInput,
        async submitSingle(refreshAfter = true) {
          if (project !== getCurrentProject()) return null;
          const target = targetInput.value.trim();
          if (!target) {
            if (statusEl) statusEl.textContent = `请先填写「${cand.source}」的译法，再收入草稿。`;
            return null;
          }
          addBtn.disabled = true;
          try {
            const noteVal = noteInput.value.trim() || `${cand.reason || "候选提取"}（本块 ${cand.count ?? 1} 次）`;
            const receipt = await apiCall(`/text/${encodeURIComponent(project)}/materials/quick-add`, {
              kind: "term",
              source: cand.source,
              target,
              category: catSelect.value,
              note: noteVal,
            });
            record.merged = true;
            li.className = "proposal-card is-merged";
            stateBadge.hidden = false;
            stateBadge.textContent = receipt?.updated_existing
              ? `已更新草稿 · ${receipt.id}`
              : `已入草稿 · ${receipt?.id || "暂定"}`;
            targetInput.disabled = true;
            catSelect.disabled = true;
            noteInput.disabled = true;
            addBtn.textContent = "已收入";
            if (refreshAfter) {
              if (statusEl) {
                statusEl.textContent = `已将「${cand.source} → ${target}」收入本地资料草稿（暂定，未批准）。`;
              }
              await onMerged();
            }
            return receipt || { ok: true };
          } catch (e) {
            addBtn.disabled = false;
            if (statusEl) {
              statusEl.textContent = `收入草稿失败：${formatMaterialAdvice(e?.body?.error || e?.message || e)}`;
            }
            return null;
          }
        },
      };

      addBtn.addEventListener("click", () => record.submitSingle(true));
      controls.append(targetInput, catSelect, noteInput, addBtn);
      li.append(controls);
      rowRecords.push(record);
    }

    ul.append(li);
  }

  panel.append(ul);
  host.append(panel);
}

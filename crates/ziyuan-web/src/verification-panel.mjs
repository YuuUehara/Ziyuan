// Six-aspect paragraph verification ("逐段勘校") & lexical evidence ("词证登记") panel.
// Shared between `/assets/verification-panel.mjs` and `node --test`.

export const SEMANTIC_CATEGORIES = Object.freeze({
  subject_speaker: {
    title: "言行归属",
    note: "核对谁在说话、谁在做事，勿张冠李戴。",
    example: "“清替我换钱”，不是“我替清换钱”。",
  },
  agency_negation: {
    title: "事理正反",
    note: "核对动作方向与肯定、否定，勿将原意颠倒。",
    example: "“借给他”不是“向他借”；“并非主任”不能漏掉“并非”。",
  },
  quantity: {
    title: "数目分寸",
    note: "核对金额、人数、约数与倍数，勿增减失度。",
    example: "“约三圆”不宜写成“恰好三圆”。",
  },
  timeline: {
    title: "前后次第",
    note: "核对时间先后与事情是否已经发生。",
    example: "“来年暑假回去”，不是“今年暑假回过”。",
  },
  names_relations: {
    title: "称谓名分",
    note: "核对人名、职务、昵称与人物关系。",
    example: "“副校长”不可误作“数学主任”。",
  },
  humor_additions: {
    title: "谐趣留白",
    note: "核对笑点、反话与含蓄之处，勿将解释写成原文。",
    example: "人物本来不懂一个绰号，不能替他补上一段词典释义。",
  },
});

export const VERIFICATION_DISPOSITIONS = Object.freeze({
  accepted: "接受",
  rejected: "驳回",
  pending: "待查",
  not_applicable: "不适用",
});

export const VERIFICATION_RESOLUTIONS = Object.freeze({
  unchecked: "未检查",
  resolved: "已解决",
  unresolved: "未解决",
  not_applicable: "不适用",
});

export const EVIDENCE_STATES = Object.freeze({
  tentative: "暂定",
  confirmed: "已确认",
});

export const EVIDENCE_FIELDS = Object.freeze({
  quote: "原文短引",
  source: "出处（网址或书目信息）",
  locator: "出处位置（词条、页码等）",
  explanation: "词义核对说明",
  context: "适用语境",
});

export function createInitialVerificationDraft(fingerprint = "") {
  return {
    fingerprint,
    checks: Object.keys(SEMANTIC_CATEGORIES).map((category) => ({
      category,
      disposition: "pending",
      resolution: "unchecked",
      note: "",
      task: "",
      quote: "",
      evidence: "",
    })),
    evidence: {
      quote: "",
      source: "",
      locator: "",
      explanation: "",
      context: "",
      status: "tentative",
    },
  };
}

export function buildVerificationRequestBody({ paragraph, draft, viewFingerprint }) {
  if (!draft || draft.fingerprint !== viewFingerprint) {
    throw new Error("材料或记录已更新，请先阅读最新内容，再点「已读最新材料，继续核对」。");
  }
  const checks = draft.checks.map((c) => {
    const hasTask = Boolean(c.task && c.task.trim());
    const hasQuote = Boolean(c.quote && c.quote.trim());
    if (
      hasTask !== hasQuote ||
      (hasTask && (!/^[1-9]\d*$/.test(c.task.trim()) || !Number.isSafeInteger(Number(c.task.trim()))))
    ) {
      throw new Error("请同时填写审校诏令号和该条意见的原句；诏令号须为正整数。");
    }
    const ids = c.evidence && c.evidence.trim()
      ? c.evidence.split(",").map((n) => n.trim())
      : [];
    if (ids.some((n) => !/^[1-9]\d*$/.test(n) || !Number.isSafeInteger(Number(n)))) {
      throw new Error("词证编号须为正整数；多个编号用英文逗号分隔，如 1,2。");
    }
    return {
      category: c.category,
      disposition: c.disposition,
      resolution: c.resolution,
      note: c.note,
      advice: hasTask ? [{ task: Number(c.task.trim()), quote: c.quote }] : [],
      evidence: ids.map(Number),
    };
  });
  return {
    paragraph,
    fingerprint: draft.fingerprint,
    checks,
  };
}

function el(doc, tag, className = "", text = null) {
  const node = doc.createElement(tag);
  if (className) node.className = className;
  if (text !== null) node.textContent = text;
  return node;
}

function buildSelect(doc, optionsMap, selectedValue, attrs = {}) {
  const select = doc.createElement("select");
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "dataset") {
      for (const [dk, dv] of Object.entries(v)) select.dataset[dk] = String(dv);
    } else {
      select[k] = v;
    }
  }
  for (const [val, label] of Object.entries(optionsMap)) {
    const opt = doc.createElement("option");
    opt.value = val;
    opt.textContent = label;
    opt.selected = val === selectedValue;
    select.append(opt);
  }
  if (selectedValue) select.value = selectedValue;
  return select;
}

export function renderVerificationFormDom({ doc = globalThis.document, host, draft }) {
  if (!host || !draft) return;
  host.replaceChildren();
  const wrap = el(doc, "div", "write");
  wrap.append(
    el(doc, "h4", "", "逐段勘校"),
    el(doc, "p", "hint", "分六项记录核对结果与存疑，意见处理与问题状态分开填；保存只作核对记录，不代准奏或定稿。"),
  );
  const grid = el(doc, "div", "verification-grid");
  draft.checks.forEach((c, i) => {
    const meta = SEMANTIC_CATEGORIES[c.category] || { title: c.category, note: "", example: "" };
    const fs = el(doc, "fieldset");
    fs.append(
      el(doc, "legend", "", meta.title),
      el(doc, "p", "hint", meta.note),
      el(doc, "p", "hint", `例：${meta.example}`),
    );
    const row1 = el(doc, "div", "row");
    const lblDisp = el(doc, "label", "", "意见处理");
    lblDisp.append(buildSelect(doc, VERIFICATION_DISPOSITIONS, c.disposition, { dataset: { vcheck: i, vfield: "disposition" } }));
    const lblRes = el(doc, "label", "", "问题状态");
    lblRes.append(buildSelect(doc, VERIFICATION_RESOLUTIONS, c.resolution, { dataset: { vcheck: i, vfield: "resolution" } }));
    row1.append(lblDisp, lblRes);

    const lblNote = el(doc, "label", "", "理由／核对说明");
    const taNote = el(doc, "textarea");
    taNote.dataset.vcheck = String(i);
    taNote.dataset.vfield = "note";
    taNote.value = c.note || "";
    lblNote.append(taNote);

    const row2 = el(doc, "div", "row");
    const lblTask = el(doc, "label", "", "审校诏令号（可选）");
    const inTask = el(doc, "input");
    inTask.type = "text";
    inTask.inputMode = "numeric";
    inTask.dataset.vcheck = String(i);
    inTask.dataset.vfield = "task";
    inTask.value = c.task || "";
    lblTask.append(inTask);

    const lblEv = el(doc, "label", "", "关联词证编号（逗号分隔，可选）");
    const inEv = el(doc, "input");
    inEv.type = "text";
    inEv.dataset.vcheck = String(i);
    inEv.dataset.vfield = "evidence";
    inEv.value = c.evidence || "";
    lblEv.append(inEv);
    row2.append(lblTask, lblEv);

    const lblQuote = el(doc, "label", "", "审校意见原句（填写诏令号时必填）");
    const taQuote = el(doc, "textarea");
    taQuote.dataset.vcheck = String(i);
    taQuote.dataset.vfield = "quote";
    taQuote.value = c.quote || "";
    lblQuote.append(taQuote);

    fs.append(row1, lblNote, row2, lblQuote);
    grid.append(fs);
  });
  wrap.append(grid);

  const actions = el(doc, "div", "actions");
  const btnSave = el(doc, "button", "small", "保存本段六项复核");
  btnSave.type = "button";
  btnSave.id = "verification-save";
  const btnRefresh = el(doc, "button", "small", "已读最新材料，继续核对");
  btnRefresh.type = "button";
  btnRefresh.id = "verification-refresh";
  actions.append(btnSave, btnRefresh);
  wrap.append(actions);

  const evDetails = el(doc, "details", "continuity-tools");
  evDetails.append(
    el(doc, "summary", "", "添加词证"),
    el(doc, "p", "hint", "记录词义出处和说明（网址仅作记录，不自动打开；保存后保留原记录）。"),
  );
  for (const [key, label] of Object.entries(EVIDENCE_FIELDS)) {
    const lbl = el(doc, "label", "", label);
    const ta = el(doc, "textarea");
    ta.dataset.vevidence = key;
    ta.value = draft.evidence[key] || "";
    lbl.append(ta);
    evDetails.append(lbl);
  }
  const lblState = el(doc, "label", "", "词证状态");
  lblState.append(buildSelect(doc, EVIDENCE_STATES, draft.evidence.status, { dataset: { vevidence: "status" } }));
  const evActions = el(doc, "div", "actions");
  const btnEvSave = el(doc, "button", "small", "保存词证");
  btnEvSave.type = "button";
  btnEvSave.id = "evidence-save";
  evActions.append(btnEvSave);
  evDetails.append(lblState, evActions);

  wrap.append(evDetails);
  host.append(wrap);
}

export function renderVerificationFactsDom({
  doc = globalThis.document,
  host,
  view,
  paragraph,
  info,
  finalized = false,
  formatLocation = (id) => String(id || ""),
}) {
  if (!host || !view) return;
  host.replaceChildren();
  const paraText = (block) => block?.paragraphs?.find((p) => p.id === paragraph)?.text || "（无）";

  host.append(
    el(
      doc,
      "p",
      "",
      `本段最新复核：${info?.latest ? "#" + info.latest : "尚无记录"} · 尚待核对：${info?.unresolved ?? 6} 项`,
    ),
    el(
      doc,
      "p",
      "",
      `结构检查：${view.structure ? (view.structure.valid ? "通过（不判断译文对错）" : "未通过") : "尚无译稿"} · 关联意见：${info?.opinions_handled ? "已处理" : "未处理"} · 人工复核：${info?.operator_reviewed ? "已复核" : "未复核"} · 御览定稿：${finalized ? "有效" : "尚无有效定稿"}${info?.latest && !info.current ? " · 旧记录须重新核对" : ""}`,
    ),
    el(doc, "p", "text-para", `原文：${paraText(view.source)}`),
    el(doc, "p", "text-para", `现行译文：${paraText(view.translation)}`),
  );

  const zhupiDet = el(doc, "details");
  zhupiDet.append(
    el(doc, "summary", "", `本段朱批 · 第 ${view.zhupi?.version ?? 0} 版`),
    el(doc, "pre", "", view.zhupi?.items?.[paragraph]?.text || "本段没有朱批。"),
  );
  host.append(zhupiDet);

  host.append(el(doc, "h4", "", "模型审校意见（仅供参考）"));
  const reviews = Array.isArray(view.reviews) ? view.reviews : [];
  if (!reviews.length) {
    host.append(el(doc, "p", "hint", "暂无模型审校意见"));
  } else {
    for (const r of reviews) {
      const det = el(doc, "details");
      const kindLabel = { review_fidelity: "忠实审校", review_style: "文风审校" }[r.kind] || String(r.kind || "");
      det.append(el(doc, "summary", "", `诏令 #${r.task} · ${kindLabel}`), el(doc, "pre", "", r.text || ""));
      host.append(det);
    }
  }

  host.append(
    el(doc, "h4", "", "已批准词表提示"),
    el(doc, "p", "hint", "这里只核对字面用词，是否适合本段仍需你判断。"),
  );
  const paraHints = (Array.isArray(view.hints) ? view.hints : []).filter((h) => h.paragraph === paragraph);
  if (!paraHints.length) {
    host.append(el(doc, "p", "hint", "暂无提示"));
  } else {
    for (const h of paraHints) {
      host.append(
        el(
          doc,
          "p",
          "",
          `${h.source} → ${(h.allowed || []).join(" / ")} · 适用范围：${h.scope || "未注明"} · 说明：${h.note || "无"} · ${h.message}`,
        ),
      );
    }
  }

  host.append(el(doc, "h4", "", "本块词证"));
  const evidences = Array.isArray(view.evidence) ? view.evidence : [];
  if (!evidences.length) {
    host.append(el(doc, "p", "hint", "尚无词证"));
  } else {
    for (const e of evidences) {
      const det = el(doc, "details");
      det.append(
        el(
          doc,
          "summary",
          "",
          `词证 #${e.id} · ${formatLocation(e.details?.paragraph)} · ${EVIDENCE_STATES[e.details?.status] || e.details?.status || ""}`,
        ),
      );
      const dl = el(doc, "dl");
      for (const [key, label] of Object.entries(EVIDENCE_FIELDS)) {
        dl.append(el(doc, "dt", "", label), el(doc, "dd", "", e.details?.[key] || ""));
      }
      det.append(dl);
      host.append(det);
    }
  }

  host.append(el(doc, "h4", "", "复核历史（保留原记录）"));
  const records = Array.isArray(view.records) ? view.records : [];
  if (!records.length) {
    host.append(el(doc, "p", "hint", "尚无复核记录，不能视为已检查"));
  } else {
    for (const r of records) {
      const det = el(doc, "details");
      const prevText = r.record?.previous ? `前次 #${r.record.previous}` : "首次记录";
      det.append(
        el(
          doc,
          "summary",
          "",
          `复核 #${r.record?.id} · ${formatLocation(r.record?.paragraph)} · ${r.current ? "对应当前材料" : "旧材料 · 需重新核对"} · ${prevText}`,
        ),
        el(doc, "p", "", r.reason || ""),
        el(doc, "p", "text-para", `记录时的原文：${r.record?.source || ""}`),
        el(doc, "p", "text-para", `记录时的译文：${r.record?.translation || ""}`),
        el(doc, "p", "text-para", `记录时的朱批：${r.record?.zhupi || "无"}`),
      );
      if (r.change) {
        det.append(
          el(doc, "p", "text-para", `实际修订前：${r.change.before || ""}`),
          el(doc, "p", "text-para", `实际修订后：${r.change.after || ""}`),
        );
      }
      const ul = el(doc, "ul", "text-glossary");
      for (const c of r.record?.checks || []) {
        const li = el(doc, "li");
        const title = el(doc, "b", "", SEMANTIC_CATEGORIES[c.category]?.title || c.category);
        li.append(
          title,
          ` · 意见处理：${VERIFICATION_DISPOSITIONS[c.disposition] || c.disposition} · 问题状态：${VERIFICATION_RESOLUTIONS[c.resolution] || c.resolution}`,
          el(doc, "p", "text-para", c.note || "未填写核对说明"),
        );
        for (const a of c.advice || []) {
          li.append(el(doc, "p", "text-para", `审校诏令 #${a.task}：${a.quote}`));
        }
        if (Array.isArray(c.evidence) && c.evidence.length) {
          li.append(el(doc, "p", "", `关联词证：${c.evidence.map((id) => "#" + id).join("、")}`));
        }
        ul.append(li);
      }
      det.append(ul);
      host.append(det);
    }
  }
}

export function createVerificationController({
  doc = globalThis.document,
  getScope = () => ({ project: "", block: "" }),
  getBlockStatus = () => null,
  isViewer = () => false,
  readAssociations = () => [],
  apiCall,
  toast = () => {},
  formatLocation = (id) => String(id || ""),
  getElement = (id) => doc.getElementById(id),
  onViewChange = () => {},
}) {
  const drafts = new Map();
  let scope = "";
  let view = null;
  let errorMsg = "";
  let requestSeq = 0;
  let paragraph = "";
  let formKey = "";
  let busy = false;
  let boundPanel = null;

  function currentScope() {
    const s = getScope();
    return `${s.project || ""}/${s.block || ""}`;
  }

  function baseUrl() {
    const s = getScope();
    return `/text/${encodeURIComponent(s.project || "")}/blocks/${encodeURIComponent(s.block || "")}`;
  }

  function getDraft() {
    const key = `${scope}/${paragraph}`;
    if (!drafts.has(key)) {
      drafts.set(key, createInitialVerificationDraft(view?.fingerprint || ""));
    }
    return drafts.get(key);
  }

  function bindPanelEvents(panel) {
    if (!panel || boundPanel === panel) return;
    boundPanel = panel;
    const saveInput = (e) => {
      if (!view || !paragraph) return;
      const d = getDraft();
      const target = e.target;
      if (target?.dataset?.vcheck !== undefined) {
        d.checks[Number(target.dataset.vcheck)][target.dataset.vfield] = target.value;
      }
      if (target?.dataset?.vevidence) {
        d.evidence[target.dataset.vevidence] = target.value;
      }
    };
    panel.addEventListener("input", saveInput);
    panel.addEventListener("change", saveInput);
    panel.addEventListener("change", (e) => {
      if (e.target?.id !== "verification-paragraph") return;
      paragraph = e.target.value;
      formKey = "";
      render();
    });
    panel.addEventListener("click", handleClick);
  }

  function ensurePanelDom() {
    let panel = getElement("verification-panel");
    if (panel) {
      bindPanelEvents(panel);
      return panel;
    }
    const anchor = getElement("text-model-drawer") || getElement("text-action-result") || getElement("text-materials");
    if (!anchor || typeof anchor.after !== "function") return null;

    panel = el(doc, "section", "text-material");
    panel.id = "verification-panel";
    const details = el(doc, "details", "continuity-tools");
    details.id = "verification-details";
    details.append(
      el(doc, "summary", "", "逐段勘校（六事核对、审校参考与词证登记）"),
      el(doc, "p", "hint", "本地记录词义依据与核对情况，不会发送给模型或代为定稿。若在「资料」页改过临时关联，点「重新读取」即可同步称谓提示。"),
    );

    const toolbar = el(doc, "div", "verification-toolbar");
    const lblPara = el(doc, "label", "", "勘校段落");
    const selPara = el(doc, "select");
    selPara.id = "verification-paragraph";
    selPara.dataset.view = "";
    lblPara.append(selPara);
    const btnReload = el(doc, "button", "small", "重新读取（含关联）");
    btnReload.type = "button";
    btnReload.id = "verification-reload";
    toolbar.append(lblPara, btnReload);

    const summary = el(doc, "p", "hint");
    summary.id = "verification-summary";
    const message = el(doc, "div");
    message.id = "verification-message";
    message.setAttribute("role", "status");
    const facts = el(doc, "div");
    facts.id = "verification-facts";
    const form = el(doc, "div");
    form.id = "verification-form";

    details.append(toolbar, summary, message, facts, form);
    panel.append(details);
    anchor.after(panel);
    bindPanelEvents(panel);
    return panel;
  }

  function render() {
    const panel = ensurePanelDom();
    if (!panel) return;
    const { project, block } = getScope();
    panel.hidden = !project || !block;

    const summaryEl = getElement("verification-summary");
    const messageEl = getElement("verification-message");
    const factsEl = getElement("verification-facts");
    const formEl = getElement("verification-form");
    const paraEl = getElement("verification-paragraph");

    if (scope !== currentScope() || !view) {
      if (summaryEl) summaryEl.textContent = "";
      if (factsEl) {
        factsEl.textContent = errorMsg || "正在读取复核材料…";
        delete factsEl.dataset.signature;
      }
      formEl?.replaceChildren();
      formKey = "";
      paraEl?.replaceChildren();
      return;
    }

    const sourceParas = view.source?.paragraphs || [];
    if (!sourceParas.some((p) => p.id === paragraph)) {
      paragraph = sourceParas[0]?.id || "";
    }
    if (paraEl) {
      paraEl.replaceChildren();
      for (const p of sourceParas) {
        const opt = doc.createElement("option");
        opt.value = p.id;
        opt.textContent = formatLocation(p.id);
        opt.selected = p.id === paragraph;
        paraEl.append(opt);
      }
      if (paragraph) paraEl.value = paragraph;
    }
    if (!paragraph) {
      if (summaryEl) summaryEl.textContent = "";
      return;
    }

    const d = getDraft();
    const stale = d.fingerprint !== view.fingerprint;
    const info = (view.paragraphs || []).find((p) => p.paragraph === paragraph);
    const statusObj = getBlockStatus();
    const finalized = statusObj?.state?.state === "finalized";

    if (summaryEl) {
      summaryEl.textContent = `最新复核：${info?.latest ? "#" + info.latest : "尚无"} · 待核对：${info?.unresolved ?? 6} 项 · 定稿：${finalized ? "有效" : "未定稿"}`;
    }
    if (messageEl) {
      messageEl.textContent = errorMsg || (
        stale
          ? "材料或复核记录已更新，暂不能保存。输入已保留：请先阅读最新内容，再点「已读最新材料，继续核对」。"
          : "当前填写保留在页面内，核对完请主动保存；整页刷新或关闭后未保存内容会清空。"
      );
    }

    const signature = JSON.stringify([scope, paragraph, view, statusObj]);
    if (factsEl && factsEl.dataset.signature !== signature) {
      renderVerificationFactsDom({
        doc,
        host: factsEl,
        view,
        paragraph,
        info,
        finalized,
        formatLocation,
      });
      factsEl.dataset.signature = signature;
    }

    const key = `${scope}/${paragraph}`;
    if (key !== formKey && formEl) {
      formKey = key;
      renderVerificationFormDom({ doc, host: formEl, draft: d });
    }
    panel.querySelectorAll("button").forEach((b) => {
      b.disabled = busy || (b.id === "verification-save" && stale);
    });
  }

  async function load() {
    const nextScope = currentScope();
    const ticket = ++requestSeq;
    if (scope !== nextScope) {
      scope = nextScope;
      view = null;
      onViewChange(null);
      paragraph = "";
      formKey = "";
    }
    errorMsg = "";
    render();
    const { project, block } = getScope();
    if (!project || !block) return;
    try {
      const assoc = readAssociations() || [];
      const suffix = assoc.length
        ? "/verification/associations?associations=" + encodeURIComponent(JSON.stringify(assoc))
        : "/verification";
      const loaded = await apiCall(baseUrl() + suffix);
      if (nextScope !== currentScope() || ticket !== requestSeq) return;
      view = loaded;
      onViewChange(loaded);
    } catch (e) {
      if (nextScope !== currentScope() || ticket !== requestSeq) return;
      errorMsg = e?.message || String(e);
    }
    render();
  }

  async function handleClick(e) {
    const action = e.target?.id;
    if (!["verification-save", "verification-refresh", "verification-reload", "evidence-save"].includes(action)) {
      return;
    }
    if (busy) return;
    if (action === "verification-reload") {
      await load();
      return;
    }
    if (isViewer()) return;
    const savedScope = currentScope();
    const d = getDraft();
    if (action === "verification-refresh") {
      if (view) d.fingerprint = view.fingerprint;
      render();
      return;
    }
    let body;
    let endpoint;
    try {
      if (action === "evidence-save") {
        body = { paragraph, ...d.evidence };
        endpoint = "/evidence";
      } else {
        body = buildVerificationRequestBody({
          paragraph,
          draft: d,
          viewFingerprint: view?.fingerprint,
        });
        endpoint = "/verification";
      }
      busy = true;
      errorMsg = "";
      render();
      await apiCall(baseUrl() + endpoint, body);
      if (savedScope !== currentScope()) return;
      toast(
        action === "evidence-save"
          ? "词证已保存；未自动纳入已批准上下文，也未发送给模型。"
          : "复核记录已保存；不会自动解决问题或御览定稿。",
      );
      await load();
    } catch (err) {
      if (savedScope === currentScope()) errorMsg = err?.message || String(err);
    } finally {
      busy = false;
      if (savedScope === currentScope()) render();
    }
  }

  return {
    load,
    render,
    getView: () => view,
  };
}

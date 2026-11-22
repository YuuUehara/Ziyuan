// Neo Functional Mode Panel ("新政枢机 · 垂拱听政台") for ZiYuan Web UI.
// Shared between `/assets/neo-panel.mjs` and `node --test`.

export const NEO_SUBMODES = Object.freeze([
  {
    id: "overview",
    label: "垂拱总览",
    subtitle: "御案急递 · 六部当值 · 度支脉案",
  },
  {
    id: "quick-edict",
    label: "军机速诏",
    subtitle: "六部锦囊 · 拟诏直发 · 玉玺授受",
  },
  {
    id: "translation-neo",
    label: "译林枢机",
    subtitle: "翰林书目 · 章卷牙牌 · 质检巡检",
  },
  {
    id: "ministry-theme",
    label: "雅集与朝纲",
    subtitle: "宋韵四色 · 乌丝栏格 · 制度改易",
  },
]);

export const CLASSICAL_THEMES = Object.freeze([
  {
    id: "tianqing",
    name: "汝窑天青",
    badge: "宋瓷雅韵",
    poem: "雨过天青云破处，这般颜色做将来",
    desc: "汝州青瓷粉青配澄心堂象牙白宣，微含竹青，朱砂钤印，久视不伤目。",
    swatches: ["#f6f3ea", "#fcfbf6", "#e1ebe8", "#1f2826", "#b4382a"],
  },
  {
    id: "mo",
    name: "墨分五色",
    badge: "善本水墨",
    poem: "生绢水墨焦浓重淡清，独留一印朱砂",
    desc: "仿宋刻善本与澄心生宣，通篇焦、浓、重、淡、清五阶水墨，唯批红与准奏钤朱。",
    swatches: ["#efebe0", "#f8f5ec", "#e4dfd1", "#1c1b18", "#8e3226"],
  },
  {
    id: "qinglv",
    name: "青绿山水",
    badge: "千里江山",
    poem: "黛蓝花青为殿脊，石绿赭石入画屏",
    desc: "取王希孟青绿设色之法，左厢深黛如碑帖，右卷素宣明净，石绿与赭石点染。",
    swatches: ["#f5f2e9", "#fbf9f3", "#1f2c45", "#1b2433", "#b0582a"],
  },
  {
    id: "xiangzhi",
    name: "宣和缃帙",
    badge: "秘阁牙签",
    poem: "秘阁牙签缃缥色，藕荷沉香伴书卷",
    desc: "取宋代秘阁藏书缃帙与藕荷香笺之色，熟宣暖白配紫檀浅褐，温煦静气，尤宜夜读译书。",
    swatches: ["#f4eee4", "#fcf9f2", "#e8dfd2", "#29201b", "#a83c32"],
  },
]);

export const NEO_EDICT_PRESETS = Object.freeze([
  {
    id: "works-audit",
    title: "工部 · 巡检代码隐患",
    badge: "只读巡检",
    department: "works",
    departmentLabel: "工部",
    tools: ["read_file", "search_code"],
    scope: "crates",
    text: "全面巡检指定模块内的错误处理、边界条件与潜在死锁或资源泄漏，列出文件与行号依据，不修改文件。",
    desc: "仅授读文件与搜代码之权，适合先行摸底代码现状。",
  },
  {
    id: "works-plan",
    title: "工部 · 拟定修缮条陈",
    badge: "条陈方案",
    department: "works",
    departmentLabel: "工部",
    tools: ["read_file", "search_code"],
    scope: "crates",
    text: "梳理当前模块结构并拟定修缮条陈：1. 拟改动或新增之文件路径；2. 分步改法与接口边界；3. 验功测试方案。待批复后再授写权。",
    desc: "先出只读条陈方案，回奏后可一键「依此回奏授写权」下发工部。",
  },
  {
    id: "works-repair",
    title: "工部 · 动手修缮并验功",
    badge: "受控写权",
    department: "works",
    departmentLabel: "工部",
    tools: ["read_file", "search_code", "write_file", "run_tests"],
    scope: "crates",
    text: "按既定边界实施代码修缮，补充相应单元测试，并在号舍内执行 run_tests 核验通过；改动暂存呈报，待御览准奏后落盘。",
    desc: "授改文件与跑测试之权；改动仅暂存事件簿，须亲自准奏方才落盘。",
  },
  {
    id: "justice-security",
    title: "刑部 · 勘验越权与注入",
    badge: "刑名风宪",
    department: "justice",
    departmentLabel: "刑部",
    tools: ["read_file", "search_code"],
    scope: "crates, SECURITY.md",
    text: "勘验鉴权令牌、观政只读边界、路径规范化及外部输入防注入防线，逐项列出风险点与核验凭证。",
    desc: "由刑部按风宪口径核查权限边界与安全防线。",
  },
  {
    id: "rites-chronicle",
    title: "礼部 · 纂修实录与文书",
    badge: "典章文书",
    department: "rites",
    departmentLabel: "礼部",
    tools: ["read_file", "search_code"],
    scope: "docs, README.md, CHANGELOG.md",
    text: "核对现行使用指南、架构文档与代码实现口径是否一致，指出遗漏或过时之处并拟定修订文稿。",
    desc: "由礼部按「结论、依据、未办之事」三段体例呈报典章核对结果。",
  },
  {
    id: "personnel-arch",
    title: "吏部 · 考校模块职掌",
    badge: "考功建制",
    department: "personnel",
    departmentLabel: "吏部",
    tools: ["read_file", "search_code"],
    scope: "crates, Cargo.toml",
    text: "梳理工作区内各 crate 的职责边界、公共接口与依赖层次，指出耦合过重之处并提出拆分或精简建议。",
    desc: "由吏部考校模块分职与依赖秩序。",
  },
]);

export function classifyTaskAttention(task, retreat = false) {
  if (!task || typeof task !== "object") {
    return {
      needsAttention: false,
      kind: "unknown",
      label: "未知",
      badgeClass: "pending",
      primaryAction: null,
    };
  }
  if (task.needs_approval) {
    return {
      needsAttention: true,
      kind: "approval",
      label: "待批红",
      badgeClass: "awaiting",
      primaryAction: "approve",
    };
  }
  if (
    task.status === "completed" &&
    task.changes &&
    task.changes.state === "proposed"
  ) {
    return {
      needsAttention: true,
      kind: "changes",
      label: "待准奏",
      badgeClass: "awaiting",
      primaryAction: "changes",
    };
  }
  if (task.status === "completed" && !task.review && !retreat) {
    return {
      needsAttention: true,
      kind: "review",
      label: "待评定",
      badgeClass: "completed",
      primaryAction: "review",
    };
  }
  if (task.status === "assigned") {
    return {
      needsAttention: false,
      kind: "assigned",
      label: "执行中",
      badgeClass: "assigned",
      primaryAction: "inspect",
    };
  }
  if (task.status === "pending") {
    return {
      needsAttention: false,
      kind: "pending",
      label: "待办",
      badgeClass: "pending",
      primaryAction: "inspect",
    };
  }
  if (task.review) {
    return {
      needsAttention: false,
      kind: task.review.verdict || "reviewed",
      label: task.review.label || "已评定",
      badgeClass: task.review.verdict || "completed",
      primaryAction: "inspect",
    };
  }
  const fallbackLabels = {
    completed: "已完成",
    failed: "失败",
    cancelled: "已撤销",
  };
  return {
    needsAttention: false,
    kind: task.status || "unknown",
    label: fallbackLabels[task.status] || String(task.status || "未知"),
    badgeClass: task.status || "pending",
    primaryAction: "inspect",
  };
}

export function summarizeCourtForNeo(state) {
  if (!state || typeof state !== "object") {
    return {
      ready: false,
      viewer: true,
      modeCode: "chancellor",
      modeLabel: "未载入",
      retreat: false,
      running: false,
      watching: false,
      taskTotal: 0,
      openTotal: 0,
      attentionCount: 0,
      approvalCount: 0,
      changesCount: 0,
      reviewCount: 0,
      runningCount: 0,
      pendingCount: 0,
      attentionTasks: [],
      activeTasks: [],
      recentTasks: [],
      usage: {
        input: 0,
        output: 0,
        total: 0,
        cacheRead: 0,
        cacheHitPercent: 0,
        cost: null,
      },
      treasury: {
        hasLimit: false,
        remaining: 0,
        limit: 0,
        usedPercent: 0,
        exhausted: false,
      },
      vacanciesCount: 0,
      vacancies: [],
      departments: [],
      appointedCount: 0,
      totalOfficials: 0,
    };
  }

  const retreat = Boolean(state.retreat);
  const rawTasks = Array.isArray(state.tasks) ? [...state.tasks].reverse() : [];
  let approvalCount = 0;
  let changesCount = 0;
  let reviewCount = 0;
  let runningCount = 0;
  let pendingCount = 0;

  const attentionTasks = [];
  const activeTasks = [];

  for (const t of rawTasks) {
    const info = classifyTaskAttention(t, retreat);
    const enriched = { ...t, _neo: info };
    if (info.needsAttention) {
      attentionTasks.push(enriched);
      if (info.kind === "approval") approvalCount += 1;
      else if (info.kind === "changes") changesCount += 1;
      else if (info.kind === "review") reviewCount += 1;
    } else if (t.status === "assigned") {
      runningCount += 1;
      activeTasks.push(enriched);
    } else if (t.status === "pending") {
      pendingCount += 1;
      activeTasks.push(enriched);
    }
  }

  const input = Number(state.usage?.input || 0);
  const output = Number(state.usage?.output || 0);
  const cacheRead = Number(state.usage?.cache_read || 0);
  const cacheHitPercent =
    cacheRead > 0 && input > 0
      ? Math.round((Math.min(cacheRead, input) / input) * 100)
      : 0;

  const hasTreasury = Boolean(
    state.treasury && Number(state.treasury.limit) > 0,
  );
  const treLimit = hasTreasury ? Number(state.treasury.limit) : 0;
  const treRem = hasTreasury ? Number(state.treasury.remaining) : 0;
  const treUsedPct =
    hasTreasury && treLimit > 0
      ? Math.min(
          100,
          Math.max(0, Math.round(((treLimit - treRem) / treLimit) * 100)),
        )
      : 0;

  const officials = Array.isArray(state.officials) ? state.officials : [];
  const appointedCount = officials.filter(
    (o) => o && o.office && !o.purged,
  ).length;
  const vacancies = Array.isArray(state.vacancies) ? state.vacancies : [];
  const departments = Array.isArray(state.departments) ? state.departments : [];

  return {
    ready: true,
    viewer: Boolean(state.viewer),
    modeCode: state.mode?.code || "chancellor",
    modeLabel: state.mode?.label || "—",
    modes: Array.isArray(state.modes) ? state.modes : [],
    retreat,
    running: Boolean(state.running),
    watching: Boolean(state.watching),
    taskTotal: Number(state.task_total ?? rawTasks.length),
    openTotal: Number(state.open_total ?? runningCount + pendingCount),
    attentionCount: attentionTasks.length,
    approvalCount,
    changesCount,
    reviewCount,
    runningCount,
    pendingCount,
    attentionTasks,
    activeTasks,
    recentTasks: rawTasks
      .slice(0, 8)
      .map((t) => ({ ...t, _neo: classifyTaskAttention(t, retreat) })),
    usage: {
      input,
      output,
      total: input + output,
      cacheRead,
      cacheHitPercent,
      cost: state.cost || null,
    },
    treasury: {
      hasLimit: hasTreasury,
      remaining: treRem,
      limit: treLimit,
      usedPercent: treUsedPct,
      exhausted: hasTreasury && treRem === 0,
    },
    vacanciesCount: vacancies.length,
    vacancies,
    departments,
    appointedCount,
    totalOfficials: officials.filter((o) => o && !o.purged).length,
  };
}

export function summarizeTranslationForNeo(
  projectsList = [],
  activeProjectDetail = null,
) {
  const projects = Array.isArray(projectsList) ? projectsList : [];
  const detail =
    activeProjectDetail && typeof activeProjectDetail === "object"
      ? activeProjectDetail
      : null;
  const projMeta = detail?.project || projects[0] || null;
  const outline = Array.isArray(detail?.outline) ? detail.outline : [];

  let totalBlocks = 0;
  let finalizedBlocks = 0;
  let approvedBlocks = 0;
  let draftBlocks = 0;
  let untranslatedBlocks = 0;

  const chapters = outline.map((ch, idx) => {
    const blockIds = Array.isArray(ch.blocks) ? ch.blocks : [];
    const blockStates = Array.isArray(ch.block_states) ? ch.block_states : [];
    const stateMap = new Map(blockStates.map((bs) => [bs.block, bs]));
    const blocks = blockIds.map((bid, bIdx) => {
      totalBlocks += 1;
      const stObj = stateMap.get(bid);
      const rawState = stObj?.state?.state || stObj?.state || "untranslated";
      let category = "untranslated";
      let label = "待初译";
      if (rawState === "finalized" || rawState === "human_finalized") {
        category = "finalized";
        label = "已定稿";
        finalizedBlocks += 1;
      } else if (
        rawState === "approved" ||
        rawState === "human_approved" ||
        rawState === "translated"
      ) {
        category = "approved";
        label = "已准奏";
        approvedBlocks += 1;
      } else if (
        rawState === "awaiting_approval" ||
        rawState === "running" ||
        rawState === "human_draft" ||
        rawState === "human_submitted" ||
        rawState === "human_stale"
      ) {
        category = "draft";
        label = rawState === "human_stale" ? "待重修" : "修撰中";
        draftBlocks += 1;
      } else {
        untranslatedBlocks += 1;
      }
      return {
        id: bid,
        index: bIdx + 1,
        rawState: String(rawState),
        category,
        label,
      };
    });
    return {
      chapterId: ch.chapter,
      index: idx + 1,
      title: ch.title || `第 ${idx + 1} 章`,
      paragraphs: Number(ch.paragraphs || 0),
      blocks,
    };
  });

  const doneCount = finalizedBlocks + approvedBlocks;
  const completionPercent =
    totalBlocks > 0 ? Math.round((doneCount / totalBlocks) * 100) : 0;

  return {
    projectCount: projects.length,
    projects,
    activeId: projMeta?.id || null,
    activeTitle: projMeta?.title || (projects.length ? projects[0].title : null),
    sourceLanguage: projMeta?.source_language || null,
    chapterCount: chapters.length || Number(projMeta?.chapters || 0),
    totalBlocks,
    finalizedBlocks,
    approvedBlocks,
    draftBlocks,
    untranslatedBlocks,
    completionPercent,
    chapters,
  };
}

function el(doc, tag, className = "", text = null) {
  const node = doc.createElement(tag);
  if (className) node.className = className;
  if (text !== null && text !== undefined) node.textContent = String(text);
  return node;
}

const fmtNum = (n) => Number(n || 0).toLocaleString("zh-CN");

export function renderNeoPanel(container, context = {}, callbacks = {}) {
  const doc = container?.ownerDocument || globalThis.document;
  if (!container || !doc) return;
  while (container.firstChild) container.removeChild(container.firstChild);

  const submode = NEO_SUBMODES.some((m) => m.id === context.submode)
    ? context.submode
    : "overview";
  const currentTheme = context.theme || "tianqing";
  const wusilan = context.wusilan !== false;
  const compact = Boolean(context.compact);
  const court = summarizeCourtForNeo(context.state);
  const trans = summarizeTranslationForNeo(
    context.textProjects,
    context.textProject,
  );
  const selectedPresetId = context.selectedPresetId || NEO_EDICT_PRESETS[0].id;
  const draftState = context.edictDraft || {};

  // 1. Top Neo Scroll Header ("新政枢机 · 垂拱听政台")
  const banner = el(doc, "section", "neo-banner card");
  const bannerTop = el(doc, "div", "neo-banner-top");

  const titleBox = el(doc, "div", "neo-title-box");
  const sealStamp = el(doc, "span", "neo-seal-stamp", "枢机");
  sealStamp.setAttribute("aria-hidden", "true");
  const titleTextWrap = el(doc, "div", "neo-title-text");
  const h2 = el(doc, "h2", "neo-heading", "新政枢机 · 垂拱听政台");
  const sub = el(
    doc,
    "p",
    "neo-subtitle",
    "一屏统揽御案急递、军机速诏、翰林译林与宋韵四色画境；审阅与准奏大权仍归御前。",
  );
  titleTextWrap.append(h2, sub);
  titleBox.append(sealStamp, titleTextWrap);

  const bannerActions = el(doc, "div", "neo-banner-actions");
  if (!court.viewer) {
    const runBtn = el(
      doc,
      "button",
      "small gold",
      court.running ? "朝廷执行中…" : `执行待办（${court.openTotal}）`,
    );
    runBtn.type = "button";
    runBtn.disabled = court.running || court.openTotal === 0;
    runBtn.addEventListener("click", () => callbacks.onRunPending?.());
    bannerActions.appendChild(runBtn);
  }
  const doctorBtn = el(doc, "button", "small", "太医院脉案");
  doctorBtn.type = "button";
  doctorBtn.dataset.view = "";
  doctorBtn.addEventListener("click", () => callbacks.onOpenDoctor?.());
  bannerActions.appendChild(doctorBtn);

  const classicBtn = el(doc, "button", "small", "返归传统六殿");
  classicBtn.type = "button";
  classicBtn.dataset.view = "";
  classicBtn.id = "neo-return-classic";
  classicBtn.addEventListener("click", () =>
    callbacks.onReturnClassic?.(context.previousHall || "fengtian"),
  );
  bannerActions.appendChild(classicBtn);

  bannerTop.append(titleBox, bannerActions);
  banner.appendChild(bannerTop);

  // 2. Four Telemetry Water-Slips ("四柱水牌")
  const pillars = el(doc, "div", "neo-pillars");

  // Pillar 1: 御案待理会
  const p1 = el(
    doc,
    "div",
    `neo-pillar ${court.attentionCount > 0 ? "is-alert" : ""}`,
  );
  p1.append(
    el(doc, "span", "neo-pillar-label", "御案待理会"),
    el(doc, "strong", "neo-pillar-value", `${court.attentionCount} 件`),
    el(
      doc,
      "span",
      "neo-pillar-meta",
      `待批红 ${court.approvalCount} · 待准奏 ${court.changesCount} · 待评定 ${court.reviewCount}`,
    ),
  );

  // Pillar 2: 朝堂规制与百官
  const p2 = el(doc, "div", "neo-pillar");
  p2.append(
    el(doc, "span", "neo-pillar-label", "朝堂规制 · 百官"),
    el(
      doc,
      "strong",
      "neo-pillar-value",
      `${court.modeLabel}${court.retreat ? "（修道）" : ""}`,
    ),
    el(
      doc,
      "span",
      "neo-pillar-meta",
      `在任 ${court.appointedCount}/${court.totalOfficials} 员 · 缺位 ${court.vacanciesCount} 处`,
    ),
  );

  // Pillar 3: 户部度支
  const p3 = el(
    doc,
    "div",
    `neo-pillar ${court.treasury.exhausted ? "is-alert" : ""}`,
  );
  p3.append(
    el(doc, "span", "neo-pillar-label", "户部度支 · 算筹"),
    el(
      doc,
      "strong",
      "neo-pillar-value",
      `${fmtNum(court.usage.total)} tokens`,
    ),
    el(
      doc,
      "span",
      "neo-pillar-meta",
      court.treasury.hasLimit
        ? `度支余 ${fmtNum(court.treasury.remaining)} / ${fmtNum(court.treasury.limit)}`
        : `入 ${fmtNum(court.usage.input)} · 出 ${fmtNum(court.usage.output)}${court.usage.cacheHitPercent ? ` · 缓存 ${court.usage.cacheHitPercent}%` : ""}`,
    ),
  );

  // Pillar 4: 翰林院书目
  const p4 = el(doc, "div", "neo-pillar");
  p4.append(
    el(doc, "span", "neo-pillar-label", "翰林院 · 译林"),
    el(
      doc,
      "strong",
      "neo-pillar-value",
      trans.activeTitle ? trans.activeTitle : `书目 ${trans.projectCount} 部`,
    ),
    el(
      doc,
      "span",
      "neo-pillar-meta",
      trans.totalBlocks > 0
        ? `已成 ${trans.finalizedBlocks + trans.approvedBlocks}/${trans.totalBlocks} 组（${trans.completionPercent}%）`
        : `共 ${trans.projectCount} 部本地译书项目`,
    ),
  );

  pillars.append(p1, p2, p3, p4);
  banner.appendChild(pillars);

  // 3. Sub-mode Tabs ("四司枢机页签")
  const tabBar = el(doc, "div", "neo-tabs");
  tabBar.setAttribute("role", "tablist");
  tabBar.setAttribute("aria-label", "新政枢机模式分区");
  for (const tab of NEO_SUBMODES) {
    const btn = el(doc, "button", `neo-tab ${tab.id === submode ? "current" : ""}`);
    btn.type = "button";
    btn.dataset.view = "";
    btn.dataset.neoTab = tab.id;
    btn.setAttribute("role", "tab");
    btn.setAttribute("aria-selected", tab.id === submode ? "true" : "false");
    const b = el(doc, "b", "", tab.label);
    const s = el(doc, "span", "", tab.subtitle);
    btn.append(b, s);
    btn.addEventListener("click", () => callbacks.onSelectSubmode?.(tab.id));
    tabBar.appendChild(btn);
  }
  banner.appendChild(tabBar);
  container.appendChild(banner);

  // 4. Active Sub-mode Workspace Body
  if (submode === "overview") {
    renderOverviewSubmode(doc, container, court, trans, callbacks);
  } else if (submode === "quick-edict") {
    renderQuickEdictSubmode(
      doc,
      container,
      court,
      selectedPresetId,
      draftState,
      callbacks,
    );
  } else if (submode === "translation-neo") {
    renderTranslationNeoSubmode(doc, container, court, trans, callbacks);
  } else if (submode === "ministry-theme") {
    renderMinistryThemeSubmode(
      doc,
      container,
      court,
      currentTheme,
      wusilan,
      compact,
      callbacks,
    );
  }
}

function renderOverviewSubmode(doc, container, court, trans, callbacks) {
  const grid = el(doc, "div", "neo-grid-two");

  // Left Column: 御案急递与待办诏令
  const leftCard = el(doc, "section", "card neo-section");
  const leftHead = el(doc, "h2", "", "御案急递 · 诏令枢机");
  const leftSub = el(
    doc,
    "small",
    "",
    court.attentionCount
      ? `${court.attentionCount} 件待御前亲裁`
      : "当前御案清净，无待批奏折",
  );
  leftHead.appendChild(leftSub);
  const newEdictBtn = el(doc, "button", "small spacer", "军机速诏");
  newEdictBtn.type = "button";
  newEdictBtn.dataset.view = "";
  newEdictBtn.addEventListener("click", () =>
    callbacks.onSelectSubmode?.("quick-edict"),
  );
  leftHead.appendChild(newEdictBtn);
  leftCard.appendChild(leftHead);

  const leftBody = el(doc, "div", "body");
  const queueList =
    court.attentionTasks.length > 0
      ? court.attentionTasks
      : court.activeTasks.length > 0
        ? court.activeTasks
        : court.recentTasks;

  if (queueList.length === 0) {
    const empty = el(
      doc,
      "div",
      "neo-empty",
      "尚无诏令入册。可入「军机速诏」选用六部锦囊下诏，或前往「翰林院」开启译书。",
    );
    leftBody.appendChild(empty);
  } else {
    const listWrap = el(doc, "div", "neo-task-cards");
    for (const t of queueList.slice(0, 6)) {
      const card = el(
        doc,
        "article",
        `neo-task-card ${t._neo.needsAttention ? "needs-attention" : ""}`,
      );
      const top = el(doc, "div", "neo-task-top");
      const idStamp = el(doc, "span", "task-id", `#${t.id}`);
      const dept = el(doc, "span", "neo-task-dept", t.department || "六部");
      const badge = el(
        doc,
        "span",
        `badge ${t._neo.badgeClass}`,
        t._neo.label,
      );
      top.append(idStamp, dept, badge);
      if (t.urgent) {
        top.appendChild(el(doc, "span", "badge urgent", "八百里加急"));
      }
      if (t.worker) {
        top.appendChild(el(doc, "span", "hint", `承办：${t.worker}`));
      }
      card.appendChild(top);

      const firstLine =
        String(t.text || "")
          .split("\n")[0]
          .slice(0, 120) || "（无文）";
      card.appendChild(el(doc, "p", "neo-task-excerpt", firstLine));

      const actRow = el(doc, "div", "neo-task-actions");
      if (!court.viewer) {
        if (t._neo.kind === "approval") {
          const draftBtn = el(
            doc,
            "button",
            "small",
            t.draft ? "重新票拟" : "内阁票拟",
          );
          draftBtn.type = "button";
          draftBtn.addEventListener("click", () => callbacks.onDraftTask?.(t.id));
          const appBtn = el(doc, "button", "small gold", "朱笔批红");
          appBtn.type = "button";
          appBtn.addEventListener("click", () => callbacks.onApproveTask?.(t.id));
          actRow.append(draftBtn, appBtn);
        } else if (t._neo.kind === "review") {
          const accBtn = el(doc, "button", "small accept", "采纳回奏");
          accBtn.type = "button";
          accBtn.addEventListener("click", () =>
            callbacks.onReviewTask?.(t.id, "accept"),
          );
          const rejBtn = el(doc, "button", "small reject", "驳回");
          rejBtn.type = "button";
          rejBtn.addEventListener("click", () =>
            callbacks.onReviewTask?.(t.id, "reject"),
          );
          actRow.append(accBtn, rejBtn);
        }
        if (t.status === "completed" && !t.censure) {
          const cenBtn = el(doc, "button", "small", "都察院复核");
          cenBtn.type = "button";
          cenBtn.addEventListener("click", () => callbacks.onCensorTask?.(t.id));
          actRow.appendChild(cenBtn);
        }
      }
      const inspectBtn = el(
        doc,
        "button",
        "small",
        t._neo.kind === "changes" ? "审阅工部奏本" : "披览全文",
      );
      inspectBtn.type = "button";
      inspectBtn.dataset.view = "";
      inspectBtn.addEventListener("click", () => callbacks.onInspectTask?.(t.id));
      actRow.appendChild(inspectBtn);

      card.appendChild(actRow);
      listWrap.appendChild(card);
    }
    leftBody.appendChild(listWrap);
  }
  leftCard.appendChild(leftBody);

  // Right Column: 六部当值派工舆图 + 传统六殿捷径
  const rightCol = el(doc, "div", "neo-stack");

  const minCard = el(doc, "section", "card neo-section");
  const minHead = el(doc, "h2", "", "六部当值与派工路线");
  minHead.appendChild(el(doc, "small", "", `现行：${court.modeLabel}`));
  minCard.appendChild(minHead);
  const minBody = el(doc, "div", "body");
  if (court.departments.length === 0) {
    minBody.appendChild(el(doc, "p", "hint", "正在载入六部路线…"));
  } else {
    const minList = el(doc, "div", "neo-ministry-list");
    for (const d of court.departments) {
      const row = el(doc, "div", "neo-ministry-row");
      const badge = el(doc, "span", "neo-ministry-name", d.label);
      const routeWrap = el(doc, "div", "neo-ministry-route");
      if (Array.isArray(d.route) && d.route.length) {
        d.route.forEach((step, idx) => {
          if (idx > 0) routeWrap.appendChild(el(doc, "span", "arrow", " ▸ "));
          routeWrap.appendChild(
            el(doc, "span", `chip ${idx === 0 ? "emperor" : ""}`, step),
          );
        });
      } else {
        routeWrap.appendChild(
          el(doc, "span", "vacancy", d.vacancy || "正副俱缺"),
        );
      }
      const quickBtn = el(doc, "button", "small", "拟诏");
      quickBtn.type = "button";
      quickBtn.dataset.view = "";
      quickBtn.addEventListener("click", () =>
        callbacks.onQuickDepartmentEdict?.(d.code),
      );
      row.append(badge, routeWrap, quickBtn);
      minList.appendChild(row);
    }
    minBody.appendChild(minList);
  }
  minCard.appendChild(minBody);

  // Hall Quick Matrix ("六殿巡幸罗盘")
  const hallCard = el(doc, "section", "card neo-section");
  const hallHead = el(doc, "h2", "", "紫垣六殿 · 巡幸直达");
  hallHead.appendChild(el(doc, "small", "", "保留传统六殿全部礼制与深层功能"));
  hallCard.appendChild(hallHead);
  const hallBody = el(doc, "div", "body neo-hall-matrix");
  const hallSpecs = [
    {
      id: "translation",
      name: "翰林院",
      desc: "译稿 · 资料 · 质检 · 导出",
      stat: trans.activeTitle || `${trans.projectCount} 部书目`,
    },
    {
      id: "fengtian",
      name: "奉天殿",
      desc: "诏令队列 · 奏本审阅 · 排程",
      stat: `${court.taskTotal} 件诏令`,
    },
    {
      id: "huagai",
      name: "华盖殿",
      desc: "三制改易 · 西苑修道 · 补缺",
      stat: court.modeLabel,
    },
    {
      id: "wenhua",
      name: "文华殿",
      desc: "百官任免 · 考成簿 · 度支奏销",
      stat: `${court.appointedCount} 员在任`,
    },
    {
      id: "wuying",
      name: "武英殿",
      desc: "实录检索 · 兵部驿站 · 作品关注",
      stat: "旧案与驿传",
    },
    {
      id: "qiju",
      name: "起居注",
      desc: "事件簿全览 · 按官按时稽考",
      stat: "铁证留痕",
    },
  ];
  for (const h of hallSpecs) {
    const tile = el(doc, "button", "neo-hall-tile");
    tile.type = "button";
    tile.dataset.view = "";
    tile.dataset.gotoHall = h.id;
    tile.append(
      el(doc, "b", "neo-tile-title", h.name),
      el(doc, "span", "neo-tile-desc", h.desc),
      el(doc, "span", "neo-tile-stat", h.stat),
    );
    tile.addEventListener("click", () => callbacks.onSwitchHall?.(h.id));
    hallBody.appendChild(tile);
  }
  hallCard.appendChild(hallBody);

  rightCol.append(minCard, hallCard);
  grid.append(leftCard, rightCol);
  container.appendChild(grid);
}

function renderQuickEdictSubmode(
  doc,
  container,
  court,
  selectedPresetId,
  draftState,
  callbacks,
) {
  const grid = el(doc, "div", "neo-grid-two");

  // Left: 六部诏令锦囊
  const presetCard = el(doc, "section", "card neo-section");
  const presetHead = el(doc, "h2", "", "六部诏令锦囊");
  presetHead.appendChild(
    el(doc, "small", "", "点选锦囊即填入军机拟诏台，可任意增删改定"),
  );
  presetCard.appendChild(presetHead);
  const presetBody = el(doc, "div", "body neo-preset-grid");

  const activePreset =
    NEO_EDICT_PRESETS.find((p) => p.id === selectedPresetId) ||
    NEO_EDICT_PRESETS[0];

  for (const preset of NEO_EDICT_PRESETS) {
    const item = el(
      doc,
      "button",
      `neo-preset-card ${preset.id === activePreset.id ? "current" : ""}`,
    );
    item.type = "button";
    item.dataset.view = "";
    item.dataset.presetId = preset.id;
    const head = el(doc, "div", "neo-preset-head");
    head.append(
      el(doc, "b", "", preset.title),
      el(doc, "span", "badge seal", preset.badge),
    );
    const desc = el(doc, "p", "neo-preset-desc", preset.desc);
    const meta = el(
      doc,
      "span",
      "neo-preset-meta",
      `玉玺：${preset.tools.join("、")} · 范围：${preset.scope}`,
    );
    item.append(head, desc, meta);
    item.addEventListener("click", () => callbacks.onSelectPreset?.(preset));
    presetBody.appendChild(item);
  }
  presetCard.appendChild(presetBody);

  // Right: 军机拟诏台
  const formCard = el(doc, "section", "card neo-section");
  const formHead = el(doc, "h2", "", "军机拟诏台");
  formHead.appendChild(
    el(
      doc,
      "small",
      "",
      court.viewer ? "观政只读：可预览锦囊，不可下诏" : "玉玺所授范围外一概封禁",
    ),
  );
  formCard.appendChild(formHead);

  const formBody = el(doc, "div", "body");
  const curDept = draftState.department ?? activePreset.department;
  const curScope = draftState.scope ?? activePreset.scope;
  const curText = draftState.text ?? activePreset.text;
  const curTools = Array.isArray(draftState.tools)
    ? draftState.tools
    : activePreset.tools;

  const deptLabel = el(doc, "label", "", "承办六部");
  const deptSelect = el(doc, "select");
  deptSelect.id = "neo-edict-dept";
  const depts =
    court.departments.length > 0
      ? court.departments
      : [
          { code: "works", label: "工部" },
          { code: "personnel", label: "吏部" },
          { code: "revenue", label: "户部" },
          { code: "rites", label: "礼部" },
          { code: "war", label: "兵部" },
          { code: "justice", label: "刑部" },
        ];
  for (const d of depts) {
    const opt = el(doc, "option", "", d.label);
    opt.value = d.code;
    if (d.code === curDept) opt.selected = true;
    deptSelect.appendChild(opt);
  }
  deptSelect.value = curDept;
  deptLabel.appendChild(deptSelect);

  const scopeLabel = el(
    doc,
    "label",
    "",
    "玉玺 · 查阅范围（逗号分隔，空为整个项目）",
  );
  const scopeInput = el(doc, "input");
  scopeInput.type = "text";
  scopeInput.id = "neo-edict-scope";
  scopeInput.value = curScope;
  scopeInput.placeholder = "如 crates/ziyuan-web, docs";
  scopeLabel.appendChild(scopeInput);

  const textLabel = el(doc, "label", "", "诏令正文");
  const textArea = el(doc, "textarea");
  textArea.id = "neo-edict-text";
  textArea.value = curText;
  textArea.placeholder = "写明查办事项、边界约束与期望回奏体例…";
  textLabel.appendChild(textArea);

  const toolsTitle = el(doc, "label", "", "玉玺 · 授权工具");
  const checksWrap = el(doc, "div", "checks");
  const toolSpecs = [
    ["read_file", "读文件 read_file"],
    ["search_code", "搜代码 search_code"],
    ["write_file", "改文件 write_file（仅工部暂存）"],
    ["run_tests", "跑测试 run_tests（仅工部号舍）"],
  ];
  const toolInputs = {};
  for (const [code, labelText] of toolSpecs) {
    const lbl = el(doc, "label");
    const box = el(doc, "input");
    box.type = "checkbox";
    box.id = `neo-tool-${code}`;
    box.checked = curTools.includes(code);
    toolInputs[code] = box;
    lbl.append(box, doc.createTextNode(` ${labelText}`));
    checksWrap.appendChild(lbl);
  }

  const syncDraft = () => {
    const chosenTools = Object.entries(toolInputs)
      .filter(([, b]) => b.checked)
      .map(([k]) => k);
    callbacks.onUpdateDraft?.({
      department: deptSelect.value,
      scope: scopeInput.value,
      text: textArea.value,
      tools: chosenTools,
    });
  };
  deptSelect.addEventListener("change", syncDraft);
  scopeInput.addEventListener("input", syncDraft);
  textArea.addEventListener("input", syncDraft);
  Object.values(toolInputs).forEach((b) =>
    b.addEventListener("change", syncDraft),
  );

  const actions = el(doc, "div", "actions");
  const collectPayload = (runNow) => ({
    department: deptSelect.value,
    text: textArea.value.trim(),
    scope: scopeInput.value
      .split(/[,，]/)
      .map((s) => s.trim())
      .filter(Boolean),
    tools: Object.entries(toolInputs)
      .filter(([, b]) => b.checked)
      .map(([k]) => k),
    mcp: [],
    template: "",
    run: runNow,
  });

  if (!court.viewer) {
    const runBtn = el(doc, "button", "primary", "即刻下诏并执行");
    runBtn.type = "button";
    runBtn.id = "neo-issue-run";
    runBtn.addEventListener("click", () =>
      callbacks.onIssueEdict?.(collectPayload(true)),
    );
    const queueBtn = el(doc, "button", "", "仅颁诏入队");
    queueBtn.type = "button";
    queueBtn.id = "neo-issue-queue";
    queueBtn.addEventListener("click", () =>
      callbacks.onIssueEdict?.(collectPayload(false)),
    );
    actions.append(runBtn, queueBtn);
  } else {
    actions.appendChild(
      el(doc, "p", "hint", "当前为观政只读令牌，不能颁布新诏令。"),
    );
  }

  const fullSheetBtn = el(doc, "button", "small", "打开奉天殿完整诏书台");
  fullSheetBtn.type = "button";
  fullSheetBtn.dataset.view = "";
  fullSheetBtn.addEventListener("click", () => callbacks.onOpenFullEdictSheet?.());
  actions.appendChild(fullSheetBtn);

  formBody.append(
    deptLabel,
    scopeLabel,
    textLabel,
    toolsTitle,
    checksWrap,
    el(
      doc,
      "p",
      "hint",
      "提示：.ziyuan、.git、.jj 与 .env* 恒受封禁；工部改动只入暂存奏本，经御览逐段准奏后方才落盘。",
    ),
    actions,
  );
  formCard.appendChild(formBody);

  grid.append(presetCard, formCard);
  container.appendChild(grid);
}

function renderTranslationNeoSubmode(doc, container, court, trans, callbacks) {
  const card = el(doc, "section", "card neo-section");
  const head = el(doc, "h2", "", "翰林译林枢机 · 书目与章卷牙牌");
  head.appendChild(
    el(
      doc,
      "small",
      "",
      trans.activeTitle
        ? `当前书目：${trans.activeTitle}（完成度 ${trans.completionPercent}%）`
        : "本地小说与长文翻译审校中枢",
    ),
  );
  const openHallBtn = el(doc, "button", "small spacer", "进入翰林院全屏工作台");
  openHallBtn.type = "button";
  openHallBtn.dataset.view = "";
  openHallBtn.addEventListener("click", () =>
    callbacks.onSwitchHall?.("translation"),
  );
  head.appendChild(openHallBtn);
  card.appendChild(head);

  const body = el(doc, "div", "body");

  // Top toolbar: project selector + quick actions
  const bar = el(doc, "div", "neo-trans-bar");
  const projSelect = el(doc, "select");
  projSelect.id = "neo-trans-project";
  if (trans.projects.length === 0) {
    const opt = el(doc, "option", "", "（尚无译书项目）");
    opt.value = "";
    projSelect.appendChild(opt);
  } else {
    for (const p of trans.projects) {
      const opt = el(
        doc,
        "option",
        "",
        `${p.title}（${p.source_language || "原文"} · ${p.chapters || 0} 章）`,
      );
      opt.value = p.id;
      if (p.id === trans.activeId) opt.selected = true;
      projSelect.appendChild(opt);
    }
  }
  projSelect.addEventListener("change", () => {
    if (projSelect.value) callbacks.onSelectProject?.(projSelect.value);
  });
  bar.appendChild(projSelect);

  if (!court.viewer) {
    const newProjBtn = el(doc, "button", "small", "新建译书项目");
    newProjBtn.type = "button";
    newProjBtn.addEventListener("click", () => callbacks.onOpenNewProject?.());
    bar.appendChild(newProjBtn);

    const quickTermBtn = el(doc, "button", "small", "快捷收术语");
    quickTermBtn.type = "button";
    quickTermBtn.disabled = !trans.activeId;
    quickTermBtn.addEventListener("click", () =>
      callbacks.onQuickGlossary?.("term"),
    );
    const quickPersonBtn = el(doc, "button", "small", "快捷收人物");
    quickPersonBtn.type = "button";
    quickPersonBtn.disabled = !trans.activeId;
    quickPersonBtn.addEventListener("click", () =>
      callbacks.onQuickGlossary?.("person"),
    );
    bar.append(quickTermBtn, quickPersonBtn);
  }

  const qaBtn = el(doc, "button", "small primary", "整书质检总览");
  qaBtn.type = "button";
  qaBtn.dataset.view = "";
  qaBtn.disabled = !trans.activeId;
  qaBtn.addEventListener("click", () => callbacks.onRunProjectQa?.());
  bar.appendChild(qaBtn);
  body.appendChild(bar);

  // Progress bar & breakdown
  if (trans.totalBlocks > 0) {
    const progBox = el(doc, "div", "neo-progress-box");
    const progStats = el(
      doc,
      "div",
      "neo-progress-stats",
      `全书共 ${trans.chapterCount} 章 ${trans.totalBlocks} 组译稿 · 已定稿 ${trans.finalizedBlocks} 组 · 已准奏 ${trans.approvedBlocks} 组 · 修撰中 ${trans.draftBlocks} 组 · 待初译 ${trans.untranslatedBlocks} 组`,
    );
    const track = el(doc, "div", "neo-progress-track");
    const pct = (n) => `${Math.round((n / trans.totalBlocks) * 100)}%`;
    if (trans.finalizedBlocks > 0) {
      const seg = el(doc, "span", "seg finalized");
      seg.style.width = pct(trans.finalizedBlocks);
      seg.title = `已定稿 ${trans.finalizedBlocks} 组`;
      track.appendChild(seg);
    }
    if (trans.approvedBlocks > 0) {
      const seg = el(doc, "span", "seg approved");
      seg.style.width = pct(trans.approvedBlocks);
      seg.title = `已准奏 ${trans.approvedBlocks} 组`;
      track.appendChild(seg);
    }
    if (trans.draftBlocks > 0) {
      const seg = el(doc, "span", "seg draft");
      seg.style.width = pct(trans.draftBlocks);
      seg.title = `修撰中 ${trans.draftBlocks} 组`;
      track.appendChild(seg);
    }
    progBox.append(progStats, track);
    body.appendChild(progBox);

    // Chapter & Block Jade Slips ("章卷牙牌")
    const chGrid = el(doc, "div", "neo-chapter-list");
    for (const ch of trans.chapters) {
      const chBox = el(doc, "div", "neo-chapter-card");
      const chHead = el(doc, "div", "neo-chapter-head");
      chHead.append(
        el(doc, "b", "", ch.title),
        el(
          doc,
          "span",
          "hint",
          `${ch.paragraphs} 段 · ${ch.blocks.length} 组译稿`,
        ),
      );
      chBox.appendChild(chHead);

      const slips = el(doc, "div", "neo-jade-slips");
      for (const blk of ch.blocks) {
        const slip = el(
          doc,
          "button",
          `neo-jade-slip is-${blk.category}`,
        );
        slip.type = "button";
        slip.dataset.view = "";
        slip.dataset.blockId = blk.id;
        slip.append(
          el(doc, "span", "slip-num", `第 ${blk.index} 组`),
          el(doc, "span", "slip-state", blk.label),
        );
        slip.addEventListener("click", () =>
          callbacks.onOpenBlock?.(trans.activeId, blk.id),
        );
        slips.appendChild(slip);
      }
      chBox.appendChild(slips);
      chGrid.appendChild(chBox);
    }
    body.appendChild(chGrid);
  } else {
    body.appendChild(
      el(
        doc,
        "div",
        "neo-empty",
        trans.activeId
          ? "当前项目尚未导入原文。可点击「进入翰林院全屏工作台」在项目管理中预览并导入 TXT / Markdown / EPUB / DOCX / SRT 原文。"
          : "尚无本地译书项目。可点击「新建译书项目」创立书目，或进入翰林院查阅。",
      ),
    );
  }

  card.appendChild(body);
  container.appendChild(card);
}

function renderMinistryThemeSubmode(
  doc,
  container,
  court,
  currentTheme,
  wusilan,
  compact,
  callbacks,
) {
  const stack = el(doc, "div", "neo-stack");

  // 1. Classical Chinese Pale-Elegant Palette Gallery ("宋韵四色 · 古风淡雅画境")
  const themeCard = el(doc, "section", "card neo-section");
  const themeHead = el(doc, "h2", "", "宋韵四色 · 中国古代淡雅设色");
  themeHead.appendChild(
    el(doc, "small", "", "取法宋瓷、水墨、青绿与缃帙，分区只靠留白、细线与一枚朱印"),
  );
  themeCard.appendChild(themeHead);

  const themeBody = el(doc, "div", "body");
  const gallery = el(doc, "div", "neo-theme-gallery");
  for (const t of CLASSICAL_THEMES) {
    const active = t.id === currentTheme;
    const item = el(
      doc,
      "button",
      `neo-theme-card ${active ? "current" : ""}`,
    );
    item.type = "button";
    item.dataset.view = "";
    item.dataset.neoTheme = t.id;

    const top = el(doc, "div", "neo-theme-top");
    top.append(
      el(doc, "b", "neo-theme-name", t.name),
      el(doc, "span", "badge seal", active ? "当前设色" : t.badge),
    );

    const poem = el(doc, "p", "neo-theme-poem", `「${t.poem}」`);
    const desc = el(doc, "p", "neo-theme-desc", t.desc);

    const swatches = el(doc, "div", "neo-swatches");
    const swatchLabels = ["纸底", "书页", "殿阁", "浓墨", "朱印"];
    t.swatches.forEach((hex, idx) => {
      const sw = el(doc, "span", "neo-swatch");
      sw.style.background = hex;
      sw.title = `${swatchLabels[idx] || "色卡"}：${hex}`;
      swatches.appendChild(sw);
    });

    item.append(top, poem, desc, swatches);
    item.addEventListener("click", () => callbacks.onSelectTheme?.(t.id));
    gallery.appendChild(item);
  }
  themeBody.appendChild(gallery);

  // Classical Book-Frame & Layout Controls ("版式与乌丝栏格调")
  const layoutRow = el(doc, "div", "neo-layout-controls");
  const wusilanBtn = el(
    doc,
    "button",
    `small ${wusilan ? "current" : ""}`,
    wusilan ? "宋刻乌丝栏：已开启" : "宋刻乌丝栏：已关闭",
  );
  wusilanBtn.type = "button";
  wusilanBtn.dataset.view = "";
  wusilanBtn.id = "neo-toggle-wusilan";
  wusilanBtn.addEventListener("click", () =>
    callbacks.onToggleWusilan?.(!wusilan),
  );

  const compactBtn = el(
    doc,
    "button",
    `small ${compact ? "current" : ""}`,
    compact ? "版式：紧凑奏折" : "版式：疏朗卷轴",
  );
  compactBtn.type = "button";
  compactBtn.dataset.view = "";
  compactBtn.id = "neo-toggle-compact";
  compactBtn.addEventListener("click", () =>
    callbacks.onToggleCompact?.(!compact),
  );

  layoutRow.append(
    el(doc, "span", "hint", "书卷装帧与行距："),
    wusilanBtn,
    compactBtn,
  );
  themeBody.appendChild(layoutRow);
  themeCard.appendChild(themeBody);

  // 2. Governance Mode & Treasury Quick Deck ("朝纲制度与户部奏销")
  const govCard = el(doc, "section", "card neo-section");
  const govHead = el(doc, "h2", "", "朝纲制度 · 考成与户部奏销");
  govHead.appendChild(
    el(doc, "small", "", "三代官制流转、考成法与户部度支账本"),
  );
  govCard.appendChild(govHead);

  const govBody = el(doc, "div", "body");
  if (court.modes && court.modes.length) {
    const modeBox = el(doc, "div", "modes");
    for (const m of court.modes) {
      const btn = el(
        doc,
        "button",
        m.code === court.modeCode ? "current" : "",
        m.label,
      );
      btn.type = "button";
      btn.disabled = court.viewer;
      btn.addEventListener("click", () => callbacks.onChangeGovernanceMode?.(m));
      modeBox.appendChild(btn);
    }
    govBody.appendChild(modeBox);
  }

  const actionBar = el(doc, "div", "actions");
  if (!court.viewer) {
    const retreatBtn = el(
      doc,
      "button",
      "small",
      court.retreat ? "回朝亲政" : "移居西苑修道（司礼监代批）",
    );
    retreatBtn.type = "button";
    retreatBtn.addEventListener("click", () => callbacks.onToggleRetreat?.());
    actionBar.appendChild(retreatBtn);
  }

  const kaoBtn = el(doc, "button", "small", "披览考成簿");
  kaoBtn.type = "button";
  kaoBtn.dataset.view = "";
  kaoBtn.addEventListener("click", () => callbacks.onOpenKaocheng?.());

  const duzhiBtn = el(doc, "button", "small", "披览度支奏销");
  duzhiBtn.type = "button";
  duzhiBtn.dataset.view = "";
  duzhiBtn.addEventListener("click", () => callbacks.onOpenDuzhi?.());

  const csvBtn = el(doc, "button", "small", "导出度支 CSV（Excel BOM）");
  csvBtn.type = "button";
  csvBtn.dataset.view = "";
  csvBtn.addEventListener("click", () => callbacks.onExportDuzhi?.("csv", true));

  const jsonBtn = el(doc, "button", "small", "导出度支 JSON");
  jsonBtn.type = "button";
  jsonBtn.dataset.view = "";
  jsonBtn.addEventListener("click", () =>
    callbacks.onExportDuzhi?.("json", false),
  );

  const docBtn = el(doc, "button", "small", "太医院环境自检");
  docBtn.type = "button";
  docBtn.dataset.view = "";
  docBtn.addEventListener("click", () => callbacks.onOpenDoctor?.());

  actionBar.append(kaoBtn, duzhiBtn, csvBtn, jsonBtn, docBtn);
  govBody.appendChild(actionBar);
  govCard.appendChild(govBody);

  stack.append(themeCard, govCard);
  container.appendChild(stack);
}

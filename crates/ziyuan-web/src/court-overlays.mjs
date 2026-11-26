// Court global overlays & Neo pivotal bridge ("奉天门 · 全局浮层与垂拱枢机桥接")
// Shared between `/assets/court-overlays.mjs` and `node --test`.

const THEME_ROTATION = Object.freeze(["tianqing", "mo", "qinglv", "xiangzhi", "ye"]);

export function mountCourtOverlays(ctx) {
  const {
    doc,
    win,
    storage,
    getById,
    api,
    act,
    toast,
    chronicle,
    preserve,
    render,
    showHall,
    setTheme,
    downloadDuzhiExport,
    selectWorkspaceTab,
    runProjectQaReport,
    runProjectUsageReport,
    selectTextBlock,
    loadText,
    loadTextProject,
    materialAdvice,
    statusLabel,
    getState,
    getHall,
    getPreviousClassicHall,
    setSelectedTask,
    getTextState,
    setTextProjectId,
    getQaNavigator,
    loadNeoModule,
    loadPaletteModule,
    loadDoctorModule,
    loadQuickGlossaryModule,
  } = ctx;

  const $ = (id) => (typeof getById === "function" ? getById(id) : doc?.getElementById?.(id));

  // ---- 快捷命令面板（Ctrl+K / Cmd+K） ----
  let cmdPaletteItems = [];
  let cmdPaletteIndex = 0;
  let cmdPaletteMod = null;
  let cmdPaletteModPromise = null;

  function setHash(nextHash) {
    if (win && win.location) win.location.hash = nextHash;
  }

  function runCommandPaletteAction(action) {
    if (!action) return;
    const state = getState?.();
    if (action.type === "hall") {
      setHash(action.hall);
      showHall?.();
    } else if (action.type === "neo") {
      setHash("#neo");
      showHall?.();
    } else if (action.type === "duzhi") {
      setHash("#wenhua");
      showHall?.();
      $("duzhi-btn")?.click?.();
    } else if (action.type === "export") {
      downloadDuzhiExport?.(action.format, action.bom);
    } else if (action.type === "kaocheng") {
      setHash("#wenhua");
      showHall?.();
      $("kaocheng-btn")?.click?.();
    } else if (action.type === "theme") {
      const cur = doc?.documentElement?.dataset?.theme || "tianqing";
      const next = THEME_ROTATION[(THEME_ROTATION.indexOf(cur) + 1) % THEME_ROTATION.length];
      setTheme?.(next);
      if (state) preserve?.(render);
    } else if (action.type === "new-edict") {
      setHash("#fengtian");
      showHall?.();
      $("edict-new")?.click?.();
    } else if (action.type === "doctor") {
      openDoctorModal();
    } else if (action.type === "run") {
      $("run-btn")?.click?.();
    } else if (action.type === "task") {
      setSelectedTask?.(action.taskId);
      setHash("#fengtian");
      showHall?.();
      preserve?.(render);
    }
  }

  async function ensureCommandPaletteModule() {
    if (cmdPaletteMod) return cmdPaletteMod;
    if (!cmdPaletteModPromise) {
      const loader = loadPaletteModule || (() => import("/assets/command-palette.mjs"));
      cmdPaletteModPromise = loader().then((m) => (cmdPaletteMod = m));
    }
    return cmdPaletteModPromise;
  }

  async function renderCommandPaletteList() {
    const input = $("cmd-palette-input");
    const list = $("cmd-palette-list");
    if (!input || !list) return;
    const mod = await ensureCommandPaletteModule();
    const state = getState?.();
    const all = mod.buildPaletteSpecs({
      viewer: !!(state && state.viewer),
      tasks: state && Array.isArray(state.tasks) ? state.tasks : [],
      statusLabel,
    });
    cmdPaletteItems = mod.filterPaletteItems(all, input.value).map((spec) => ({
      ...spec,
      run: () => runCommandPaletteAction(spec.action),
    }));
    if (cmdPaletteIndex >= cmdPaletteItems.length) cmdPaletteIndex = 0;
    list.textContent = "";
    if (Array.isArray(list.children)) list.children.length = 0;
    if (!cmdPaletteItems.length) {
      const empty = doc.createElement("li");
      empty.className = "cmd-palette-item";
      empty.textContent = "无匹配指令";
      list.appendChild(empty);
      return;
    }
    cmdPaletteItems.forEach((item, idx) => {
      const li = doc.createElement("li");
      li.className = "cmd-palette-item" + (idx === cmdPaletteIndex ? " active" : "");
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", idx === cmdPaletteIndex ? "true" : "false");
      const title = doc.createElement("span");
      title.textContent = item.label;
      const meta = doc.createElement("span");
      meta.className = "cmd-meta";
      meta.textContent = item.meta;
      li.appendChild(title);
      li.appendChild(meta);
      li.addEventListener("mouseenter", () => {
        cmdPaletteIndex = idx;
        list.querySelectorAll?.(".cmd-palette-item")?.forEach((n, i) => {
          n.classList.toggle("active", i === idx);
          n.setAttribute("aria-selected", i === idx ? "true" : "false");
        });
      });
      li.addEventListener("click", () => {
        closeCommandPalette();
        item.run();
      });
      list.appendChild(li);
    });
  }

  function openCommandPalette() {
    const modal = $("cmd-palette");
    const input = $("cmd-palette-input");
    if (!modal || !input) return;
    modal.hidden = false;
    input.value = "";
    cmdPaletteIndex = 0;
    renderCommandPaletteList();
    input.focus?.();
  }

  function closeCommandPalette() {
    const modal = $("cmd-palette");
    if (modal) modal.hidden = true;
  }

  // ---- 太医院脉案自检弹窗 ----
  let doctorMod = null;
  let doctorModPromise = null;

  async function loadDoctorReport() {
    const body = $("doctor-body");
    if (!body) return;
    body.textContent = "正在执行只读环境自检…";
    try {
      if (!doctorMod) {
        if (!doctorModPromise) {
          const loader = loadDoctorModule || (() => import("/assets/doctor-panel.mjs"));
          doctorModPromise = loader().then((m) => (doctorMod = m));
        }
        await doctorModPromise;
      }
      const report = await api("/doctor");
      doctorMod.renderDoctorReport(body, report);
    } catch (err) {
      body.textContent = `自检失败：${err.message || err}`;
    }
  }

  function openDoctorModal() {
    const modal = $("doctor-modal");
    if (!modal) return;
    modal.hidden = false;
    loadDoctorReport();
  }

  function closeDoctorModal() {
    const modal = $("doctor-modal");
    if (modal) modal.hidden = true;
  }

  // ---- 垂拱枢机 · 御前听政台桥接 ----
  let neoMod = null;
  let neoModPromise = null;
  let neoSubmode = "overview";
  let neoSelectedPresetId = "works-audit";
  let neoEdictDraft = {};
  let neoWusilan = (() => {
    try {
      return storage ? storage.getItem("ziyuan-wusilan") !== "0" : true;
    } catch {
      return true;
    }
  })();
  let neoCompact = (() => {
    try {
      return storage ? storage.getItem("ziyuan-compact") === "1" : false;
    } catch {
      return false;
    }
  })();

  function applyNeoLayoutClasses() {
    doc?.body?.classList?.toggle("neo-wusilan", neoWusilan);
    doc?.body?.classList?.toggle("neo-compact", neoCompact);
  }
  applyNeoLayoutClasses();

  function toggleNeoMode() {
    if (getHall?.() === "neo") {
      setHash("#" + (getPreviousClassicHall?.() || "fengtian"));
    } else {
      setHash("#neo");
    }
    showHall?.();
  }

  function ensureNeoPanel() {
    if (neoMod) {
      renderNeoView();
      return Promise.resolve(neoMod);
    }
    if (!neoModPromise) {
      const loader = loadNeoModule || (() => import("/assets/neo-panel.mjs"));
      neoModPromise = loader()
        .then((m) => {
          neoMod = m;
          renderNeoView();
          return m;
        })
        .catch((err) => {
          neoModPromise = null;
          const root = $("neo-panel-root");
          if (root) root.textContent = "垂拱枢机面板加载失败：" + (err.message || err);
        });
    }
    return neoModPromise;
  }

  function renderNeoView() {
    const root = $("neo-panel-root");
    if (!root || !neoMod) return;
    const hall = getHall?.();
    if (
      hall === "neo" &&
      neoSubmode === "quick-edict" &&
      root.contains?.(doc?.activeElement)
    ) {
      const tag = doc?.activeElement?.tagName;
      if (tag === "TEXTAREA" || tag === "INPUT" || tag === "SELECT") return;
    }
    const state = getState?.();
    const ts = getTextState?.() || {};
    const textProjects = Array.isArray(ts.textProjects)
      ? ts.textProjects
      : ts.textProjects?.projects || [];
    neoMod.renderNeoPanel(
      root,
      {
        submode: neoSubmode,
        theme: doc?.documentElement?.dataset?.theme || "tianqing",
        wusilan: neoWusilan,
        compact: neoCompact,
        state,
        textProjects,
        textProject: ts.textProject || null,
        textUsage: ts.textUsageSummaryCache || null,
        selectedPresetId: neoSelectedPresetId,
        edictDraft: neoEdictDraft,
        previousHall: getPreviousClassicHall?.() || "fengtian",
      },
      {
        onSelectSubmode: (id) => {
          neoSubmode = id;
          if (id === "translation-neo" && !ts.textLoaded) loadText?.();
          renderNeoView();
        },
        onRunPending: () => $("run-btn")?.click?.(),
        onOpenDoctor: () => openDoctorModal(),
        onReturnClassic: (target) => {
          setHash("#" + (target || getPreviousClassicHall?.() || "fengtian"));
          showHall?.();
        },
        onDraftTask: (id) => {
          act(api(`/tasks/${id}/draft`, {})).then(
            (r) => r && toast?.(`诏令 #${id} 票拟：拟派 ${r.name}`),
          );
        },
        onApproveTask: (id) => {
          act(api(`/tasks/${id}/approve`, {}), `诏令 #${id} 已批红`);
        },
        onReviewTask: (id, verdict) => {
          act(
            api(`/tasks/${id}/review`, { verdict, note: "" }),
            verdict === "accept" ? "已采纳" : "已驳回",
          );
        },
        onCensorTask: (id) => {
          act(api(`/tasks/${id}/censor`, {})).then(
            (r) => r && toast?.(`诏令 #${id} 都察院${r.label}`),
          );
        },
        onInspectTask: (id) => {
          setSelectedTask?.(id);
          setHash("#fengtian");
          showHall?.();
          preserve?.(render);
        },
        onQuickDepartmentEdict: (deptCode) => {
          const preset =
            neoMod.NEO_EDICT_PRESETS.find((p) => p.department === deptCode) ||
            neoMod.NEO_EDICT_PRESETS[0];
          neoSelectedPresetId = preset.id;
          neoEdictDraft = {
            department: deptCode,
            scope: preset.scope,
            text: preset.text,
            tools: preset.tools.slice(),
          };
          neoSubmode = "quick-edict";
          renderNeoView();
        },
        onSwitchHall: (targetHall) => {
          setHash("#" + targetHall);
          showHall?.();
        },
        onSelectPreset: (preset) => {
          neoSelectedPresetId = preset.id;
          neoEdictDraft = {
            department: preset.department,
            scope: preset.scope,
            text: preset.text,
            tools: preset.tools.slice(),
          };
          renderNeoView();
        },
        onUpdateDraft: (draft) => {
          neoEdictDraft = { ...neoEdictDraft, ...draft };
        },
        onIssueEdict: async (payload) => {
          if (!payload.text) {
            toast?.("请先填写诏令正文", true);
            return;
          }
          const curState = getState?.();
          const runNow = !!payload.run;
          const result = await act(
            api("/edicts", {
              department: payload.department,
              text: payload.text,
              scope: payload.scope,
              tools: payload.tools,
              mcp: payload.mcp || [],
              template: "",
            }),
          );
          if (!result) return;
          setSelectedTask?.(result.id);
          toast?.(
            `诏令 #${result.id} 已自军机处颁布${curState?.mode?.code === "cabinet" && !runNow ? "，待批红" : ""}`,
          );
          if (runNow && curState?.mode?.code === "cabinet") {
            await act(api(`/tasks/${result.id}/draft`, {}));
            await act(api(`/tasks/${result.id}/approve`, {}));
          }
          if (runNow && !curState?.running) await act(api("/run", {}));
          neoSubmode = "overview";
          renderNeoView();
        },
        onOpenFullEdictSheet: () => {
          setSelectedTask?.("new");
          setHash("#fengtian");
          showHall?.();
          preserve?.(render);
        },
        onSelectProject: (projId) => {
          setTextProjectId?.(projId);
          loadTextProject?.();
        },
        onOpenNewProject: () => {
          setHash("#translation");
          showHall?.();
          $("text-new-project")?.click?.();
        },
        onQuickGlossary: (kind) => {
          openQuickGlossaryDialog(kind);
        },
        onRunProjectQa: () => {
          setHash("#translation");
          showHall?.();
          selectWorkspaceTab?.("delivery");
          runProjectQaReport?.();
        },
        onOpenProjectUsage: () => {
          setHash("#translation");
          showHall?.();
          selectWorkspaceTab?.("delivery");
          $("text-project-usage")?.scrollIntoView?.({ block: "nearest" });
          runProjectUsageReport?.();
        },
        onOpenBlock: (projId, blockId) => {
          const curProj = getTextState?.()?.textProjectId;
          if (projId && projId !== curProj) setTextProjectId?.(projId);
          setHash(
            `#translation/text/${encodeURIComponent(projId)}/${encodeURIComponent(blockId)}`,
          );
          showHall?.();
          selectTextBlock?.(blockId);
        },
        onSelectTheme: (themeId) => {
          setTheme?.(themeId);
          if (getState?.()) preserve?.(render);
        },
        onToggleWusilan: (on) => {
          neoWusilan = Boolean(on);
          try {
            storage?.setItem("ziyuan-wusilan", neoWusilan ? "1" : "0");
          } catch {}
          applyNeoLayoutClasses();
          renderNeoView();
        },
        onToggleCompact: (on) => {
          neoCompact = Boolean(on);
          try {
            storage?.setItem("ziyuan-compact", neoCompact ? "1" : "0");
          } catch {}
          applyNeoLayoutClasses();
          renderNeoView();
        },
        onChangeGovernanceMode: (m) => {
          const curState = getState?.();
          if (!curState || m.code === curState.mode.code) return;
          if (m.code === "direct" && curState.mode.code === "chancellor") {
            setHash("#huagai");
            showHall?.();
            const abolish = $("abolish-choice");
            if (abolish) abolish.hidden = false;
            return;
          }
          const ok = win?.confirm
            ? win.confirm(`改制为「${m.label}」？旧制度的中书省/内阁官职将自动撤销。`)
            : true;
          if (ok) {
            act(api("/reform", { mode: m.code }), `已改制为${m.label}`).then(
              (r) => r && chronicle?.(r.note),
            );
          }
        },
        onToggleRetreat: () => {
          $("retreat-btn")?.click?.();
        },
        onOpenKaocheng: () => {
          setHash("#wenhua");
          showHall?.();
          $("kaocheng-btn")?.click?.();
        },
        onOpenDuzhi: () => {
          setHash("#wenhua");
          showHall?.();
          $("duzhi-btn")?.click?.();
        },
        onExportDuzhi: (fmt, bom) => {
          downloadDuzhiExport?.(fmt, bom);
        },
      },
    );
  }

  // ---- 快捷收录术语／人物弹窗 ----
  let quickGlossaryKind = "term";
  let quickGlossaryModPromise = null;
  let quickLastSelection = "";

  function loadQuickGlossaryMod() {
    if (!quickGlossaryModPromise) {
      const loader =
        loadQuickGlossaryModule || (() => import("/assets/quick-glossary.mjs"));
      quickGlossaryModPromise = loader();
    }
    return quickGlossaryModPromise;
  }

  async function openQuickGlossaryDialog(kind) {
    const curProj = getTextState?.()?.textProjectId;
    if (!curProj) {
      const st = $("human-toggle-note");
      if (st) st.textContent = "请先选择项目。";
      return;
    }
    const mod = await loadQuickGlossaryMod();
    const sel = quickLastSelection || mod.extractSelectionText(doc);
    quickLastSelection = "";
    quickGlossaryKind = kind;
    const dlg = $("quick-glossary-dialog");
    const title = $("quick-glossary-title");
    const termBox = $("quick-term-fields");
    const personBox = $("quick-person-fields");
    const status = $("quick-glossary-status");
    if (status) status.textContent = "";
    if (kind === "person") {
      if (title) title.textContent = "快捷收录人物";
      if (termBox) termBox.hidden = true;
      if (personBox) personBox.hidden = false;
      $("quick-person-name").value = sel;
      $("quick-person-alias").value = "";
      $("quick-person-voice").value = "";
    } else {
      if (title) title.textContent = "快捷收录术语";
      if (termBox) termBox.hidden = false;
      if (personBox) personBox.hidden = true;
      $("quick-term-source").value = sel;
      $("quick-term-target").value = "";
      $("quick-term-category").value = "common";
      $("quick-term-note").value = "";
    }
    if (dlg && typeof dlg.showModal === "function") dlg.showModal();
    const firstInput =
      kind === "person"
        ? $("quick-person-name")
        : sel
          ? $("quick-term-target")
          : $("quick-term-source");
    firstInput?.focus?.();
  }

  async function submitQuickGlossary() {
    const curProj = getTextState?.()?.textProjectId;
    if (!curProj) return;
    const mod = await loadQuickGlossaryMod();
    const status = $("quick-glossary-status");
    const fields =
      quickGlossaryKind === "person"
        ? {
            name: $("quick-person-name").value,
            alias: $("quick-person-alias").value,
            voice: $("quick-person-voice").value,
          }
        : {
            source: $("quick-term-source").value,
            target: $("quick-term-target").value,
            category: $("quick-term-category").value,
            note: $("quick-term-note").value,
          };
    const check = mod.validateQuickMaterialFields(quickGlossaryKind, fields);
    if (!check.ok) {
      if (status) status.textContent = check.error;
      return;
    }
    if (status) status.textContent = "正在写入本地资料草稿…";
    try {
      const payload = mod.buildQuickMaterialPayload(quickGlossaryKind, fields);
      const receipt = await api(
        "/text/" + encodeURIComponent(curProj) + "/materials/quick-add",
        payload,
      );
      const msg = mod.formatQuickMaterialReceipt(receipt);
      if (status) status.textContent = msg;
      const note = $("human-toggle-note");
      if (note) note.textContent = msg;
      $("quick-glossary-dialog")?.close?.();
    } catch (e) {
      const advise = materialAdvice ? materialAdvice(e?.body?.error || e?.message) : e?.message;
      if (status) status.textContent = "收录失败：" + advise;
    }
  }

  // ---- 绑定 DOM 与全局快捷键 ----
  $("neo-mode-btn")?.addEventListener?.("change", (e) => {
    setHash(e.target.checked ? "#neo" : "#" + (getPreviousClassicHall?.() || "fengtian"));
    showHall?.();
  });
  $("doctor-open-btn")?.addEventListener?.("click", openDoctorModal);
  $("doctor-close-btn")?.addEventListener?.("click", closeDoctorModal);
  $("doctor-refresh-btn")?.addEventListener?.("click", loadDoctorReport);
  $("doctor-modal")?.addEventListener?.("click", (e) => {
    if (e.target === $("doctor-modal")) closeDoctorModal();
  });

  $("cmd-palette-btn")?.addEventListener?.("click", openCommandPalette);
  $("cmd-palette-close")?.addEventListener?.("click", closeCommandPalette);
  $("cmd-palette")?.addEventListener?.("click", (e) => {
    if (e.target === $("cmd-palette")) closeCommandPalette();
  });
  $("cmd-palette-input")?.addEventListener?.("input", () => {
    cmdPaletteIndex = 0;
    renderCommandPaletteList();
  });
  $("cmd-palette-input")?.addEventListener?.("keydown", (e) => {
    if (e.key === "ArrowDown") {
      e.preventDefault?.();
      if (cmdPaletteItems.length) {
        cmdPaletteIndex = (cmdPaletteIndex + 1) % cmdPaletteItems.length;
        renderCommandPaletteList();
      }
    } else if (e.key === "ArrowUp") {
      e.preventDefault?.();
      if (cmdPaletteItems.length) {
        cmdPaletteIndex =
          (cmdPaletteIndex - 1 + cmdPaletteItems.length) % cmdPaletteItems.length;
        renderCommandPaletteList();
      }
    } else if (e.key === "Enter") {
      e.preventDefault?.();
      const chosen = cmdPaletteItems[cmdPaletteIndex];
      if (chosen) {
        closeCommandPalette();
        chosen.run();
      }
    } else if (e.key === "Escape") {
      e.preventDefault?.();
      closeCommandPalette();
    }
  });

  for (const btnId of ["quick-term-btn", "quick-person-btn"]) {
    $(btnId)?.addEventListener?.("mousedown", () => {
      const el = doc?.activeElement;
      if (
        el &&
        typeof el.value === "string" &&
        typeof el.selectionStart === "number" &&
        el.selectionEnd > el.selectionStart
      ) {
        quickLastSelection = el.value
          .slice(el.selectionStart, el.selectionEnd)
          .trim()
          .slice(0, 120);
      } else {
        quickLastSelection = (win?.getSelection?.()?.toString() || "")
          .trim()
          .slice(0, 120);
      }
    });
  }
  $("quick-term-btn")?.addEventListener?.("click", () => openQuickGlossaryDialog("term"));
  $("quick-person-btn")?.addEventListener?.("click", () => openQuickGlossaryDialog("person"));
  $("quick-glossary-cancel")?.addEventListener?.("click", () =>
    $("quick-glossary-dialog")?.close?.(),
  );
  $("quick-glossary-submit")?.addEventListener?.("click", submitQuickGlossary);

  doc?.addEventListener?.("keydown", (e) => {
    const key = String(e.key || "");
    if ((e.ctrlKey || e.metaKey) && key.toLowerCase() === "k") {
      e.preventDefault?.();
      const modal = $("cmd-palette");
      if (modal && !modal.hidden) closeCommandPalette();
      else openCommandPalette();
    } else if (e.altKey && !e.ctrlKey && !e.metaKey && key.toLowerCase() === "n") {
      e.preventDefault?.();
      toggleNeoMode();
    } else if (e.altKey && !e.ctrlKey && !e.metaKey && key === "[") {
      const qaNav = getQaNavigator?.();
      if (qaNav?.isActive?.()) {
        e.preventDefault?.();
        qaNav.prev();
      }
    } else if (e.altKey && !e.ctrlKey && !e.metaKey && key === "]") {
      const qaNav = getQaNavigator?.();
      if (qaNav?.isActive?.()) {
        e.preventDefault?.();
        qaNav.next();
      }
    } else if (key === "Escape") {
      const modal = $("cmd-palette");
      if (modal && !modal.hidden) closeCommandPalette();
      const dModal = $("doctor-modal");
      if (dModal && !dModal.hidden) closeDoctorModal();
    }
  });

  return {
    ensureNeoPanel,
    renderNeoView,
    toggleNeoMode,
    openCommandPalette,
    closeCommandPalette,
    openDoctorModal,
    closeDoctorModal,
    openQuickGlossaryDialog,
    applyNeoLayoutClasses,
  };
}

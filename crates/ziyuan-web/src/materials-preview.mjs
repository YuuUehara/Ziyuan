// Read-only, single-run identity preview. No network globals or persistent state.
const clone = (value) => JSON.parse(JSON.stringify(value));
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const exactKeys = (value, keys) => isObject(value)
  && Object.keys(value).length === keys.length && keys.every((key) => Object.hasOwn(value, key));
const strings = (value) => Array.isArray(value) && value.every((item) => typeof item === "string");
const hash = (value) => typeof value === "string" && /^[0-9a-f]{64}$/.test(value);
const status = (value) => value === "confirmed" || value === "tentative";

function validIdentity(value) {
  return exactKeys(value, ["kind", "id", "rule_index"])
    && ["term", "person", "address_rule"].includes(value.kind) && typeof value.id === "string"
    && (value.rule_index === null || Number.isInteger(value.rule_index));
}
function validParagraphEntry(value, kind) {
  const keys = kind === "terms" ? ["term_id", "status", "paragraphs"]
    : ["person_id", "status", "paragraphs"];
  return exactKeys(value, keys) && typeof value[keys[0]] === "string"
    && status(value.status) && strings(value.paragraphs);
}
function validSelection(value) {
  if (!exactKeys(value, ["terms", "persons", "address_rules"])) return false;
  return ["terms", "persons"].every((kind) => Array.isArray(value[kind])
    && value[kind].every((entry) => validParagraphEntry(entry, kind)))
    && Array.isArray(value.address_rules) && value.address_rules.every((rule) =>
      exactKeys(rule, ["person_id", "rule_index", "person_status", "term_status", "paragraphs"])
      && typeof rule.person_id === "string" && Number.isInteger(rule.rule_index)
      && status(rule.person_status) && status(rule.term_status) && strings(rule.paragraphs));
}
function validDiagnostic(value) {
  return exactKeys(value, ["identity", "reason", "dependency", "paragraphs"])
    && validIdentity(value.identity) && typeof value.reason === "string"
    && (value.dependency === null || validIdentity(value.dependency)) && strings(value.paragraphs);
}

// Reject, rather than partially display, a response outside the identity-only contract.
function validResponse(value, input) {
  return isObject(value) && exactKeys(value.binding,
    ["content_hash", "project_hash", "block_hash", "associations_hash"])
    && ["content_hash", "block_hash", "associations_hash"].every((key) => hash(value.binding[key]))
    && value.binding.project_hash === input.project_hash
    && exactKeys(value.report, ["project", "block", "applicable", "automatic", "diagnostics"])
    && value.report.project === input.project_id && value.report.block === input.block
    && validSelection(value.report.applicable) && validSelection(value.report.automatic)
    && Array.isArray(value.report.diagnostics) && value.report.diagnostics.every(validDiagnostic);
}

/**
 * editor supplies previewState(), beginReceive(), update(), and subscribe().
 * getView() and runPreview(request) are injected by the integration layer.
 * update(view, ticket) is also the page-entry integration point.
 */
export function mountMaterialPreview(host, { editor, getView, runPreview } = {}) {
  if (!editor || typeof getView !== "function" || typeof runPreview !== "function") {
    throw new Error("Preview requires an editor and injected GET/preview callbacks.");
  }
  const doc = host.ownerDocument;
  let disposed = false;
  let epoch = 0;
  let selectedBlock = "";
  let associations = new Map();
  let lastKey = null;
  let lastIdentity = null;
  let success = null;
  let message = "";
  let running = false;

  const make = (tag, props = {}) => {
    const node = doc.createElement(tag);
    for (const [key, value] of Object.entries(props)) {
      if (key === "dataset") Object.assign(node.dataset, value);
      else node[key] = value;
    }
    return node;
  };
  const on = (node, type, handler) => node.addEventListener(type, (event) => {
    if (!disposed) handler(event);
  });
  const text = (tag, textContent, dataset = {}) => make(tag, { textContent, dataset });
  const root = make("section", { className: "materials-preview", dataset: { role: "materials-preview" } });
  const localWarning = text("p", "完整本地资料卡仅用于当前编辑；预览不是批准，不会保存资料或拼装提示词。",
    { role: "local-warning" });
  const card = make("pre", { dataset: { role: "material-card" } });
  const blockSelect = make("select", { dataset: { role: "block" } });
  const paragraphs = make("div", { dataset: { role: "paragraphs" } });
  const associationHost = make("div", { dataset: { role: "associations" } });
  const runButton = text("button", "运行身份预览", { role: "run" });
  runButton.type = "button";
  const refreshButton = text("button", "刷新并清空临时关联", { role: "refresh" });
  refreshButton.type = "button";
  const statusNode = make("p", { dataset: { role: "preview-status" } });
  statusNode.setAttribute("role", "status");
  const projectHash = text("p", "", { role: "preview-project-hash" });
  const previewWarning = text("p", "报告仅绑定本次输入与 project_hash；外部磁盘变化后必须重新核验，不代表持续有效。",
    { role: "preview-warning" });
  const reportHost = make("div", { dataset: { role: "report-host" } });
  root.append(localWarning, card, blockSelect, paragraphs, associationHost, runButton, refreshButton,
    statusNode, projectHash, previewWarning, reportHost);
  host.replaceChildren(root);

  function state() { return editor.previewState(); }
  function blocks() {
    return state()?.view?.project?.chapters?.flatMap((chapter) => chapter.blocks ?? []) ?? [];
  }
  function currentBlock() { return blocks().find((item) => item.block_id === selectedBlock); }
  function people() { return state()?.material?.persons ?? []; }

  function resetAssociations() {
    associations = new Map((currentBlock()?.paragraphs ?? []).map((paragraph) => [paragraph.id, {
      persons: new Set(), targets: new Set(), speaker: { kind: "", person_id: "" },
    }]));
  }
  function associationPayload() {
    return (currentBlock()?.paragraphs ?? []).map((paragraph) => {
      const entry = associations.get(paragraph.id) ?? {
        persons: new Set(), targets: new Set(), speaker: { kind: "", person_id: "" },
      };
      const output = {
        paragraph: paragraph.id,
        persons: people().map((person) => person.person_id).filter((id) => entry.persons.has(id)),
        targets: people().map((person) => person.person_id).filter((id) => entry.targets.has(id)),
      };
      if (entry.speaker.kind === "person") {
        output.speaker = { kind: "person", person_id: entry.speaker.person_id };
      } else if (entry.speaker.kind === "narration" || entry.speaker.kind === "unknown") {
        output.speaker = { kind: entry.speaker.kind };
      }
      return output;
    });
  }
  function snapshot() {
    const current = state();
    if (!current) return null;
    return {
      workspace_id: current.workspace_id, project_id: current.project_id,
      project_hash: current.project_hash, local_epoch: current.local_epoch,
      material: clone(current.material), block: selectedBlock, associations: associationPayload(),
    };
  }
  function key() { return JSON.stringify(snapshot()); }

  function invalidate(nextMessage = "输入已变化，结果已失效。") {
    epoch++;
    success = null;
    running = false;
    message = nextMessage;
    lastKey = key();
    render();
  }

  function syncFromEditor() {
    if (disposed) return;
    const current = state();
    if (!current) {
      epoch++;
      success = null;
      associations = new Map();
      lastKey = null;
      lastIdentity = null;
      render();
      return;
    }
    const identity = JSON.stringify([current.workspace_id, current.project_id]);
    if (identity !== lastIdentity || !currentBlock()) {
      selectedBlock = blocks()[0]?.block_id ?? "";
      resetAssociations();
      lastIdentity = identity;
      lastKey = null;
    }
    const nextKey = key();
    if (lastKey !== null && nextKey !== lastKey) {
      epoch++;
      success = null;
      running = false;
      message = "输入已变化，结果已失效。";
    }
    lastKey = nextKey;
    render();
  }

  // Keep value and label assignment together without HTML parsing.
  function makeOption(value, label = value) {
    const node = text("option", label);
    node.value = value;
    return node;
  }

  function renderAssociations() {
    associationHost.replaceChildren();
    for (const paragraph of currentBlock()?.paragraphs ?? []) {
      const entry = associations.get(paragraph.id);
      if (!entry) continue;
      const row = make("fieldset", { dataset: { role: "association", paragraph: paragraph.id } });
      row.append(text("legend", paragraph.id));
      for (const kind of ["persons", "targets"]) {
        const group = make("div", { dataset: { role: kind, paragraph: paragraph.id } });
        group.append(text("span", kind === "persons" ? "关联人物" : "称呼目标"));
        for (const person of people()) {
          const label = make("label");
          const input = make("input", { type: "checkbox", checked: entry[kind].has(person.person_id),
            dataset: { association: kind, paragraph: paragraph.id, id: person.person_id } });
          on(input, "change", () => {
            if (input.checked) entry[kind].add(person.person_id);
            else entry[kind].delete(person.person_id);
            invalidate();
          });
          label.append(input, text("span", person.person_id));
          group.append(label);
        }
        row.append(group);
      }
      const speaker = make("select", { dataset: { role: "speaker", paragraph: paragraph.id } });
      for (const [value, label] of [["", "未标注"], ["person", "人物"],
        ["narration", "旁白"], ["unknown", "未知"]]) speaker.append(makeOption(value, label));
      speaker.value = entry.speaker.kind;
      on(speaker, "change", () => {
        entry.speaker.kind = speaker.value;
        if (speaker.value === "person" && !entry.speaker.person_id) {
          entry.speaker.person_id = people()[0]?.person_id ?? "";
        }
        invalidate();
      });
      row.append(speaker);
      if (entry.speaker.kind === "person") {
        const person = make("select", { dataset: { role: "speaker-person", paragraph: paragraph.id } });
        for (const candidate of people()) person.append(makeOption(candidate.person_id));
        person.value = entry.speaker.person_id;
        on(person, "change", () => { entry.speaker.person_id = person.value; invalidate(); });
        row.append(person);
      }
      associationHost.append(row);
    }
  }

  function renderSelection(parent, group, selection) {
    for (const kind of ["terms", "persons", "address_rules"]) {
      const section = make("div", { dataset: { role: `${group}-${kind}` } });
      section.append(text("h4", `${group} ${kind}`));
      for (const entry of selection[kind]) {
        const id = kind === "terms" ? entry.term_id : entry.person_id;
        const detail = kind === "address_rules"
          ? `${id} · rule_index=${entry.rule_index} · ${entry.person_status}/${entry.term_status} · ${entry.paragraphs.join(", ")}`
          : `${id} · ${entry.status} · ${entry.paragraphs.join(", ")}`;
        section.append(text("div", detail, { role: "report-entry", reportGroup: group, reportKind: kind, id }));
      }
      parent.append(section);
    }
  }

  function renderReport() {
    reportHost.replaceChildren();
    projectHash.textContent = success ? `project_hash: ${success.value.binding.project_hash}` : "";
    if (!success) return;
    const { report } = success.value;
    const reportNode = make("article", { dataset: { role: "identity-report", block: report.block } });
    reportNode.append(text("h3", `身份可见性报告 · ${report.project} · ${report.block}`));
    renderSelection(reportNode, "applicable", report.applicable);
    renderSelection(reportNode, "automatic", report.automatic);
    const diagnostics = make("div", { dataset: { role: "diagnostics" } });
    for (const diagnostic of report.diagnostics) {
      const identity = diagnostic.identity;
      const dependency = diagnostic.dependency
        ? ` · dependency=${diagnostic.dependency.kind}:${diagnostic.dependency.id}:${diagnostic.dependency.rule_index}` : "";
      diagnostics.append(text("div", `${identity.kind}:${identity.id}:${identity.rule_index} · ${diagnostic.reason}${dependency} · ${diagnostic.paragraphs.join(", ")}`,
        { role: "diagnostic", id: identity.id }));
    }
    reportNode.append(diagnostics);
    reportHost.append(reportNode);
  }

  function render() {
    const current = state();
    card.textContent = current ? JSON.stringify(current.material, null, 2) : "";
    blockSelect.replaceChildren();
    for (const item of blocks()) blockSelect.append(makeOption(item.block_id));
    blockSelect.value = selectedBlock;
    paragraphs.replaceChildren();
    for (const paragraph of currentBlock()?.paragraphs ?? []) {
      paragraphs.append(text("div", `${paragraph.id} · ${paragraph.text}`, { paragraph: paragraph.id }));
    }
    renderAssociations();
    runButton.disabled = disposed || !current || !currentBlock();
    refreshButton.disabled = disposed || !current;
    statusNode.textContent = running ? "正在重新核验并预览……" : message;
    renderReport();
  }

  async function refresh() {
    if (disposed || !state()) return false;
    const ticket = editor.beginReceive();
    const sequence = ++epoch;
    success = null;
    running = false;
    resetAssociations();
    message = "正在重新读取项目……";
    lastKey = key();
    render();
    let view;
    try { view = await getView(); } catch (error) {
      if (sequence === epoch && !disposed) {
        success = null;
        message = `读取失败：${error.message}`;
        lastKey = key();
        render();
      }
      return false;
    }
    if (disposed || sequence !== epoch) return false;
    const accepted = editor.update(view, ticket);
    if (!accepted) return false;
    epoch++;
    success = null;
    resetAssociations();
    message = "已重新读取；临时关联已清空。";
    lastIdentity = JSON.stringify([state()?.workspace_id, state()?.project_id]);
    if (!currentBlock()) selectedBlock = blocks()[0]?.block_id ?? "";
    resetAssociations();
    lastKey = key();
    render();
    return true;
  }

  function update(view, ticket) {
    if (disposed) return false;
    const accepted = editor.update(view, ticket);
    if (!accepted) return false;
    epoch++;
    success = null;
    running = false;
    const current = state();
    const identity = JSON.stringify([current?.workspace_id, current?.project_id]);
    if (identity !== lastIdentity || !currentBlock()) selectedBlock = blocks()[0]?.block_id ?? "";
    lastIdentity = identity;
    resetAssociations();
    message = "项目视图已更新；旧结果已失效，临时关联已清空。";
    lastKey = key();
    render();
    return true;
  }

  async function run() {
    if (disposed || running || !state() || !currentBlock()) return false;
    const initial = snapshot();
    const initialKey = JSON.stringify(initial);
    const sequence = ++epoch;
    success = null;
    running = true;
    message = "";
    const ticket = editor.beginReceive();
    render();
    let view;
    try { view = await getView(); } catch (error) {
      if (sequence === epoch && !disposed) {
        running = false;
        success = null;
        message = `读取失败：${error.message}`;
        render();
      }
      return false;
    }
    if (disposed || sequence !== epoch) return false;
    if (!editor.update(view, ticket) || sequence !== epoch || key() !== initialKey) {
      if (sequence === epoch) invalidate("项目或输入在预览前已变化，结果已失效。");
      return false;
    }
    const request = {
      workspace_id: initial.workspace_id, material: clone(initial.material),
      block: initial.block, associations: clone(initial.associations),
    };
    let value;
    try { value = await runPreview(request); } catch (error) {
      if (sequence === epoch && !disposed) {
        running = false;
        success = null;
        message = `预览失败：${error.message}`;
        render();
      }
      return false;
    }
    if (disposed || sequence !== epoch || key() !== initialKey) return false;
    running = false;
    if (!validResponse(value, initial)) {
      success = null;
      message = "预览响应无效或输入绑定不匹配；旧结果不会复用。";
      render();
      return false;
    }
    success = { value: clone(value), key: initialKey };
    message = "预览完成；结果仅属于本次完整输入。";
    lastKey = key();
    render();
    return true;
  }

  on(blockSelect, "change", () => {
    selectedBlock = blockSelect.value;
    resetAssociations();
    invalidate();
  });
  on(runButton, "click", () => { run(); });
  on(refreshButton, "click", () => { refresh(); });
  const unsubscribe = editor.subscribe(syncFromEditor);
  syncFromEditor();

  return {
    update,
    enter: refresh,
    refresh,
    focus: refresh,
    run,
    dispose() {
      if (disposed) return;
      disposed = true;
      epoch++;
      running = false;
      success = null;
      associations = new Map();
      unsubscribe();
      host.replaceChildren();
    },
  };
}

import { MaterialModel } from "./materials-model.mjs";

const PAGE_SIZE = 50;
const NESTED_PAGE_SIZE = 10;
const GROUPS = {
  terms: { label: "术语", id: "term_id", name: "source" },
  persons: { label: "人物", id: "person_id", name: "name" },
};
const CATEGORY = [["person_name", "人名"], ["place", "地名"], ["organization", "组织"],
  ["title", "称谓"], ["culture", "文化"], ["common", "通用"]];
const STATUS = [["tentative", "暂定"], ["confirmed", "已确认"]];
const STANCE = [["project_fact", "项目事实"], ["character_belief", "角色认知"], ["narration", "叙述"]];
const SPEAKER = [["person", "人物"], ["narration", "旁白"], ["unknown", "未知"]];
const SCOPE = [["project", "全项目"], ["range", "章节范围"]];

const clone = (value) => JSON.parse(JSON.stringify(value));
const valueAt = (root, path) => path.reduce((value, part) => value?.[part], root);
const display = (value) => (value === undefined ? "（缺失）" : value === "" ? "（空）" : String(value));
// New values start tentative: this editor never confirms or approves material.
const blank = {
  terms: () => ({ term_id: "", source: "", category: "common", allowed: [],
    scope: { kind: "project" }, evidence: [], status: "tentative" }),
  persons: () => ({ person_id: "", name: "", aliases: [], terms: [], address_rules: [],
    voice: { description: "" }, evidence: [], status: "tentative" }),
  evidence: () => ({ paragraph: "", quote: "", explanation: "", stance: "project_fact", status: "tentative" }),
  rule: () => ({ target: "", speaker: { kind: "unknown" }, scope: { kind: "project" }, term: "", evidence: [] }),
  position: () => ({ chapter: "" }),
};

function referenceLocations(material, group, index) {
  const id = material[group][index]?.[GROUPS[group].id];
  const found = [];
  material.persons.forEach((person, p) => {
    if (group === "persons" && p === index) return;
    const owner = `persons[${p}]`;
    if (group === "terms") {
      (person?.terms ?? []).forEach((ref, j) => { if (ref === id) found.push(`${owner}.terms[${j}]`); });
    }
    (person?.address_rules ?? []).forEach((rule, r) => {
      const at = `${owner}.address_rules[${r}]`;
      if (group === "terms" && rule?.term === id) found.push(`${at}.term`);
      if (group === "persons" && rule?.target === id) found.push(`${at}.target`);
      if (group === "persons" && rule?.speaker?.kind === "person" && rule.speaker.person_id === id) {
        found.push(`${at}.speaker.person_id`);
      }
    });
  });
  return found;
}

/**
 * Persistent DOM for one project's materials. update(view, ticket) feeds a
 * materials GET response into the model. Input only changes local model state;
 * nothing here issues requests. The DOM is built with host.ownerDocument and
 * direct listeners only, and user data is assigned through properties.
 */
export function mountMaterialEditor(host, { storage, confirm = globalThis.confirm, viewer = false } = {}) {
  const doc = host.ownerDocument;
  let disposed = false;
  let model = null;
  let currentView = null;
  const listeners = new Set();
  let tab = "terms";
  let composing = null;
  // Updates arriving during an IME composition queue in arrival order; a single
  // slot would let an older response overwrite a newer one before the model's
  // ticket watermark ever saw it.
  let deferred = [];
  let form = null;
  let message = "";
  const selected = { terms: null, persons: null };
  const lastIndex = { terms: 0, persons: 0 };
  const pages = { terms: 0, persons: 0 };
  // Per-entry UI memory: values hidden by a toggle and nested list pages.
  const entryState = new Map();
  let listItems = new Map();

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
  const button = (textContent, dataset = {}) => make("button", { type: "button", textContent, dataset });
  const isViewer = () => (typeof viewer === "function" ? !!viewer() : !!viewer);
  const editable = () => !disposed && !isViewer() && !!model?.canSave;
  const entries = (group) => model?.material?.[group] ?? [];
  const keys = (group) => (model?.material ? model.entryKeys[group] : []);
  const indexOf = (group, key) => (key === null ? -1 : keys(group).indexOf(key));
  const current = () => (form ? entries(form.group)[indexOf(form.group, form.key)] : undefined);
  const state = () => {
    if (!entryState.has(form.key)) entryState.set(form.key, { hidden: new Map(), pages: new Map() });
    return entryState.get(form.key);
  };

  const root = make("div", { className: "materials-editor", dataset: { role: "materials-editor" } });
  const status = make("p", { className: "hint", dataset: { role: "status" } });
  status.setAttribute("role", "status");
  const tabs = make("div", { className: "actions", dataset: { role: "tabs" } });
  tabs.setAttribute("role", "tablist");
  const tabButtons = {};
  for (const group of Object.keys(GROUPS)) {
    const node = button(GROUPS[group].label, { role: "tab", group });
    node.setAttribute("role", "tab");
    on(node, "click", () => switchTab(group));
    on(node, "keydown", (event) => {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      event.preventDefault();
      const other = group === "terms" ? "persons" : "terms";
      switchTab(other);
      tabButtons[other].focus();
    });
    tabButtons[group] = node;
    tabs.append(node);
  }
  const addButton = button("", { role: "add" });
  on(addButton, "click", addEntry);
  const list = make("div", { className: "materials-list", dataset: { role: "list" } });
  list.setAttribute("role", "listbox");
  const pager = make("div", { className: "actions", dataset: { role: "pager" } });
  const prevPage = button("上一页", { role: "prev-page" });
  const nextPage = button("下一页", { role: "next-page" });
  const pageInfo = make("span", { dataset: { role: "page-info" } });
  on(prevPage, "click", () => { pages[tab]--; renderList(); });
  on(nextPage, "click", () => { pages[tab]++; renderList(); });
  pager.append(prevPage, pageInfo, nextPage);
  const formHost = make("div", { className: "materials-form", dataset: { role: "form" } });
  root.append(status, tabs, addButton, list, pager, formHost);
  host.replaceChildren(root);

  function finishComposition() {
    const active = composing;
    composing = null;
    active?.commit();
  }

  function applyDeferred() {
    if (!deferred.length || composing) return;
    const pending = deferred;
    deferred = [];
    for (const { view, ticket } of pending) update(view, ticket);
  }

  // Dropping an optional entry field replaces the entry object, which gives it a new model key.
  function follow(group, index, oldKey) {
    const newKey = keys(group)[index];
    if (newKey === oldKey) return;
    if (entryState.has(oldKey)) {
      entryState.set(newKey, entryState.get(oldKey));
      entryState.delete(oldKey);
    }
    if (selected[group] === oldKey) selected[group] = newKey;
    if (form?.key === oldKey) form.key = newKey;
  }

  // value undefined removes the field, because the editable schema rejects null options.
  function write(rel, value) {
    if (!editable() || !form) return false;
    const { group, key } = form;
    const index = indexOf(group, key);
    if (index < 0) return false;
    try {
      if (value === undefined) {
        const parentRel = rel.slice(0, -1);
        const parent = clone(valueAt(entries(group)[index], parentRel));
        delete parent[rel.at(-1)];
        model.edit([group, index, ...parentRel], parent);
        if (!parentRel.length) follow(group, index, key);
      } else {
        model.edit([group, index, ...rel], value);
      }
    } catch (error) {
      message = `无法修改 ${rel.join(".")}：${error.message}`;
      renderStatus();
      return false;
    }
    message = "";
    form.json = JSON.stringify(entries(group)[index]);
    relabel();
    renderStatus();
    for (const listener of [...listeners]) listener();
    return true;
  }

  // Memory is keyed by index path, so it must follow the items that shift down after a removal.
  function shiftMemory(rel, removed) {
    const prefix = `${rel.join(".")}.`;
    for (const memory of Object.values(state())) {
      // Collect first: renaming in place would overwrite a later index not yet visited.
      const moved = [];
      for (const [id, value] of memory) {
        if (!id.startsWith(prefix)) continue;
        const [head, ...rest] = id.slice(prefix.length).split(".");
        const index = Number(head);
        if (!Number.isInteger(index) || index < removed) continue;
        moved.push([id, index > removed ? [prefix + (index - 1), ...rest].join(".") : null, value]);
      }
      for (const [id] of moved) memory.delete(id);
      for (const [, id, value] of moved) if (id !== null) memory.set(id, value);
    }
  }

  function location(rel) {
    const path = [form.group, indexOf(form.group, form.key), ...rel];
    return path.map((part, i) => (typeof part === "number" ? `[${part}]` : i ? `.${part}` : part)).join("");
  }

  function register(node, kind, rel) {
    const id = rel.join(".");
    node.dataset[kind] = id;
    form.fields.set(`${kind}:${id}`, node);
    return node;
  }

  function labelled(text, controlNode) {
    const label = make("label");
    label.append(make("span", { textContent: text }), controlNode);
    return label;
  }

  function fieldset(legend) {
    const node = make("fieldset");
    node.dataset.legend = legend;
    node.append(make("legend", { textContent: legend }));
    return node;
  }

  function textField(box, label, rel, multiline = false) {
    const input = make(multiline ? "textarea" : "input");
    if (!multiline) input.type = "text";
    const stored = () => String(valueAt(current(), rel) ?? "");
    input.value = stored();
    register(input, "field", rel);
    const commit = () => { if (!write(rel, input.value)) input.value = stored(); };
    on(input, "compositionstart", () => { composing = { input, commit }; });
    on(input, "input", (event) => {
      if (composing?.input === input || event.isComposing) return;
      commit();
    });
    on(input, "compositionend", () => {
      if (composing?.input === input) composing = null;
      commit();
      applyDeferred();
    });
    box.append(labelled(label, input));
    return input;
  }

  function option(value, textContent, isRetained = false) {
    const node = make("option", { value: String(value ?? ""), textContent });
    if (isRetained) node.dataset.retained = "";
    return node;
  }

  function selectField(box, label, rel, choices, missingLabel, choose) {
    const select = make("select");
    const value = valueAt(current(), rel);
    const known = choices.some(([choice]) => choice === value);
    if (!known) select.append(option(value, `${display(value)}（${missingLabel}）`, true));
    for (const [choice, text] of choices) select.append(option(choice, text));
    select.value = String(value ?? "");
    register(select, "field", rel);
    on(select, "change", () => {
      const before = valueAt(current(), rel);
      if (!known && select.value === String(before ?? "")) return;
      if (!(choose ? choose(select.value) : write(rel, select.value))) select.value = String(before ?? "");
    });
    box.append(labelled(label, select));
    return select;
  }

  const choiceField = (box, label, rel, choices, choose) =>
    selectField(box, label, rel, choices, "未知值", choose);

  function referenceField(box, label, rel, group) {
    const seen = new Set();
    const choices = [];
    for (const entry of entries(group)) {
      const id = entry?.[GROUPS[group].id];
      if (typeof id !== "string" || seen.has(id)) continue;
      seen.add(id);
      choices.push([id, `${id === "" ? "（空 ID）" : id} · ${entry[GROUPS[group].name] || "（未命名）"}`]);
    }
    return selectField(box, label, rel, choices, "引用不存在");
  }

  function optional(box, label, rel, makeDefault, build) {
    const value = valueAt(current(), rel);
    const present = value !== undefined && value !== null;
    const toggle = register(make("input", { type: "checkbox", checked: present }), "toggle", rel);
    on(toggle, "change", () => {
      const memory = state().hidden;
      const id = rel.join(".");
      if (toggle.checked) {
        if (!write(rel, memory.has(id) ? memory.get(id) : makeDefault())) toggle.checked = false;
      } else {
        const prior = valueAt(current(), rel);
        if (write(rel, undefined)) {
          if (prior !== undefined && prior !== null) memory.set(id, clone(prior));
        } else {
          toggle.checked = true;
        }
      }
      rebuildForm();
    });
    box.append(labelled(label, toggle));
    if (present) {
      const inner = make("div", { className: "material-optional" });
      build(inner, rel);
      box.append(inner);
    }
  }

  // Switching a tagged union keeps the abandoned variant so switching back restores it.
  function variant(rel, keep, next) {
    const memory = state().hidden;
    const id = rel.join(".");
    const prior = valueAt(current(), rel);
    if (prior?.kind === keep) memory.set(id, clone(prior));
    const ok = write(rel, next(memory.get(id)));
    rebuildForm();
    return ok;
  }

  function positionField(box, label, rel) {
    const wrap = fieldset(label);
    textField(wrap, "章节", [...rel, "chapter"]);
    optional(wrap, "指定段落", [...rel, "paragraph"], () => "",
      (inner, path) => textField(inner, "段落", path));
    box.append(wrap);
  }

  function scopeField(box, label, rel) {
    const wrap = fieldset(label);
    choiceField(wrap, "类型", [...rel, "kind"], SCOPE, (kind) => variant(rel, "range",
      (kept) => (kind === "range"
        ? kept ?? { kind: "range", from: blank.position(), to: blank.position() } : { kind })));
    if (valueAt(current(), rel)?.kind === "range") {
      positionField(wrap, "起点", [...rel, "from"]);
      positionField(wrap, "终点", [...rel, "to"]);
    }
    box.append(wrap);
  }

  function speakerField(box, rel) {
    const wrap = fieldset("说话人");
    choiceField(wrap, "类型", [...rel, "kind"], SPEAKER, (kind) => variant(rel, "person",
      (kept) => (kind === "person" ? kept ?? { kind: "person", person_id: "" } : { kind })));
    if (valueAt(current(), rel)?.kind === "person") {
      referenceField(wrap, "说话人物", [...rel, "person_id"], "persons");
    }
    box.append(wrap);
  }

  function arrayField(box, label, rel, makeNew, buildItem) {
    const wrap = fieldset(label);
    const values = valueAt(current(), rel);
    box.append(wrap);
    if (!Array.isArray(values)) {
      wrap.append(make("p", { className: "hint", textContent: "（不是数组，无法在此编辑）" }));
      return;
    }
    const pageState = state().pages;
    const id = rel.join(".");
    const pageCount = Math.max(1, Math.ceil(values.length / NESTED_PAGE_SIZE));
    const page = Math.min(pageState.get(id) ?? 0, pageCount - 1);
    const start = page * NESTED_PAGE_SIZE;
    for (let index = start; index < Math.min(values.length, start + NESTED_PAGE_SIZE); index++) {
      const row = make("div", { className: "material-item" });
      buildItem(row, [...rel, index], index);
      const remove = register(button("移除"), "remove", [...rel, index]);
      on(remove, "click", () => {
        if (!editable() || !form) return;
        finishComposition();
        const latest = valueAt(current(), rel);
        if (!Array.isArray(latest) || index >= latest.length) return;
        const item = latest[index];
        const prompt = [`移除 ${location([...rel, index])}？`,
          typeof item === "string" ? `内容：${display(item)}` : "",
          "其他条目中的引用不会随之修改。"].filter(Boolean).join("\n");
        if (typeof confirm !== "function" || !confirm(prompt)) return;
        if (!write(rel, latest.filter((_, i) => i !== index))) return;
        shiftMemory(rel, index);
        rebuildForm();
      });
      row.append(remove);
      wrap.append(row);
    }
    if (values.length > NESTED_PAGE_SIZE) {
      const nav = make("div", { className: "actions" });
      const prev = register(button("上一页"), "prev", rel);
      const next = register(button("下一页"), "next", rel);
      prev.disabled = page === 0;
      next.disabled = page >= pageCount - 1;
      on(prev, "click", () => { pageState.set(id, page - 1); rebuildForm(); });
      on(next, "click", () => { pageState.set(id, page + 1); rebuildForm(); });
      nav.append(prev, make("span", { textContent: `${page + 1}/${pageCount} 页 · 共 ${values.length} 项` }), next);
      wrap.append(nav);
    }
    const addItem = register(button("添加"), "add", rel);
    on(addItem, "click", () => {
      const latest = valueAt(current(), rel);
      if (!write(rel, [...latest, makeNew()])) return;
      pageState.set(id, Math.floor(latest.length / NESTED_PAGE_SIZE));
      rebuildForm();
    });
    wrap.append(addItem);
  }

  const stringList = (box, label, rel, itemLabel, multiline = false) => arrayField(box, label, rel,
    () => "", (row, path, index) => textField(row, `${itemLabel} #${index + 1}`, path, multiline));

  function evidenceItem(box, rel) {
    textField(box, "段落", [...rel, "paragraph"]);
    textField(box, "引文", [...rel, "quote"], true);
    textField(box, "说明", [...rel, "explanation"], true);
    choiceField(box, "立场", [...rel, "stance"], STANCE);
    choiceField(box, "状态", [...rel, "status"], STATUS);
  }

  const evidenceList = (box, rel) => arrayField(box, "出处", rel, blank.evidence, evidenceItem);

  function ruleItem(box, rel) {
    referenceField(box, "称呼对象", [...rel, "target"], "persons");
    speakerField(box, [...rel, "speaker"]);
    scopeField(box, "适用范围", [...rel, "scope"]);
    referenceField(box, "称呼术语", [...rel, "term"], "terms");
    optional(box, "备注", [...rel, "note"], () => "", (inner, path) => textField(inner, "备注", path, true));
    evidenceList(box, [...rel, "evidence"]);
  }

  function idWarning(box) {
    box.append(make("p", { className: "hint warning", dataset: { role: "id-warning" },
      textContent: "修改 ID 会使其他条目中引用旧 ID 的位置失效；编辑器不会自动更新这些引用。" }));
  }

  function termForm(box) {
    textField(box, "术语 ID", ["term_id"]);
    idWarning(box);
    textField(box, "原文", ["source"]);
    choiceField(box, "类别", ["category"], CATEGORY);
    stringList(box, "允许译法", ["allowed"], "译法");
    optional(box, "备注", ["note"], () => "", (inner, path) => textField(inner, "备注", path, true));
    scopeField(box, "适用范围", ["scope"]);
    optional(box, "生效位置", ["available_from"], blank.position,
      (inner, path) => positionField(inner, "生效位置", path));
    evidenceList(box, ["evidence"]);
    choiceField(box, "状态", ["status"], STATUS);
  }

  function personForm(box) {
    textField(box, "人物 ID", ["person_id"]);
    idWarning(box);
    textField(box, "姓名", ["name"]);
    optional(box, "读音", ["reading"], () => "", (inner, path) => textField(inner, "读音", path));
    optional(box, "生效位置", ["available_from"], blank.position,
      (inner, path) => positionField(inner, "生效位置", path));
    stringList(box, "别名", ["aliases"], "别名");
    arrayField(box, "关联术语", ["terms"], () => "",
      (row, path, index) => referenceField(row, `术语 #${index + 1}`, path, "terms"));
    arrayField(box, "称呼规则", ["address_rules"], blank.rule, ruleItem);
    const voice = fieldset("语气");
    textField(voice, "描述", ["voice", "description"], true);
    optional(voice, "示例", ["voice", "examples"], () => [],
      (inner, path) => stringList(inner, "示例", path, "示例", true));
    box.append(voice);
    evidenceList(box, ["evidence"]);
    choiceField(box, "状态", ["status"], STATUS);
  }

  function activeFieldId() {
    for (const [id, node] of form?.fields ?? []) if (node === doc.activeElement) return id;
    return null;
  }

  function renderForm(force) {
    const group = tab;
    const key = selected[group];
    const index = indexOf(group, key);
    if (index < 0) {
      form = null;
      formHost.replaceChildren(make("p", { className: "hint",
        textContent: model?.canSave ? `选择或新增${GROUPS[group].label}。` : "资料尚未载入。" }));
      return;
    }
    const json = JSON.stringify(entries(group)[index]);
    if (!force && form?.group === group && form.key === key && form.json === json) {
      applyEditable();
      return;
    }
    const focused = form?.group === group && form.key === key ? activeFieldId() : null;
    form = { group, key, json, fields: new Map() };
    const box = make("div", { dataset: { role: "entry-form", group } });
    const remove = register(button(`删除此${GROUPS[group].label}`), "action", ["delete"]);
    on(remove, "click", deleteEntry);
    box.append(remove);
    (group === "terms" ? termForm : personForm)(box);
    formHost.replaceChildren(box);
    applyEditable();
    if (focused && form.fields.has(focused)) form.fields.get(focused).focus();
  }

  // Never replace DOM under an active IME composition.
  function rebuildForm() {
    if (!composing) renderForm(true);
  }

  function applyEditable() {
    const can = editable();
    const readOnly = isViewer();
    addButton.textContent = `新增${GROUPS[tab].label}`;
    addButton.hidden = readOnly;
    addButton.disabled = !can;
    for (const [id, node] of form?.fields ?? []) {
      const kind = id.slice(0, id.indexOf(":"));
      if (kind === "field" && node.tagName !== "SELECT") node.readOnly = !can;
      else if (kind === "field" || kind === "toggle") node.disabled = !can;
      else if (kind === "add" || kind === "remove" || kind === "action") {
        node.hidden = readOnly;
        node.disabled = !can;
      }
    }
  }

  function idCounts(group) {
    const counts = new Map();
    for (const entry of entries(group)) {
      const id = entry?.[GROUPS[group].id];
      counts.set(id, (counts.get(id) ?? 0) + 1);
    }
    return counts;
  }

  function labelFor(group, entry, counts) {
    const id = entry?.[GROUPS[group].id];
    const name = entry?.[GROUPS[group].name];
    return `${id === "" ? "（空 ID）" : display(id)}${counts.get(id) > 1 ? "（ID 重复）" : ""} · ${name || "（未命名）"}`;
  }

  function relabel() {
    if (!form || form.group !== tab) return;
    const counts = idCounts(tab);
    for (const [index, item] of listItems) item.textContent = labelFor(tab, entries(tab)[index], counts);
  }

  function renderList() {
    const group = tab;
    const all = entries(group);
    const allKeys = keys(group);
    const pageCount = Math.max(1, Math.ceil(all.length / PAGE_SIZE));
    pages[group] = Math.min(Math.max(pages[group], 0), pageCount - 1);
    let focused = null;
    for (const [index, item] of listItems) if (item === doc.activeElement) focused = index;
    const counts = idCounts(group);
    const start = pages[group] * PAGE_SIZE;
    listItems = new Map();
    for (let index = start; index < Math.min(all.length, start + PAGE_SIZE); index++) {
      const item = button(labelFor(group, all[index], counts), { role: "item" });
      item.setAttribute("role", "option");
      item.setAttribute("aria-selected", String(allKeys[index] === selected[group]));
      on(item, "click", () => select(group, index));
      on(item, "keydown", (event) => navigate(event, index));
      listItems.set(index, item);
    }
    list.replaceChildren(...listItems.values());
    pager.hidden = pageCount <= 1;
    prevPage.disabled = pages[group] === 0;
    nextPage.disabled = pages[group] >= pageCount - 1;
    pageInfo.textContent = `${pages[group] + 1}/${pageCount} 页 · 共 ${all.length} 条`;
    if (focused !== null) listItems.get(focused)?.focus();
  }

  function navigate(event, index) {
    const count = entries(tab).length;
    const target = { ArrowDown: index + 1, ArrowUp: index - 1, Home: 0, End: count - 1 }[event.key];
    if (target === undefined || target < 0 || target >= count) return;
    event.preventDefault();
    pages[tab] = Math.floor(target / PAGE_SIZE);
    select(tab, target);
    listItems.get(target)?.focus();
  }

  function select(group, index) {
    finishComposition();
    const key = keys(group)[index];
    if (key === undefined) return;
    selected[group] = key;
    lastIndex[group] = index;
    renderList();
    renderForm(false);
    applyDeferred();
  }

  function switchTab(group) {
    finishComposition();
    tab = group;
    render();
    applyDeferred();
  }

  function ensureSelection(group) {
    const all = keys(group);
    const index = indexOf(group, selected[group]);
    if (index >= 0) { lastIndex[group] = index; return; }
    selected[group] = all.length ? all[Math.min(lastIndex[group], all.length - 1)] : null;
    if (selected[group] !== null) pages[group] = Math.floor(all.indexOf(selected[group]) / PAGE_SIZE);
  }

  function addEntry() {
    if (!editable()) return;
    finishComposition();
    const group = tab;
    const before = entries(group);
    model.edit([group], [...before, blank[group]()]);
    const index = before.length;
    selected[group] = keys(group)[index];
    lastIndex[group] = index;
    pages[group] = Math.floor(index / PAGE_SIZE);
    render();
    form?.fields.get(`field:${GROUPS[group].id}`)?.focus();
  }

  function deleteEntry() {
    if (!editable() || !form) return;
    finishComposition();
    const { group, key } = form;
    const index = indexOf(group, key);
    if (index < 0) return;
    const entry = entries(group)[index];
    const references = referenceLocations(model.material, group, index);
    const prompt = [`删除${GROUPS[group].label} #${index + 1}「${display(entry?.[GROUPS[group].id])}」？`,
      references.length ? `以下引用不会随之修改，需另行处理：\n${references.join("\n")}` : "没有其他条目引用它。",
    ].join("\n");
    if (typeof confirm !== "function" || !confirm(prompt)) return;
    model.edit([group], entries(group).filter((_, i) => i !== index));
    entryState.delete(key);
    selected[group] = null;
    lastIndex[group] = index;
    render();
  }

  function renderStatus() {
    const parts = [];
    if (!model?.canSave) parts.push("资料尚未载入");
    else {
      parts.push(model.dirty ? "有未保存的修改" : "与已保存资料一致");
      parts.push(`术语 ${entries("terms").length} 条 · 人物 ${entries("persons").length} 条`);
    }
    if (isViewer()) parts.push("只读");
    if (model?.mirrorError) parts.push(model.mirrorError);
    if (message) parts.push(message);
    status.textContent = parts.join(" · ");
  }

  function render() {
    for (const [group, node] of Object.entries(tabButtons)) {
      node.setAttribute("aria-selected", String(group === tab));
    }
    ensureSelection(tab);
    renderStatus();
    renderList();
    renderForm(false);
    for (const listener of [...listeners]) listener();
  }

  function update(view, ticket) {
    if (disposed) return false;
    if (composing) {
      deferred.push({ view, ticket });
      return false;
    }
    const project = view?.project?.id;
    if (!model || model.project !== project) {
      model = new MaterialModel({ project, storage });
      currentView = null;
      form = null;
      entryState.clear();
      for (const group of Object.keys(GROUPS)) {
        selected[group] = null;
        lastIndex[group] = 0;
        pages[group] = 0;
      }
    }
    const result = model.receive(view, ticket);
    if (result) currentView = clone(view);
    // A clean changed GET replaces entry objects and their keys; memory keyed by
    // keys that no longer exist can never be reached again. Local edits keep keys
    // (follow/shiftMemory), so their memory survives this prune.
    const live = new Set([...keys("terms"), ...keys("persons")]);
    for (const key of entryState.keys()) if (!live.has(key)) entryState.delete(key);
    render();
    applyEditable();
    return result;
  }

  // path: [group, entryIndex, ...fields/indexes], e.g. ["persons", 0, "address_rules", 0, "quote"].
  function focusField(path) {
    if (disposed || !Array.isArray(path)) return false;
    const [group, index, ...rel] = path;
    const key = Object.hasOwn(GROUPS, group) && Number.isInteger(index) ? keys(group)[index] : undefined;
    if (key === undefined) return false;
    finishComposition();
    if (!entryState.has(key)) entryState.set(key, { hidden: new Map(), pages: new Map() });
    const pageState = entryState.get(key).pages;
    rel.forEach((part, i) => {
      if (typeof part === "number" && i > 0) {
        pageState.set(rel.slice(0, i).join("."), Math.floor(part / NESTED_PAGE_SIZE));
      }
    });
    tab = group;
    selected[group] = key;
    lastIndex[group] = index;
    pages[group] = Math.floor(index / PAGE_SIZE);
    render();
    renderForm(true);
    applyDeferred();
    const id = rel.join(".");
    // A field inside an absent option or variant can only be reached through its toggle.
    let target = form?.fields.get(`field:${id}`) ?? form?.fields.get(`toggle:${id}`);
    for (let end = rel.length - 1; !target && end > 0; end--) {
      target = form?.fields.get(`toggle:${rel.slice(0, end).join(".")}`);
    }
    if (!target) return false;
    target.scrollIntoView?.({ block: "nearest" });
    target.focus();
    return true;
  }

  return {
    update,
    focusField,
    // Read-only consumers receive a fresh snapshot; temporary preview state stays
    // outside MaterialModel and its recoverable session mirror.
    addWizardEntry: (group, patch) => {
      if (!editable() || !["terms", "persons"].includes(group)) return false;
      tab = group;
      const before = entries(group);
      const entry = { ...blank[group](), ...patch };
      model.edit([group], [...before, entry]);
      selected[group] = keys(group)[before.length];
      lastIndex[group] = before.length;
      pages[group] = Math.floor(before.length / PAGE_SIZE);
      render();
      form?.fields.get(`field:${GROUPS[group].id}`)?.focus();
      return true;
    },
    previewState: () => model?.canSave && currentView ? {
      view: clone(currentView), workspace_id: model.workspaceId,
      revision: model.remoteRevision,
      project_id: model.project, project_hash: model.projectHash,
      local_epoch: model.localEpoch, material: clone(model.material),
    } : null,
    subscribe(listener) {
      if (disposed || typeof listener !== "function") return () => {};
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    // Task 12 polling helpers: tickets for GETs must come from the same model
    // that receives their responses, or the watermark cannot reject stale ones.
    // beginReceive() returns undefined until the first update creates the model.
    beginReceive: () => model?.beginReceive(),
    needsRefresh: () => !!model?.needsRefresh,
    modelState: () => model ? {
      revision: model.remoteRevision, canSave: model.canSave,
      dirty: model.dirty, mirrorError: model.mirrorError,
    } : null,
    recovery: () => model?.recoveryChoice ? {
      revision: model.recoveryChoice.baseRevision,
      material: clone(model.recoveryChoice.material),
    } : null,
    restoreRecovery: () => {
      const restored = !!model?.restore();
      if (restored) render();
      return restored;
    },
    discardLocal: () => {
      model?.discardLocalConfirmed();
      render();
    },
    // Test-only inspection: entryState is pruned against live keys after each receive.
    _entryStateSize: () => entryState.size,
    hasDirty: () => !disposed && (!!composing || !!model?.dirty),
    flushMirror() {
      if (disposed || !model?.canSave) return;
      composing?.commit();
      model._writeMirror();
    },
    dispose() {
      disposed = true;
      composing = null;
      deferred = [];
      form = null;
      currentView = null;
      listItems = new Map();
      listeners.clear();
      host.replaceChildren();
    },
  };
}

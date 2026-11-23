import test from "node:test";
import assert from "node:assert/strict";

import {
  VERSION_MARK,
  VERSION_MARK_LINE,
  buildDoctorSummaryLine,
  renderDoctorReport,
  severityLabel,
} from "./doctor-panel.mjs";

function makeFakeDoc() {
  function makeEl(tag) {
    const children = [];
    const el = {
      tagName: tag.toUpperCase(),
      className: "",
      dataset: {},
      _text: "",
      get textContent() {
        return this._text + children.map((c) => c.textContent).join("");
      },
      set textContent(v) {
        this._text = String(v);
        children.length = 0;
      },
      get firstChild() {
        return children[0] || null;
      },
      children,
      hidden: false,
      attrs: {},
      listeners: {},
      appendChild(child) {
        children.push(child);
        return child;
      },
      removeChild(child) {
        const idx = children.indexOf(child);
        if (idx >= 0) children.splice(idx, 1);
      },
      setAttribute(name, value) {
        this.attrs[name] = String(value);
      },
      getAttribute(name) {
        return this.attrs[name];
      },
      addEventListener(type, fn) {
        (this.listeners[type] ||= []).push(fn);
      },
      click() {
        for (const fn of this.listeners.click || []) fn();
      },
    };
    return el;
  }
  return { createElement: makeEl };
}

test("severityLabel and buildDoctorSummaryLine format counts accurately", () => {
  assert.equal(severityLabel("ok"), "正常");
  assert.equal(severityLabel("warn"), "提示");
  assert.equal(severityLabel("error"), "错误");
  const line = buildDoctorSummaryLine({
    version: "1.44.0",
    os: "windows",
    arch: "x86_64",
    overall: "warn",
    ok_count: 5,
    warn_count: 1,
    error_count: 0,
  });
  assert.match(line, /v1\.44\.0/);
  assert.match(line, /总评：提示/);
  assert.match(line, /正常 5 · 提示 1 · 错误 0/);
});

test("renderDoctorReport uses textContent safely without injecting HTML", () => {
  const doc = makeFakeDoc();
  const root = doc.createElement("div");
  renderDoctorReport(
    root,
    {
      version: "1.44.0",
      os: "linux",
      arch: "x86_64",
      workspace: "/tmp/<img src=x onerror=alert(1)>",
      overall: "error",
      ok_count: 1,
      warn_count: 0,
      error_count: 1,
      checks: [
        {
          id: "llm.models_json",
          title: "异构模型路由表",
          severity: "error",
          summary: "<script>bad()</script> 明文密钥",
          detail: "请改用 api_key_env",
        },
      ],
    },
    { doc },
  );

  assert.equal(root.children.length, 3);
  const list = root.children[2];
  assert.equal(list.tagName, "UL");
  assert.equal(list.children.length, 1);
  assert.equal(list.children[0].dataset.checkId, "llm.models_json");
  assert.ok(list.children[0].textContent.includes("<script>bad()</script>"));
  assert.ok(list.children[0].textContent.includes("请改用 api_key_env"));
});

test("doctor version starts as the reported version and reveals the mark only after click", () => {
  const doc = makeFakeDoc();
  const root = doc.createElement("div");
  renderDoctorReport(
    root,
    {
      version: "1.45.1",
      os: "linux",
      arch: "x86_64",
      overall: "ok",
      ok_count: 2,
      warn_count: 0,
      error_count: 0,
      checks: [],
    },
    { doc },
  );
  const header = root.children[0];
  const button = header.children.find((el) => el.tagName === "BUTTON");
  const era = header.children.find((el) => el.className === "doctor-era");
  assert.equal(button.textContent, "v1.45.1");
  assert.equal(button.getAttribute("aria-pressed"), "false");
  assert.equal(era.hidden, true);
  assert.equal(era.textContent, VERSION_MARK_LINE);
  assert.equal(header.textContent.includes(VERSION_MARK), false);
  button.click();
  assert.equal(button.textContent, VERSION_MARK);
  assert.equal(VERSION_MARK, "（1）1.45.1（4）");
  assert.equal(button.getAttribute("aria-pressed"), "true");
  assert.equal(era.hidden, false);
  assert.equal(era.textContent, "好时代，来临罢！");
  assert.match(header.textContent, /总评：正常/);
  button.click();
  assert.equal(button.textContent, "（1）1.45.1（4）");
});

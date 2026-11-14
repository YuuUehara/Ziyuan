import test from 'node:test';
import assert from 'node:assert/strict';
import { parseTextRoute, textRoute, choosePanel, toolPanel, textLocation, mountWorkspace } from './translation-workspace.mjs';

test('book locations read as chapter, paragraph and translation group without changing IDs', () => {
  assert.equal(textLocation('c0001'), '第1章');
  assert.equal(textLocation('c0002-p0023'), '第2章 · 第23段');
  assert.equal(textLocation('c0002-p0023', true), '第23段');
  assert.equal(textLocation('c0001-b0002'), '第1章 · 译稿第2组');
  assert.equal(textLocation('c0001-b0002', true), '译稿第2组');
  assert.equal(textLocation('c10000-p10001'), '第10000章 · 第10001段');
  assert.equal(textLocation('external-id'), 'external-id');
  assert.equal(textLocation(null), '');
});

test('new and saved legacy book links preserve project and block', () => {
  for (const prefix of ['translation', 'wuying']) {
    assert.deepEqual(parseTextRoute(`#${prefix}/text/book/chapter-block`), { project:'book', block:'chapter-block' });
  }
  assert.equal(parseTextRoute('#wuying'), null);
  assert.equal(textRoute('book','chapter-block'),'#translation/text/book/chapter-block');
});
test('new projects lead to import, populated projects lead to writing, user tab survives refresh', () => {
  assert.equal(choosePanel(null, {project:'book', hasSource:false}), 'manage');
  assert.equal(choosePanel(null, {project:'book', hasSource:true}), 'draft');
  assert.equal(choosePanel({project:'book',hasSource:true,panel:'materials'}, {project:'book',hasSource:true}), 'materials');
  assert.equal(choosePanel({project:'book',hasSource:false,panel:'manage'}, {project:'book',hasSource:true}), 'draft');
  assert.equal(choosePanel({project:'old',hasSource:true,panel:'delivery'}, {project:'new',hasSource:true}), 'draft');
});
test('management tools never become the default writing surface', () => {
  assert.equal(toolPanel('backup'),'manage');
  assert.equal(toolPanel('source-update'),'manage');
  assert.equal(toolPanel('reading'),'delivery');
  assert.equal(toolPanel('search'),'draft');
  assert.equal(toolPanel('issues'),'materials');
  assert.equal(toolPanel('document-import'),'manage');
  assert.equal(toolPanel('document-export'),'delivery');
});

test('mountWorkspace places human-mode-bar directly above text-human-editor in draft panel', () => {
  globalThis.MutationObserver = class { observe() {} };
  const makeNode = (id, cls = '') => {
    const node = {
      id, className: cls, dataset: {}, attributes: {}, children: [], hidden: false, parentElement: null,
      ownerDocument: null,
      setAttribute(k, v) { this.attributes[k] = v; },
      addEventListener() {},
      append(...items) {
        for (const item of items) {
          if (item.parentElement && item.parentElement !== this) {
            item.parentElement.children = item.parentElement.children.filter((c) => c !== item);
          }
          item.parentElement = this;
          this.children.push(item);
        }
      },
      insertBefore(item, ref) {
        this.append(item);
        if (ref) {
          this.children = this.children.filter((c) => c !== item);
          const idx = this.children.indexOf(ref);
          this.children.splice(idx >= 0 ? idx : this.children.length, 0, item);
        }
      },
      querySelector(sel) {
        if (sel === '.text-main') return main;
        if (sel === '#text-status') return status;
        return null;
      },
      querySelectorAll() { return []; },
    };
    return node;
  };
  const doc = { createElement: (tag) => makeNode(tag) };
  const main = makeNode('main'); main.ownerDocument = doc;
  const status = makeNode('text-status'); status.ownerDocument = doc;
  const bar = makeNode('human-mode-bar'); bar.ownerDocument = doc;
  const editor = makeNode('text-human-editor'); editor.ownerDocument = doc;
  main.append(status);
  const card = {
    ownerDocument: doc,
    querySelector(sel) {
      return ({ '.text-main': main, '#human-mode-bar': bar, '#text-human-editor': editor })[sel] || null;
    },
    querySelectorAll() { return []; },
  };
  mountWorkspace(card);
  const draftPanel = main.children.find((c) => c.dataset.workspacePanel === 'draft');
  const idxBar = draftPanel.children.indexOf(bar);
  const idxEditor = draftPanel.children.indexOf(editor);
  assert.ok(idxBar >= 0 && idxEditor === idxBar + 1, 'human-mode-bar must precede text-human-editor');
});

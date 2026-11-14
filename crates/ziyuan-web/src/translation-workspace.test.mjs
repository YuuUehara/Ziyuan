import test from 'node:test';
import assert from 'node:assert/strict';
import { parseTextRoute, textRoute, choosePanel, toolPanel, textLocation } from './translation-workspace.mjs';

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

export function parseTextRoute(hash) {
  const parts = hash.replace(/^#/, '').split('/');
  if (!['translation', 'wuying'].includes(parts[0]) || parts[1] !== 'text') return null;
  return { project: parts[2] || null, block: parts[3] || null };
}
export function textRoute(project, block) {
  return '#translation/text' + (project ? '/' + project : '') + (block ? '/' + block : '');
}
export function choosePanel(previous, next) {
  if (!previous || previous.project !== next.project || previous.hasSource !== next.hasSource) return next.hasSource ? 'draft' : 'manage';
  return previous.panel;
}
export function toolPanel(tool) {
  return ({backup:'manage', 'source-update':'manage', issues:'materials', reading:'delivery', search:'draft', tmx:'materials', 'document-import':'manage', 'document-export':'delivery', documents:'delivery'})[tool] || 'manage';
}

const definitions = [
  ['draft', '译稿', '在左侧选章节，直接写人工译稿；需要模型初译时，使用下方译书操作区。'],
  ['materials', '资料', '集中维护人物、术语、参考译法与跨章待查。保存资料后仍须按原流程单独批准。'],
  ['delivery', '通读交稿', '先通读和预检，再导出定稿；取回原件也在这里。'],
  ['manage', '项目管理', '新项目先导入原文；原文更新、版本记录、备份与恢复也收在这里。'],
];

export function mountWorkspace(card) {
  const main = card.querySelector('.text-main');
  const tabs = card.ownerDocument.createElement('div');
  tabs.className = 'workspace-tabs'; tabs.setAttribute('role', 'tablist'); tabs.setAttribute('aria-label', '译书工作台');
  const panels = new Map(); const buttons = new Map(); let current = null;
  for (const [key, title, hint] of definitions) {
    const button = card.ownerDocument.createElement('button');
    button.type = 'button'; button.textContent = title; button.dataset.workspaceTab = key; button.dataset.view = '';
    button.id = `workspace-tab-${key}`; button.setAttribute('role', 'tab'); button.setAttribute('aria-controls', `workspace-panel-${key}`);
    const panel = card.ownerDocument.createElement('section');
    panel.id = `workspace-panel-${key}`; panel.dataset.workspacePanel = key;
    panel.setAttribute('role', 'tabpanel'); panel.setAttribute('aria-labelledby', button.id);
    const description = card.ownerDocument.createElement('p'); description.className = 'hint'; description.textContent = hint; panel.append(description);
    tabs.append(button); main.append(panel); panels.set(key, panel); buttons.set(key, button);
    button.addEventListener('click', () => select(key));
    button.addEventListener('keydown', (event) => {
      const keys = definitions.map(([id]) => id), index = keys.indexOf(key);
      const next = event.key === 'ArrowRight' ? keys[(index + 1) % keys.length] : event.key === 'ArrowLeft' ? keys[(index + keys.length - 1) % keys.length] : event.key === 'Home' ? keys[0] : event.key === 'End' ? keys.at(-1) : null;
      if (next) { event.preventDefault(); select(next); buttons.get(next).focus(); }
    });
  }
  main.prepend(tabs);
  function select(key) {
    if (!panels.has(key)) return;
    if (current) current.panel = key;
    for (const [id, panel] of panels) {
      panel.hidden = id !== key;
      buttons.get(id).setAttribute('aria-selected', String(id === key));
      buttons.get(id).tabIndex = id === key ? 0 : -1;
    }
  }
  const move = (node, panel) => { if (node && node.parentElement !== panels.get(panel)) panels.get(panel).append(node); };
  function arrange() {
    // Existing editor hosts are moved intact; switching tabs never reconstructs them.
    for (const [selector, panel] of [
      ['#text-actions','draft'], ['#text-action-result','draft'], ['#text-human-editor','draft'], ['.text-compare','draft'], ['#text-materials','draft'], ['#verification-panel','draft'], ['#text-n2b-hints','draft'],
      ['#text-materials-panel','materials'], ['#text-memory','materials'], ['.text-notes-layout','materials'],
      ['#text-export','delivery'], ['#text-import','manage'], ['#text-versions','manage'],
    ]) move(card.querySelector(selector), panel);
    for (const tool of card.querySelectorAll('[data-workspace-tool]')) {
      const kind = tool.dataset.workspaceTool, panel = panels.get(toolPanel(kind));
      const anchor = kind === 'document-import' ? card.querySelector('#text-versions') : kind === 'reading' ? card.querySelector('#text-export') : null;
      if (anchor?.parentElement === panel) {
        if (tool.parentElement !== panel || tool.nextElementSibling !== anchor) panel.insertBefore(tool, anchor);
      } else move(tool, toolPanel(kind));
    }
  }
  const observer = new MutationObserver(arrange);
  observer.observe(card, { childList:true, subtree:true });
  arrange(); select('manage');
  return {
    select,
    update(next) { const panel = choosePanel(current, next); current = {...next, panel}; select(panel); arrange(); },
  };
}

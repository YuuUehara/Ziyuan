const scopes={all:'全部',source:'原文',translation:'当前译文',draft:'人工草稿',history:'历史译稿'};
function node(tag,text,className){const e=document.createElement(tag);e.textContent=text;if(className)e.className=className;return e;}
export function replacementReady(preview){return !!preview&&!preview.blocked&&Number(preview.occurrences)>0;}
export function mountSearch(host,{api,getSelection,navigate,refresh}){
 const section=document.createElement('details');section.className='continuity-tools';section.id='continuity-search';
 section.innerHTML=`<summary>全文检索与草稿替换</summary><p class="hint">可在原文、当前译文、人工草稿和历史译稿中查找相同文字。草稿替换先预览，再确认保存；准奏稿保持原样，送审与准奏仍须另行操作。</p><div class="row"><input data-query aria-label="检索文字" placeholder="输入要查找的文字"><select data-scope aria-label="检索范围"><option value="all">全部</option><option value="source">原文</option><option value="translation">当前译文</option><option value="draft">人工草稿</option><option value="history">历史译稿</option></select><button type="button" data-search>查找</button></div><p data-status role="status"></p><div data-results></div>`;
 host.after(section);const el=k=>section.querySelector(`[data-${k}]`);let busy=false;
 const run=async task=>{if(busy)return;busy=true;try{el('status').textContent='处理中…';await task();}catch(e){el('status').textContent=e.message;}finally{busy=false;}};
 el('search').onclick=()=>run(async()=>{
  const project=getSelection().project;if(!project)throw new Error('请先选择项目');const query=el('query').value;if(!query.trim())throw new Error('请输入检索文字');el('results').replaceChildren();
  const result=await api(`/text/${encodeURIComponent(project)}/search?query=${encodeURIComponent(query)}&scope=${encodeURIComponent(el('scope').value)}`);if(project!==getSelection().project)throw new Error('项目已变，请重新查找');
  el('status').textContent=`找到 ${result.hits.length} 处${result.truncated?'；结果较多，请缩小检索范围':''}`;
  for(const hit of result.hits){
   const item=node('article','','continuity-change');item.append(node('h4',`${scopes[hit.kind]||hit.kind} · ${hit.chapter}${hit.version?' · '+hit.version:''}`),node('p',hit.text));
   const locate=node('button','回到本段编辑','small');locate.type='button';locate.onclick=()=>run(async()=>{if(project!==getSelection().project)throw new Error('项目已变，请重新查找');await navigate(hit);el('status').textContent='已定位';});item.append(locate);
   if(hit.kind==='draft'){
    const controls=node('div','','row write'),replacement=document.createElement('input');replacement.placeholder='替换为';replacement.setAttribute('aria-label','替换文字');
    const previewButton=node('button','预览本段替换'),confirm=node('button','确认保存草稿');previewButton.type=confirm.type='button';confirm.disabled=true;const change=node('div','','continuity-result');let proposed;
    replacement.oninput=()=>{proposed=null;confirm.disabled=true;};
    previewButton.onclick=()=>run(async()=>{if(project!==getSelection().project)throw new Error('项目已变，请重新查找');proposed=null;confirm.disabled=true;const input={block:hit.block,paragraph:hit.paragraph,from:query,to:replacement.value};const preview=await api(`/text/${encodeURIComponent(project)}/draft-replace`,input);if(project!==getSelection().project)throw new Error('项目已变，请重新查找');proposed={input,preview};change.replaceChildren(node('p',`本段命中 ${preview.occurrences} 处`),node('p','替换前','hint'),node('p',preview.before),node('p','替换后','hint'),node('p',preview.after));if(preview.blocked)change.append(node('p',typeof preview.blocked==='string'?preview.blocked:'要替换的文字带有注音或样式标记，请回编辑器逐字核对','text-problems'));confirm.disabled=!replacementReady(preview);el('status').textContent='请核对替换前后内容';});
    confirm.onclick=()=>run(async()=>{if(!proposed||project!==getSelection().project)throw new Error('请重新预览');await api(`/text/${encodeURIComponent(project)}/draft-replace`,{...proposed.input,fingerprint:proposed.preview.fingerprint,confirm:true});proposed=null;confirm.disabled=true;el('status').textContent='人工草稿已保存，仍须送审、准奏后才能用于定稿';if(refresh)await refresh();});controls.append(replacement,previewButton,confirm);item.append(controls,change);
   }el('results').append(item);
  }
 });return section;
}

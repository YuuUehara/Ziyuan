import { textLocation } from "./translation-workspace.mjs";
const labels={pending:"待查",confirmed:"确认问题",intentional:"有意异译",resolved:"已解决"};
export function mountIssues(host,{api,getSelection,navigate}) {
  const section=document.createElement("details");section.className="continuity-tools";
  section.innerHTML=`<summary>跨章待查</summary><p class="hint">把需要核对的问题关联到原译文，记下判断理由。相关文字或资料变化后，请重新核对原来的处理结果。登记或解决问题只更新待查记录，改稿与准奏仍须另行操作。</p><button type="button" data-load>载入待查与段落</button><div class="write"><label>当前段<select data-source aria-label="当前段"></select></label><label>关联前后文（可多选）<select data-related multiple size="4" aria-label="关联段落"></select></label><label>核对事项<input data-rule placeholder="如：人物称呼、术语或情节疑点"></label><label>问题说明<textarea data-body rows="3"></textarea></label><button type="button" data-add>登记待查</button></div><p data-status role="status"></p><div data-list></div>`;
  host.after(section);const el=(key)=>section.querySelector(`[data-${key}]`);
  let loaded,project,busy=false;
  const run=async(task)=>{if(busy)return;busy=true;try{el("status").textContent="处理中…";await task();el("status").textContent="完成";}catch(e){el("status").textContent=e.message;}finally{busy=false;}};
  const check=()=>{if(!loaded||project!==getSelection().project)throw new Error("请先载入当前项目的待查记录");};
  const option=(anchor)=>{const o=document.createElement("option");o.value=anchor.paragraph;o.textContent=`${anchor.title} · ${textLocation(anchor.paragraph, true)} · ${anchor.source.slice(0,50)}`;return o;};
  const load=async()=>{
    loaded=null;project=null;el("list").replaceChildren();
    const selected=getSelection();if(!selected.project)throw new Error("请先选择项目");
    const result=await api(`/text/${encodeURIComponent(selected.project)}/issues`);
    if(getSelection().project!==selected.project)throw new Error("项目已变，请重新读取");
    loaded=result;project=selected.project;
    for(const key of ["source","related"]){el(key).replaceChildren(...loaded.anchors.map(option));}
    const selectedAnchor=loaded.anchors.find(a=>a.paragraph===selected.paragraph)||loaded.anchors.find(a=>a.block===selected.block);
    if(selectedAnchor)el("source").value=selectedAnchor.paragraph;
    el("related").selectedIndex=-1;
    const list=el("list");list.replaceChildren();
    for(const view of loaded.issues){
      const item=document.createElement("article");item.className="continuity-change";
      const title=document.createElement("h4");title.textContent=`${view.current?labels[view.record.state]:"需重新核对"} · ${view.record.input.rule}`;
      const body=document.createElement("p");body.textContent=view.record.input.body;item.append(title,body);
      for(const anchor of view.anchors){const button=document.createElement("button");button.type="button";button.className="small";button.textContent=`定位：${anchor.title} · ${anchor.source.slice(0,30)}`;button.onclick=()=>run(async()=>{check();await navigate(anchor);});item.append(button);}
      const reason=document.createElement("textarea");reason.rows=2;reason.placeholder="本次复核理由";reason.setAttribute("aria-label","本次复核理由");reason.className="write";
      const controls=document.createElement("div");controls.className="row write";
      const states=view.current?Object.keys(labels):["pending"];
      for(const state of states){const button=document.createElement("button");button.type="button";button.textContent=view.current?labels[state]:"重新核对并转待查";button.onclick=()=>run(async()=>{check();await api(`/text/${encodeURIComponent(project)}/issues/${view.record.issue_id}/review`,{version:view.record.version,fingerprint:view.fingerprint,state,reason:reason.value,recheck:!view.current});await load();});controls.append(button);}
      const history=document.createElement("details");const summary=document.createElement("summary");summary.textContent=`历史记录（${view.history.length}次）`;
      history.append(summary);for(const record of view.history){const entry=document.createElement("article");entry.className="continuity-history";const heading=document.createElement("h4");heading.textContent=labels[record.state];const reason=document.createElement("p");reason.textContent=record.reason||"登记待查";entry.append(heading,reason);for(const anchor of record.anchors){const evidence=document.createElement("p");evidence.className="hint";evidence.textContent=anchor.source+" → "+(anchor.translation??"尚无译稿");entry.append(evidence);}history.append(entry);}
      item.append(reason,controls,history);list.append(item);
    }
    if(!loaded.issues.length)list.textContent="尚未登记待查问题，通读时仍请留意前后文。";
  };
  el("load").onclick=()=>run(load);
  el("add").onclick=()=>run(async()=>{check();const input={paragraph:el("source").value,related:[...el("related").selectedOptions].map(o=>o.value).filter(p=>p!==el("source").value),rule:el("rule").value,body:el("body").value};await api(`/text/${encodeURIComponent(project)}/issues`,{input,fingerprint:loaded.fingerprint});el("body").value="";await load();});
  return section;
}

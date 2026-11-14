import { paragraphFromWire } from "./editor-core.mjs";
import { previewHtml } from "./human-editor.mjs";
export function mountReading(host,{api,getSelection,getChapters,navigate}) {
  const section=document.createElement("details");section.className="continuity-tools";section.id="continuity-reader";
  section.innerHTML=`<summary>整章通读与交稿预检</summary><p class="hint">通读显示已准奏或已定稿的正式译稿（仅保存人工草稿而未送审准奏时不会显示在此）。交稿前可按全书或单章、按 TXT、Markdown 或 HTML 检查缺稿、定稿状态和标记能否保留，并查看尚待核对的问题。预检通过后仍须按原流程导出。</p><div class="row"><button type="button" data-load>载入通读</button><select data-chapter aria-label="通读章节"><option value="">全书</option></select><select data-format aria-label="预检导出格式"><option value="txt">TXT</option><option value="markdown">Markdown</option><option value="html">HTML（保留文学标记）</option></select><button type="button" data-check>交稿预检</button></div><p data-status role="status"></p><div data-report></div><div data-pages></div>`;
  host.after(section);const el=(key)=>section.querySelector(`[data-${key}]`);let reading,project;
  const syncChapters=()=>{
    const requested=getSelection().project;
    const select=el("chapter");
    const current=select.value;
    const list=reading&&project===requested
      ? [...new Map(reading.paragraphs.map(p=>[p.chapter,p.title])).entries()]
      : (getChapters?.()||[]).map(c=>[c.id,c.title]);
    select.replaceChildren(new Option("全书",""));
    for(const [id,title] of list) select.append(new Option(title,id));
    select.value=list.some(([id])=>id===current)?current:"";
  };
  section.addEventListener("toggle",()=>{ if(section.open) syncChapters(); });
  const run=async(task)=>{try{el("status").textContent="处理中…";await task();}catch(e){el("status").textContent=e.message;}};
  const render=()=>{
    if(!reading||project!==getSelection().project){el("pages").replaceChildren();return;}
    const pages=el("pages");pages.replaceChildren();let chapter;
    for(const paragraph of reading.paragraphs){
      if(el("chapter").value&&paragraph.chapter!==el("chapter").value)continue;
      if(chapter!==paragraph.chapter){chapter=paragraph.chapter;const title=document.createElement("h3");title.textContent=paragraph.title;pages.append(title);}
      const row=document.createElement("div");row.className="reading-row";
      const source=document.createElement("p");source.textContent=paragraph.source;
      const target=document.createElement("div");const text=document.createElement("p");
      if(paragraph.translation===null)text.textContent="尚无已准奏译稿（仅保存人工草稿不会显示在此），可回到本段编辑";
      else text.innerHTML=previewHtml(paragraphFromWire({id:paragraph.paragraph,text:paragraph.translation,marks:paragraph.marks}));
      const link=document.createElement("button");link.type="button";link.className="small";link.textContent="回到本段编辑";link.onclick=()=>run(async()=>{if(project!==getSelection().project)throw new Error("项目已变，请重新读取");await navigate(paragraph);});
      target.append(text,link);row.append(source,target);pages.append(row);
    }
    el("status").textContent=reading.warnings.join("；")||"通读材料已载入";
  };
  el("load").onclick=()=>run(async()=>{
    reading=null;el("pages").replaceChildren();const requested=getSelection().project;if(!requested)throw new Error("请先选择项目");
    const result=await api(`/text/${encodeURIComponent(requested)}/reading`);if(requested!==getSelection().project)throw new Error("项目已变，请重新读取");reading=result;project=requested;
    syncChapters();render();
  });
  el("chapter").onchange=render;
  el("check").onclick=()=>run(async()=>{
    const requested=getSelection().project;if(!requested)throw new Error("请先选择项目");
    syncChapters();
    const chapter=el("chapter").value;
    const report=await api(`/text/${encodeURIComponent(requested)}/delivery?format=${encodeURIComponent(el("format").value)}${chapter?`&chapter=${encodeURIComponent(chapter)}`:""}`);
    if(requested!==getSelection().project)throw new Error("项目已变，请重新预检");el("report").replaceChildren();
    const result=document.createElement("p");result.textContent=report.export_ready?`成稿检查通过；另有 ${report.open_issues} 项待查尚未处理或需要重新核对。实际导出时会再次检查。`:"暂不能导出成稿";el("report").append(result);
    for(const message of [...report.blockers,...report.warnings]){const p=document.createElement("p");p.textContent=message;el("report").append(p);}el("status").textContent="预检完成";
  });
  return section;
}

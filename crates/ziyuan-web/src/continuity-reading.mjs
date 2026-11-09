import { paragraphFromWire } from "./editor-core.mjs";
import { previewHtml } from "./human-editor.mjs";
export function mountReading(host,{api,getSelection,navigate}) {
  const section=document.createElement("details");section.className="continuity-tools";section.id="continuity-reader";
  section.innerHTML=`<summary>整章通读与交稿预检</summary><p class="hint">通读显示磁盘现行译稿；批准与定稿以预检为准。预检复用 TXT／Markdown／HTML 成稿导出规则，列出缺稿、失效、标记损失与未处置疑点，不改变审批。</p><div class="row"><button type="button" data-load>读取通读</button><select data-chapter aria-label="通读章节"><option value="">全书</option></select><select data-format aria-label="预检导出格式"><option value="txt">TXT</option><option value="markdown">Markdown</option><option value="html">HTML（保留文学标记）</option></select><button type="button" data-check>交稿预检</button></div><p data-status role="status"></p><div data-report></div><div data-pages></div>`;
  host.after(section);const el=(key)=>section.querySelector(`[data-${key}]`);let reading,project;
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
      if(paragraph.translation===null)text.textContent="尚无结构有效译稿";
      else text.innerHTML=previewHtml(paragraphFromWire({id:paragraph.paragraph,text:paragraph.translation,marks:paragraph.marks}));
      const link=document.createElement("button");link.type="button";link.className="small";link.textContent="定位到编辑段";link.onclick=()=>run(async()=>{if(project!==getSelection().project)throw new Error("项目已变，请重新读取");await navigate(paragraph);});
      target.append(text,link);row.append(source,target);pages.append(row);
    }
    el("status").textContent=reading.warnings.join("；")||"通读材料已载入";
  };
  el("load").onclick=()=>run(async()=>{
    reading=null;el("pages").replaceChildren();const requested=getSelection().project;if(!requested)throw new Error("请先选择项目");
    const result=await api(`/text/${encodeURIComponent(requested)}/reading`);if(requested!==getSelection().project)throw new Error("项目已变，请重新读取");reading=result;project=requested;
    el("chapter").replaceChildren(new Option("全书",""));const chapters=new Map(result.paragraphs.map(p=>[p.chapter,p.title]));for(const [id,title]of chapters)el("chapter").append(new Option(title,id));render();
  });
  el("chapter").onchange=render;
  el("check").onclick=()=>run(async()=>{
    const requested=getSelection().project;if(!requested)throw new Error("请先选择项目");const chapter=reading&&project===requested?el("chapter").value:"";
    const report=await api(`/text/${encodeURIComponent(requested)}/delivery?format=${encodeURIComponent(el("format").value)}${chapter?`&chapter=${encodeURIComponent(chapter)}`:""}`);
    if(requested!==getSelection().project)throw new Error("项目已变，请重新预检");el("report").replaceChildren();
    const result=document.createElement("p");result.textContent=report.export_ready?`成稿资格检查通过；未处置或过期待查 ${report.open_issues} 项。实际导出仍会重新检查。`:"暂不能导出成稿";el("report").append(result);
    for(const message of [...report.blockers,...report.warnings]){const p=document.createElement("p");p.textContent=message;el("report").append(p);}el("status").textContent="预检完成";
  });
  return section;
}

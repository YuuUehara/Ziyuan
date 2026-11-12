import test from "node:test";
import assert from "node:assert/strict";
import { mountIssues } from "./continuity-issues.mjs";
class Element {
  constructor(){this.elements=new Map();this.style={};this.selectedOptions=[];this.value="";}
  querySelector(key){if(!this.elements.has(key))this.elements.set(key,new Element());return this.elements.get(key);}
  after(){} replaceChildren(){} append(){} setAttribute(){}
}
test("failed project switch clears old issue state before any mutation",async()=>{
  const original=globalThis.document;globalThis.document={createElement:()=>new Element()};
  try{
    let project="A",writes=0;
    const api=async(path,body)=>{if(body!==undefined){writes++;return{};}if(path.includes("/B/"))throw new Error("读取失败");return{anchors:[],issues:[],fingerprint:"old"};};
    const section=mountIssues(new Element(),{api,getSelection:()=>({project}),navigate:async()=>{}});
    await section.querySelector("[data-load]").onclick();project="B";
    await section.querySelector("[data-load]").onclick();
    await section.querySelector("[data-add]").onclick();
    assert.equal(writes,0);assert.match(section.querySelector("[data-status]").textContent,/载入当前项目的待查记录/);
  }finally{globalThis.document=original;}
});

import test from 'node:test';
import assert from 'node:assert/strict';
import { mountSearch,replacementReady } from './continuity-search.mjs';
import { backupSummary, decodeTextBytes, sourceUpdateRequest } from './continuity.mjs';
class Element {
 constructor(tag='div'){this.tagName=tag;this.children=[];this.elements=new Map();this.value='';this.textContent='';}
 querySelector(key){if(!this.elements.has(key))this.elements.set(key,new Element());return this.elements.get(key);}
 append(...children){this.children.push(...children);} replaceChildren(...children){this.children=children;} after(){} setAttribute(){}
}
function find(node,predicate){if(predicate(node))return node;for(const child of node.children){if(typeof child==='object'){const hit=find(child,predicate);if(hit)return hit;}}}
test('replacement confirmation requires real unblocked occurrences',()=>{
 assert.equal(replacementReady(null),false);assert.equal(replacementReady({occurrences:0}),false);assert.equal(replacementReady({occurrences:2,blocked:'标记冲突'}),false);assert.equal(replacementReady({occurrences:2}),true);
});
test('backup summary presents counts without leaking raw metadata',()=>{assert.deepEqual(backupSummary({manifest:{projects:['a'],files:[{bytes:20},{bytes:30}]}}),{projects:1,files:2,bytes:50});});
test('source update keeps text formats as UTF-8 and binaries as package bytes',()=>{
 const text=sourceUpdateRequest('chapter.md',new TextEncoder().encode('朝。\n'),null);
 assert.equal(text.text,'朝。\n'); assert.equal(text.package_base64,undefined);
 const binary=sourceUpdateRequest('book.docx',Uint8Array.from([0,255,10]),'c0001');
 assert.equal(binary.text,''); assert.equal(binary.chapter,'c0001');
 assert.equal(Buffer.from(binary.package_base64,'base64').toString('hex'),'00ff0a');
});
test('old project search cannot confirm a draft replacement after project switch',async()=>{
 const previous=globalThis.document;globalThis.document={createElement:tag=>new Element(tag)};
 try{
  let project='A',writes=0;
  const api=async(path,body)=>{if(body){writes++;return{fingerprint:'f',before:'旧译',after:'新译',occurrences:1};}return{hits:[{chapter:'c0001',block:'c0001-b0001',paragraph:'c0001-p0001',kind:'draft',text:'旧译'}],truncated:false};};
  const section=mountSearch(new Element(),{api,getSelection:()=>({project}),navigate:async()=>{}});section.querySelector('[data-query]').value='旧';section.querySelector('[data-scope]').value='draft';
  await section.querySelector('[data-search]').onclick();const results=section.querySelector('[data-results]');const replacement=find(results,e=>e.placeholder==='替换为');replacement.value='新';const preview=find(results,e=>e.textContent==='预览本段替换');await preview.onclick();const confirm=find(results,e=>e.textContent==='确认保存草稿');assert.equal(confirm.disabled,false);
  project='B';await confirm.onclick();assert.equal(writes,1);assert.match(section.querySelector('[data-status]').textContent,/重新预览/);
 }finally{globalThis.document=previous;}
});
test('source update and decodeTextBytes support Shift_JIS, ISO-2022-JP, EUC-JP and reject invalid bytes',()=>{
 const sjis=sourceUpdateRequest('aozora.txt',Uint8Array.from([0x90,0xd4,0x8c,0xbb]),null);
 assert.equal(sjis.text,'赤現');
 assert.equal(decodeTextBytes(Uint8Array.from([0x90,0xd4,0x8c,0xbb]),'aozora.txt').encoding,'Shift_JIS');
 const iso=decodeTextBytes(Uint8Array.from([0x1b,0x24,0x42,0x40,0x56,0x38,0x73,0x1b,0x28,0x42]),'jis.txt');
 assert.equal(iso.encoding,'ISO-2022-JP');
 assert.equal(iso.text,'赤佼');
 // EUC-JP hiragana bytes that are also valid Shift_JIS half-width katakana must decode as EUC-JP.
 const eucBytes=Uint8Array.from([0xa4,0xa2,0xa4,0xa4,0xa4,0xa6,0xa4,0xa8,0xa4,0xaa]);
 const euc=decodeTextBytes(eucBytes,'euc.txt');
 assert.equal(euc.encoding,'EUC-JP');
 assert.equal(euc.text,'あいうえお');
 assert.throws(()=>sourceUpdateRequest('broken.txt',Uint8Array.from([0xff,0xfe]),null),/UTF-8.*JIS/);
 assert.throws(()=>decodeTextBytes(Uint8Array.from([0xef,0xbb,0xbf,0x82,0xa0]),'bom.txt'),/UTF-8.*JIS/);
});
test("pressing Enter (form submit) runs search without navigating", async () => {
  const previous = globalThis.document;
  globalThis.document = { createElement: (tag) => new Element(tag) };
  try {
    let searches = 0;
    const api = async () => { searches++; return { hits: [], truncated: false }; };
    const section = mountSearch(new Element(), { api, getSelection: () => ({ project: "P" }), navigate: async () => {} });
    section.querySelector("[data-query]").value = "词语";
    let prevented = false;
    await section.querySelector("[data-form]").onsubmit({ preventDefault() { prevented = true; } });
    assert.equal(prevented, true);
    assert.equal(searches, 1);
  } finally {
    globalThis.document = previous;
  }
});

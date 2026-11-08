#!/usr/bin/env node
// Local OpenAI-compatible mock: first turn reads the approved context, second turn
// asserts the tool result contains v2 data and finishes. No network, no paid model.
import http from "node:http";
import fs from "node:fs";

const args = new Map();
for (let i = 2; i < process.argv.length; i += 2) args.set(process.argv[i], process.argv[i + 1]);
const port = Number(args.get("--port") || 18999);
const log = args.get("--log") || "mock-llm-requests.jsonl";
const assertFile = args.get("--assert") || "mock-llm-assert.json";
const contextPath = args.get("--context") || "";
const sourcePath = args.get("--source") || "";

const sse = (events) => events.map((e) => "data: " + JSON.stringify(e) + "\n\n").join("") + "data: [DONE]\n\n";
const chunk = (delta, finish = null) => ({
  id: "chatcmpl-mock", object: "chat.completion.chunk", created: 0, model: "mock",
  choices: [{ index: 0, delta, finish_reason: finish }],
});
let turn = 0;

const server = http.createServer((req, res) => {
  if (req.method !== "POST") { res.writeHead(404).end("{}"); return; }
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    let parsed = {};
    try { parsed = JSON.parse(body); } catch {}
    fs.appendFileSync(log, body + "\n");
    turn += 1;
    const messages = parsed.messages || [];
    const toolText = messages.filter((m) => m.role === "tool").map((m) => String(m.content || "")).join("\n");
    if (turn === 1) {
      const call = {
        id: "call_ctx_1", type: "function",
        function: { name: "read_file", arguments: JSON.stringify({ path: contextPath }) },
      };
      const events = [
        chunk({ role: "assistant", tool_calls: [{ index: 0, ...call }] }),
        chunk({}, "tool_calls"),
      ];
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      res.end(sse(events));
      return;
    }
    if (turn === 2) {
      const sawV2 = toolText.includes('"format": 2') || toolText.includes('"format":2');
      if (sawV2) fs.writeFileSync(assertFile, JSON.stringify({ ok: true, sawV2, toolTextLength: toolText.length, turns: turn }, null, 2));
      const dir = contextPath.replace(/context\.json$/, "");
      let paragraphs = [];
      try {
        const source = JSON.parse(fs.readFileSync(sourcePath || (dir ? dir + "/" : "") + "source.json", "utf8"));
        paragraphs = (source.paragraphs || []).map((p) => ({ id: p.id, text: "夜明。译文。" }));
      } catch {}
      const zh = JSON.stringify({ schema_version: 1, block_id: "c0001-b0001", paragraphs });
      const call = {
        id: "call_write_1", type: "function",
        function: { name: "write_file", arguments: JSON.stringify({ path: (dir ? dir + "/" : "") + "zh.json", content: zh }) },
      };
      const events = [
        chunk({ role: "assistant", tool_calls: [{ index: 0, ...call }] }),
        chunk({}, "tool_calls"),
      ];
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      res.end(sse(events));
      return;
    }
    const sawV2 = toolText.includes('"format": 2') || toolText.includes('"format":2');
    if (sawV2) {
      fs.writeFileSync(assertFile, JSON.stringify({ ok: true, sawV2, toolTextLength: toolText.length, turns: turn }, null, 2));
    } else if (!fs.existsSync(assertFile)) {
      fs.writeFileSync(assertFile, JSON.stringify({ ok: false, sawV2, toolTextLength: toolText.length, turns: turn }, null, 2));
    }
    const events = [
      chunk({ role: "assistant", content: "结论：已完成初译。\n依据：依 source.json 与 context.json，逐段保留 id。\n未办之事：无。" }),
      chunk({}, "stop"),
    ];
    res.writeHead(200, { "Content-Type": "text/event-stream" });
    res.end(sse(events));
  });
});
server.listen(port, "127.0.0.1", () => console.error("mock-llm listening", port));

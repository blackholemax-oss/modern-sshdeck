// 通过 Chrome DevTools Protocol 控制真实 Tauri 应用的 WebView2。
// 用法：node scripts/cdp.mjs <js表达式文件>
import fs from "node:fs";

const pageId = await (async () => {
  const res = await fetch("http://127.0.0.1:9222/json");
  const pages = await res.json();
  return pages.find((p) => p.type === "page")?.id;
})();

if (!pageId) throw new Error("no page found on 9222");

const wsUrl = `ws://127.0.0.1:9222/devtools/page/${pageId}`;

// Node 内置 WebSocket（Node 22+）
const ws = new WebSocket(wsUrl);

const pending = new Map();
let nextId = 1;

function send(method, params = {}) {
  const id = nextId++;
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });
}

ws.addEventListener("message", (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id && pending.has(msg.id)) {
    const { resolve, reject } = pending.get(msg.id);
    pending.delete(msg.id);
    if (msg.error) reject(new Error(JSON.stringify(msg.error)));
    else resolve(msg.result);
  }
});

await new Promise((resolve, reject) => {
  ws.addEventListener("open", resolve, { once: true });
  ws.addEventListener("error", reject, { once: true });
});

const script = fs.readFileSync(process.argv[2], "utf8");

const result = await send("Runtime.evaluate", {
  expression: `(async () => { ${script} })()`,
  awaitPromise: true,
  returnByValue: true,
});

if (result.exceptionDetails) {
  console.error("EVAL ERROR:", JSON.stringify(result.exceptionDetails, null, 2));
  process.exit(1);
}

console.log(JSON.stringify(result.result?.value ?? result.result, null, 2));
ws.close();

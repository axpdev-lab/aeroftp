// Read-only page probe over the WebKit remote inspector.
// Usage: node inspect-page.mjs [host:port] [timeoutMs]
const hostport = process.argv[2] || '127.0.0.1:9222';
const timeoutMs = Number(process.argv[3] || 15000);
const EXPR = `JSON.stringify({
  href: location.href, readyState: document.readyState, visibility: document.visibilityState,
  now: Math.round(performance.now()), tauri: typeof window.__TAURI_INTERNALS__,
  root: (document.getElementById('root') || {childElementCount: -1}).childElementCount,
  sheets: [...document.styleSheets].map(s => { let n = -1; try { n = s.cssRules.length } catch (e) { n = 'x:' + e.name } return (s.href || 'inline') + ' rules=' + n }),
  links: [...document.querySelectorAll('link')].map(l => l.rel + ' ' + l.href + (l.sheet ? ' sheet' : ' nosheet')),
  resources: performance.getEntriesByType('resource').map(e => ({ n: e.name.slice(0, 90), start: Math.round(e.startTime), end: Math.round(e.responseEnd), dur: Math.round(e.duration), size: e.transferSize, init: e.initiatorType })),
  nav: performance.getEntriesByType('navigation').map(e => ({ resEnd: Math.round(e.responseEnd), domInteractive: Math.round(e.domInteractive), dcl: Math.round(e.domContentLoadedEventEnd), load: Math.round(e.loadEventEnd) }))
})`;
const res = await fetch('http://' + hostport + '/').then(r => r.text()).catch(e => 'ERR ' + e);
const socks = [...res.matchAll(/\/socket\/(\d+)\/(\d+)\/WebPage/g)].map(m => m[0]);
const urls = [...res.matchAll(/class="targeturl">([^<]*)</g)].map(m => m[1]);
console.log(JSON.stringify({ targets: urls, sockets: socks }));
for (const path of [...new Set(socks)]) {
  const out = await new Promise((resolve) => {
    const ws = new WebSocket('ws://' + hostport + path);
    const console_ = [];
    let tid = null, n = 0;
    const done = (v) => { try { ws.close() } catch {} resolve(v) };
    const t = setTimeout(() => done({ path, timeout: true, console: console_ }), timeoutMs);
    const send = (m) => ws.send(JSON.stringify({ id: ++n, method: 'Target.sendMessageToTarget', params: { targetId: tid, message: JSON.stringify(m) } }));
    ws.onmessage = (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.method === 'Target.targetCreated' && !tid) {
        tid = msg.params.targetInfo.targetId;
        send({ id: 1, method: 'Console.enable' });
        setTimeout(() => send({ id: 2, method: 'Runtime.evaluate', params: { expression: EXPR, returnByValue: true } }), 1500);
      } else if (msg.method === 'Target.dispatchMessageFromTarget') {
        const inner = JSON.parse(msg.params.message);
        if (inner.method === 'Console.messageAdded') {
          const m = inner.params.message; console_.push(`${m.level} ${m.source} ${String(m.text).slice(0, 300)} ${m.url || ''}`);
        } else if (inner.id === 2) {
          clearTimeout(t);
          const v = inner.result && inner.result.result && inner.result.result.value;
          done({ path, page: v ? JSON.parse(v) : inner, console: console_ });
        }
      }
    };
    ws.onerror = (e) => { clearTimeout(t); done({ path, wsError: String(e.message || e.type) }) };
  });
  console.log(JSON.stringify(out, null, 1));
}
process.exit(0);

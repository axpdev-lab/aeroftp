// SPDX-License-Identifier: GPL-3.0-or-later
// Minimal WebKit inspector client: read the current page's text.
const ws = new WebSocket(`ws://127.0.0.1:${process.env.INSPECTOR_PORT}/socket/1/1/WebPage`);
let targetId, outer = 0;
const timer = setTimeout(() => { console.log('TIMEOUT'); process.exit(2); }, 15000);
ws.onmessage = e => {
  const m = JSON.parse(e.data);
  if (m.method === 'Target.targetCreated' && m.params.targetInfo.type === 'page' && !targetId) {
    targetId = m.params.targetInfo.targetId;
    ws.send(JSON.stringify({ id: ++outer, method: 'Target.sendMessageToTarget', params: { targetId,
      message: JSON.stringify({ id: 1, method: 'Runtime.evaluate', params: { expression: 'location.href + "\\n" + document.body.innerText', returnByValue: true } }) } }));
  } else if (m.method === 'Target.dispatchMessageFromTarget') {
    const r = JSON.parse(m.params.message);
    if (r.id === 1) { console.log(r.result?.result?.value ?? JSON.stringify(r)); clearTimeout(timer); ws.close(); process.exit(0); }
  }
};
ws.onerror = () => { console.log('WS ERROR'); process.exit(3); };

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// Owned WebKitGTK development inspector only; never imported by application code.
export function inspectorPort(value = '9222') {
    if (!/^\d{1,5}$/.test(String(value)) || Number(value) < 1024 || Number(value) > 65535) {
        throw Error('INSPECTOR_PORT must be an integer from 1024 to 65535');
    }
    return Number(value);
}

/** WebKit lacks awaitPromise. Evaluate in a page target and poll a unique result slot. */
export async function evaluate(body, { port = inspectorPort(process.env.INSPECTOR_PORT), timeoutMs = 55000 } = {}) {
    const ws = new WebSocket(`ws://127.0.0.1:${inspectorPort(port)}/socket/1/1/WebPage`);
    let outerId = 0, innerId = 0, targetId;
    const pending = new Map();
    let timer;
    const evaluateInPage = expression => new Promise((resolve, reject) => {
        const id = ++innerId;
        pending.set(id, { resolve, reject });
        ws.send(JSON.stringify({ id: ++outerId, method: 'Target.sendMessageToTarget', params: {
            targetId, message: JSON.stringify({ id, method: 'Runtime.evaluate', params: { expression, returnByValue: true } }),
        } }));
    });
    const value = reply => {
        if (reply.error || reply.result?.wasThrown) throw Error('Inspector evaluation failed');
        return reply.result?.result?.value;
    };
    return await new Promise((resolve, reject) => {
        let settled = false;
        const finish = (error, result) => {
            if (settled) return;
            settled = true; clearTimeout(timer);
            for (const call of pending.values()) call.reject(Error('Inspector closed'));
            pending.clear(); ws.close();
            if (error) reject(error); else resolve(result);
        };
        timer = setTimeout(() => finish(Error('Inspector timed out; inspect state before retrying a mutation')), timeoutMs);
        const run = async () => {
            // Prevent complete DEV scripts from reaching an approval/secondary webview.
            const main = value(await evaluateInPage("Boolean(window.__aeroftpController && window.__TAURI_INTERNALS__)"));
            if (!main) throw Error('Expected main development controller webview');
            const slot = `__aeroftp_drive_${crypto.randomUUID().replaceAll('-', '')}`;
            value(await evaluateInPage(`window[${JSON.stringify(slot)}]={state:'pending'};(async()=>{try {
                const result = await (async()=>{${body}})(); window[${JSON.stringify(slot)}]={state:'ok',result};
                }catch(error){window[${JSON.stringify(slot)}]={state:'error',error:String(error?.message || error)};}})();true`));
            while (!settled) {
                const raw = value(await evaluateInPage(`JSON.stringify(window[${JSON.stringify(slot)}] || {state:'gone'})`));
                const result = raw === undefined ? { state: 'gone' } : JSON.parse(raw);
                if (result.state === 'gone') throw Error('Development page changed during request; inspect state before retrying');
                if (result.state !== 'pending') {
                    value(await evaluateInPage(`delete window[${JSON.stringify(slot)}];true`));
                    if (result.state === 'error') throw Error(result.error);
                    finish(null, result.result); return;
                }
                await new Promise(resolve => setTimeout(resolve, 50));
            }
        };
        ws.onmessage = event => {
            let message;
            try { message = JSON.parse(event.data); } catch { finish(Error('Malformed inspector response')); return; }
            if (message.method === 'Target.targetCreated' && message.params.targetInfo.type === 'page' && !targetId) {
                targetId = message.params.targetInfo.targetId;
                void run().catch(error => finish(error));
            } else if (message.method === 'Target.dispatchMessageFromTarget') {
                try {
                    const reply = JSON.parse(message.params.message);
                    const call = pending.get(reply.id);
                    if (call) { pending.delete(reply.id); call.resolve(reply); }
                } catch { finish(Error('Malformed inspector target response')); }
            }
        };
        ws.onerror = () => finish(Error('Owned development inspector connection failed'));
        ws.onclose = () => finish(Error('Inspector disconnected; operation may have run, inspect state before retrying'));
    });
}

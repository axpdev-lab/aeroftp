// SPDX-License-Identifier: GPL-3.0-or-later
// Deterministic, test-only MCP stdio peer. Never used by the app at runtime.
import readline from 'node:readline';

const mode = process.argv[2];
const lines = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
const send = (value) => {
  const frame = `${JSON.stringify(value)}\n`;
  if (mode === 'split') {
    const middle = Math.floor(frame.length / 2);
    process.stdout.write(frame.slice(0, middle));
    setTimeout(() => process.stdout.write(frame.slice(middle)), 10);
  } else {
    process.stdout.write(frame);
  }
};
let initialized = false;

if (mode === 'exit') process.exit(17);
if (mode === 'oversized') process.stdout.write('x'.repeat(70_000));
if (mode === 'partial') {
  process.stdout.write('{"jsonrpc":"2.0","id":1', () => process.exit(0));
}
if (mode === 'partial-hang') process.stdout.write('{"jsonrpc":"2.0","id":1');
if (mode === 'malformed') process.stdout.write('{not json}\n');
if (mode === 'stderr-flood') process.stderr.write('s'.repeat(1_000_000));
if (mode === 'stubborn') setInterval(() => {}, 1000);

lines.on('line', (line) => {
  if (mode === 'silent' || mode === 'stubborn' || mode === 'partial-hang') return;
  const req = JSON.parse(line);
  const id = req.id;
  const method = req.method;
  if (mode.startsWith('session-') && method !== 'server/discover') {
    if (mode === 'session-eof') process.exit(0);
    if (mode === 'session-partial') {
      process.stdout.write('{"jsonrpc":"2.0","id":', () => process.exit(0));
      return;
    }
    if (mode === 'session-partial-hang') { process.stdout.write('{"jsonrpc":"2.0","id":'); return; }
    if (mode === 'session-malformed') { process.stdout.write('{not json}\n'); return; }
    if (mode === 'session-oversized') { process.stdout.write('x'.repeat(70_000)); return; }
    if (mode === 'session-silent') return;
  }
  if (mode.startsWith('legacy') && !initialized && method !== 'initialize') {
    process.exit(0); // Models legacy servers that exit on a pre-initialize probe.
  }
  if (method === 'tools/list' || method === 'tools/call') {
    if (mode === 'notifications') {
      send({ jsonrpc: '2.0', method: 'notifications/progress', params: { progress: 1 } });
      send({ jsonrpc: '2.0', method: 'notifications/tools/list_changed' });
    }
    if (mode === 'notification-request') {
      send({ jsonrpc: '2.0', id: 42, method: 'notifications/message', params: {} });
      return;
    }
    if (mode === 'notification-flood') {
      setInterval(() => send({ jsonrpc: '2.0', method: 'notifications/progress', params: {} }), 5);
      return;
    }
  }
  if (method === 'server/discover') {
    if (mode === 'modern-fallback') {
      send({ jsonrpc: '2.0', id, error: { code: -32601, message: 'Use initialize' } });
      return;
    }
    if (mode === 'modern-slow-probe') {
      setTimeout(() => send({ jsonrpc: '2.0', id, result: { resultType: 'complete', supportedVersions: ['2026-07-28'], capabilities: { tools: {} }, ttlMs: 0, cacheScope: 'private', _meta: { 'io.modelcontextprotocol/serverInfo': { name: 'fixture', version: '1' } } } }), 3500);
      return;
    }
    const version = req.params?._meta?.['io.modelcontextprotocol/protocolVersion'];
    if (version !== '2026-07-28') {
      send({ jsonrpc: '2.0', id, error: { code: -32022, message: 'Unsupported protocol version', data: { supported: ['2026-07-28'], requested: version } } });
    } else if (mode === 'no-overlap') {
      send({ jsonrpc: '2.0', id, result: { resultType: 'complete', supportedVersions: ['2027-01-01'], capabilities: {}, ttlMs: 0, cacheScope: 'private' } });
    } else if (mode === 'invalid-discovery') {
      send({ jsonrpc: '2.0', id, result: {} });
    } else {
      send({ jsonrpc: '2.0', id, result: { resultType: 'complete', supportedVersions: ['2026-07-28'], capabilities: { tools: {} }, ttlMs: 0, cacheScope: 'private', _meta: { 'io.modelcontextprotocol/serverInfo': { name: 'fixture', version: '1' } } } });
    }
    return;
  }
  if (method === 'initialize') {
    initialized = true;
    const protocolVersion = mode === 'legacy-new' ? '2025-11-25' : mode === 'legacy-other' ? '2025-03-26' : '2024-11-05';
    send({ jsonrpc: '2.0', id, result: { protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'fixture-legacy', version: '1' } } });
    return;
  }
  if (method === 'notifications/initialized') return;
  if (method === 'notifications/cancelled') {
    process.stderr.write('cancelled');
    return;
  }
  // A modern probe can time out on a loaded host, followed by a fresh legacy
  // initialize. Once accepted, use that session's negotiated wire format.
  const legacy = initialized || mode.startsWith('legacy');
  if (!legacy) {
    const meta = req.params?._meta;
    if (meta?.['io.modelcontextprotocol/protocolVersion'] !== '2026-07-28' || typeof meta?.['io.modelcontextprotocol/clientCapabilities'] !== 'object') {
      send({ jsonrpc: '2.0', id, error: { code: -32602, message: 'missing modern metadata' } });
      return;
    }
  }
  if (method === 'tools/list') {
    send({ jsonrpc: '2.0', id, result: { ...(legacy ? {} : { resultType: 'complete' }), tools: [{ name: 'echo', inputSchema: { type: 'object', properties: { text: { type: 'string' } } } }] } });
  } else if (method === 'tools/call') {
    if (req.params.name === 'wait') return;
    send({ jsonrpc: '2.0', id, result: { ...(legacy ? {} : { resultType: 'complete' }), content: [{ type: 'text', text: req.params.arguments?.text ?? '' }] } });
  } else {
    send({ jsonrpc: '2.0', id, error: { code: -32601, message: 'unknown method' } });
  }
});

if (mode !== 'stubborn') lines.on('close', () => { process.exitCode = 0; });

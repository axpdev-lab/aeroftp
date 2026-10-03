# MCP client interoperability captures

Captured on 2026-10-02. Copies of the internal evidence in
`docs/dev/drafts/aa26-mcp-interop-fixtures/`; no secrets or session identifiers.

- `server-everything-2026.8.31-tools-list.json`: complete, unmodified `tools/list`
  response from `@modelcontextprotocol/server-everything@2026.8.31`, after STDIO
  initialization with protocol version 2025-06-18. Contains thirteen tool schemas.
- `server-everything-stdio-discover-reply.json`: its response to `server/discover`.
- `deepwiki-modern-probe.body.json`: unmodified HTTP 400 JSON-RPC response from
  `https://mcp.deepwiki.com/mcp` to `server/discover` with protocol version
  2026-07-28. The SDK reports `id: "server-error"` and error code -32600.

The reference server can be reproduced in a scratch directory with
`npm install --ignore-scripts @modelcontextprotocol/server-everything@2026.8.31`.
Bundle `dist/index.js` as one Node ESM file with the repository's installed
rolldown (`--platform node --format esm --inline-dynamic-imports`), then run
`node everything.bundle.mjs stdio` in the AeroFTP sandbox.

# AeroFTP TODO - Phase 2: Multi-Protocol Provider Fixes

**Created:** 2026-01-22  
**Version Target:** 1.2.5  
**Status:** Active Development

---

## 📋 Summary

This document tracks critical bugs discovered during multi-protocol testing (FTP, S3, WebDAV, OAuth providers). These must be fixed before finalizing v1.2.5.

---

## 🐛 Bug #1: FTP Empty Remote List After S3 Connection

### Problem Description
After connecting to an S3 server, switching to any FTP server shows an empty remote file list. Returning to the FTP tab and switching again makes it work (reconnection triggers).

### Root Cause Analysis
When switching from S3 to FTP, the code path in `switchSession()` (App.tsx:1154-1165) correctly calls `connect_ftp` and `list_files`. However, there's a race condition where the `ProviderState` (used by provider_* commands) may still be "active" from the S3 session, causing conflicts.

**Key Code Locations:**
- `App.tsx:1154-1165` - FTP reconnection in switchSession
- `useFileOperations.ts:61-77` - loadRemoteFiles (now correctly checks protocol)
- `provider_commands.rs:164-182` - provider_disconnect

### Proposed Fix
1. Ensure `provider_disconnect()` is called BEFORE `connect_ftp()` when switching FROM a provider session TO an FTP session.
2. Clear the ProviderState completely so FTP commands don't conflict.

```typescript
// In switchSession, FTP block (line ~1154)
// Already has this but may need ordering fix:
try { await invoke('provider_disconnect'); } catch { }  // THIS MUST COME FIRST
await invoke('connect_ftp', { params: targetSession.connectionParams });
```

### Status: ✅ PARTIALLY FIXED
The code already has `provider_disconnect` before FTP connect. Issue may be timing or state-related. Need further testing.

---

## 🐛 Bug #2: S3 Reconnection "Bucket Name Required" Error

### Problem Description
When switching tabs TO an S3 session that was previously connected, the error "S3 requires a bucket name" appears. This happens because the session's `connectionParams` don't get converted to the proper provider format.

### Root Cause Analysis
In `switchSession()` (App.tsx:1126-1171), S3/WebDAV sessions were calling:
```typescript
await invoke('provider_connect', { params: connectParams });
```
But `connectParams` is the session's `ConnectionParams` format (with `options.bucket`), while `provider_connect` expects a flat format with `bucket` at the top level.

### Proposed Fix ✅ APPLIED
We already fixed this by converting params:
```typescript
const providerParams = {
  protocol: protocol,
  server: connectParams.server,
  bucket: connectParams.options?.bucket,  // <-- Extracted from options
  region: connectParams.options?.region || 'us-east-1',
  // ... etc
};
await invoke('provider_connect', { params: providerParams });
```

### Status: ✅ FIXED
Applied in current session. Needs testing verification.

---

## 🐛 Bug #3: Activity Log "Running" Entries Not Replaced by "Completed"

### Problem Description
In the Activity Log, when downloading a file, both "Downloading..." (running status) and "Downloaded ✓" (success status) entries appear. The running entry should be removed/updated when the operation completes.

### Root Cause Analysis
The current implementation creates **new** log entries for each event:
- `transfer_event: 'start'` → Creates "Downloading..." with `status: 'running'`
- `transfer_event: 'complete'` → Creates a **new** "Downloaded ✓" with `status: 'success'`

The original "running" entry is never updated or removed.

**Key Code Locations:**
- `App.tsx:500-546` - Transfer event listener
- `useActivityLog.ts:103-129` - log() function creates new entries
- `useActivityLog.ts:132-140` - updateEntry() exists but is never used for transfers

### Proposed Fix
Use `updateEntry()` instead of creating new entries:
1. Store the log entry ID when 'start' is emitted
2. Use `updateEntry()` when 'complete' or 'error' is received

```typescript
// Map to track transfer_id -> logEntry id
const transferToLogId = useRef<Map<string, string>>(new Map());

// On 'start':
const logId = activityLog.log('DOWNLOAD', `⬇️ ${filename}`, 'running');
transferToLogId.current.set(data.transfer_id, logId);

// On 'complete':
const logId = transferToLogId.current.get(data.transfer_id);
if (logId) {
  activityLog.updateEntry(logId, { 
    status: 'success', 
    message: `⬇️ ✓ ${filename}` 
  });
  transferToLogId.current.delete(data.transfer_id);
} else {
  // Fallback: create new entry
  activityLog.log('SUCCESS', `⬇️ ${filename}`, 'success');
}
```

### Status: ✅ FIXED
Implemented in this session using `updateEntry()` to update existing log entries instead of creating duplicates.

---

## 🐛 Bug #4: OAuth Repeated Authorization on Reconnect

### Problem Description
When disconnecting from an OAuth provider (Google Drive) and reconnecting (either via Saved Servers or direct click), the browser opens again asking for authorization even though tokens are already stored.

### Root Cause Analysis
The issue is in `SavedServers.tsx:133-140` which always calls `oauth2_full_auth`:
```typescript
await invoke('oauth2_full_auth', { params: {...} });  // ALWAYS starts new auth
await invoke('oauth2_connect', { params: {...} });
```

But `oauth2_full_auth` ALWAYS starts a new authentication flow (opens browser), ignoring existing valid tokens. The correct flow should:
1. Check if tokens exist (via `oauth2_has_tokens`)
2. If tokens exist AND are valid → call `oauth2_connect` directly (uses stored tokens)
3. If no tokens OR expired → call `oauth2_full_auth`

The `OAuthConnect.tsx` component DOES check `hasExistingTokens` (line 233) and calls `handleQuickConnect` which only calls `connect()` (which is `oauth2_connect`). This is correct.

**The problem:** `SavedServers.tsx` doesn't check for existing tokens before calling `oauth2_full_auth`.

### Proposed Fix ✅ APPLIED
Updated `SavedServers.tsx:handleConnect()`:

```typescript
// Check if tokens already exist
const hasTokens = await invoke<boolean>('oauth2_has_tokens', { 
  provider: oauthProvider 
});

if (!hasTokens) {
  // No tokens - need full auth flow (opens browser)
  await invoke('oauth2_full_auth', { params });
}
// Connect using stored tokens
const displayName = await invoke<string>('oauth2_connect', { params });
```

### Status: ✅ FIXED
Applied in current session. OAuth providers now skip authorization if tokens exist.

---

## 📝 Implementation Priority

| #   | Bug                        | Severity | Effort | Status    |
| --- | -------------------------- | -------- | ------ | --------- |
| 1   | FTP Empty List after S3    | High     | Medium | ⚠️ Testing |
| 2   | S3 Bucket Name Error       | High     | Low    | ✅ Fixed   |
| 3   | Activity Log Running State | Medium   | Low    | ✅ Fixed   |
| 4   | OAuth Repeated Auth        | High     | Low    | ✅ Fixed   |

---

## ✅ Completed Fixes (This Session)

### Fix A: WebDAV Remote File Listing (useFileOperations.ts)
- **Problem:** `loadRemoteFiles()` always called FTP's `list_files` instead of `provider_list_files` for WebDAV/S3
- **Solution:** Added protocol check to call correct command
- **Status:** ✅ Implemented and compiled

### Fix B: S3/WebDAV Session Switch Parameter Format (App.tsx)
- **Problem:** `switchSession()` passed raw `connectionParams` to `provider_connect` which expects flat format
- **Solution:** Convert params to proper format with `bucket`, `region`, etc. at top level
- **Status:** ✅ Implemented and compiled

### Fix C: Activity Log Update Instead of Duplicate (App.tsx)
- **Problem:** Transfer events created new "complete" entries instead of updating "running" ones
- **Solution:** Added `transferIdToLogId` ref to track entries, use `updateEntry()` on complete/error
- **Status:** ✅ Implemented and compiled

### Fix D: OAuth Token Check Before Auth (SavedServers.tsx)
- **Problem:** `SavedServers.handleConnect()` always called `oauth2_full_auth` even when tokens exist
- **Solution:** Check `oauth2_has_tokens` first, only call `oauth2_full_auth` if no tokens
- **Status:** ✅ Implemented and compiled

---

## 🎯 Next Steps

1. ~~**Test WebDAV Fix** - Verify remote file listing now works~~ → USER testing
2. ~~**Implement Bug #4 Fix** - OAuth token check before auth~~ → ✅ Done
3. ~~**Implement Bug #3 Fix** - Activity log entry updates~~ → ✅ Done
4. **Full Multi-Protocol Switch Testing:**
   - FTP → S3 → FTP (verify no empty lists)
   - FTP → WebDAV → S3 → WebDAV (verify reconnects)
   - OAuth → disconnect → reconnect (verify no browser)
5. **Release v1.2.5** after all tests pass

---

## 🔮 Future Enhancements (Post 1.2.5)

- [ ] **Credential Encryption** - Encrypt stored passwords in localStorage/keyring
- [ ] **OAuth Token Refresh** - Auto-refresh expired tokens without full re-auth
- [ ] **Connection Pool** - Keep multiple connections alive for faster tab switching
- [ ] **Offline Mode** - Cache file listings for offline browsing

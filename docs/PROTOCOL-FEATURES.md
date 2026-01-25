# AeroFTP Protocol Features Matrix

> Last Updated: 25 January 2026
> Version: v1.2.7

---

## Share Link Support

### Current Implementation Status

| Protocol | Share Link Support | Implementation | Notes |
|----------|-------------------|----------------|-------|
| **FTP** | Via AeroCloud | `generate_share_link` | Requires AeroCloud setup with `public_url_base` |
| **FTPS** | Via AeroCloud | `generate_share_link` | Same as FTP |
| **SFTP** | Via AeroCloud | `generate_share_link` | Same as FTP, new in v1.3.0 |
| **WebDAV** | Via AeroCloud | `generate_share_link` | No native support |
| **S3** | Native (Pre-signed URLs) | `provider_create_share_link` | 7-day expiry default |
| **Google Drive** | Native | `provider_create_share_link` | Permanent "anyone with link" |
| **Dropbox** | Native | `provider_create_share_link` | Uses shared_links API |
| **OneDrive** | Native | `provider_create_share_link` | "view" permission link |
| **MEGA.nz** | Not Available | N/A | API doesn't expose share links |

### Context Menu Visibility

**Current Logic (App.tsx lines 2395-2412):**
```typescript
// Native Share Link shown for OAuth providers only
const isOAuthProvider = connectionParams.protocol &&
    ['googledrive', 'dropbox', 'onedrive'].includes(connectionParams.protocol);
if (isOAuthProvider && !file.is_dir) {
    items.push({ label: 'Create Share Link', ... });
}
```

**Issue Found:** S3 supports native share links (pre-signed URLs) but the context menu only shows "Create Share Link" for OAuth providers. S3 should be included.

**AeroCloud Share Link (lines 2377-2392):**
- Shows when AeroCloud is active with `public_url_base` configured
- File must be within AeroCloud remote folder
- Works for any protocol including FTP, SFTP, WebDAV

### Recommended Fix

Add S3 to the Share Link context menu:

```typescript
// Add native Share Link for providers that support it
const supportsNativeShareLink = connectionParams.protocol &&
    ['googledrive', 'dropbox', 'onedrive', 's3'].includes(connectionParams.protocol);
if (supportsNativeShareLink && !file.is_dir) {
    items.push({ label: 'Create Share Link', ... });
}
```

---

## File Operations Matrix

### Operation Support by Protocol

| Operation | FTP | FTPS | SFTP | WebDAV | S3 | Google Drive | Dropbox | OneDrive | MEGA |
|-----------|-----|------|------|--------|-----|--------------|---------|----------|------|
| List | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Upload | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Download | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Delete | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Rename | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Move | ✅ | ✅ | ✅ | ✅ | ✅* | ✅ | ✅ | ✅ | ✅ |
| Copy | ✅ | ✅ | ✅ | ✅* | ✅* | ✅ | ✅ | ✅ | ✅ |
| Mkdir | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Chmod | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| Stat | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Share Link | AeroCloud | AeroCloud | AeroCloud | AeroCloud | ✅ | ✅ | ✅ | ✅ | ❌ |
| Sync | AeroCloud | AeroCloud | AeroCloud | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |

*Note: S3/WebDAV copy/move is implemented as copy+delete (no native server-side operation)*

---

## Competitor Comparison: File Operations

### Context Menu Operations

| Operation | AeroFTP | FileZilla | Cyberduck | WinSCP | Transmit |
|-----------|---------|-----------|-----------|--------|----------|
| Download | ✅ | ✅ | ✅ | ✅ | ✅ |
| Upload | ✅ | ✅ | ✅ | ✅ | ✅ |
| Rename (F2) | ✅ | ✅ | ✅ | ✅ | ✅ |
| Delete (Del) | ✅ | ✅ | ✅ | ✅ | ✅ |
| New Folder | ✅ | ✅ | ✅ | ✅ | ✅ |
| Copy Path | ✅ | ❌ | ✅ | ✅ | ✅ |
| Copy FTP URL | ✅ | ❌ | ✅ | ✅ | ❌ |
| Open With | ❌ | ❌ | ✅ | ✅ | ✅ |
| Preview | ✅ | ❌ | ✅ | ❌ | ✅ |
| Edit | ✅ Monaco | ✅ External | ✅ External | ✅ Internal | ✅ External |
| Share Link | ✅ | ❌ | ✅ | ❌ | ❌ |
| Properties | ❌ | ✅ | ✅ | ✅ | ✅ |
| Compress | ❌ | ❌ | ✅ | ✅ | ✅ |
| Checksum | ❌ | ❌ | ✅ | ✅ | ❌ |

### Unique AeroFTP Features
- **Monaco Code Editor**: VS Code-quality editing directly in the client
- **Universal Preview**: Media player, image viewer, PDF, code with syntax highlighting
- **AeroCloud Sync**: Bidirectional sync any FTP/SFTP to personal cloud
- **AI Assistant**: File analysis and command suggestions
- **Multiple Cloud Providers**: Google Drive, Dropbox, OneDrive, S3, MEGA in one client

### Missing Operations (vs Competitors)
1. **Properties Dialog**: Show detailed file metadata
2. **Compress/Archive**: Create ZIP/TAR from remote files
3. **Checksum Verification**: MD5/SHA verification
4. **Open With External App**: Launch files in associated apps

---

## Drag & Drop Implementation

### Current State

**Status:** Not implemented for file transfers

The `tauri.conf.json` mentions "drag & drop transfers" in the app description, but the actual implementation is limited to:
- Image panning in the viewer component
- No cross-panel drag & drop
- No local-to-remote drag & drop
- No external file drop support

### Competitor Drag & Drop Features

| Feature | FileZilla | Cyberduck | WinSCP | Transmit | ForkLift |
|---------|-----------|-----------|--------|----------|----------|
| Panel-to-Panel | ✅ | ❌ | ✅ | ❌ | ✅ |
| External Drop | ✅ | ✅ | ✅ | ✅ | ✅ |
| Drag to Desktop | ✅ | ✅ | ✅ | ✅ | ✅ |
| Drop to Upload | ✅ | ✅ | ✅ | ✅ | ✅ |
| Visual Feedback | ✅ | ✅ | ✅ | ✅ | ✅ |

### Planned Drag & Drop (v1.4.0)

From `TODO-AEROCLOUD-2.0.md`:

```
Sprint 2.5: UX Enhancements (Drag & Drop, Move)

- [ ] Advanced Drag & Drop:
  - Cross-panel file transfers
  - Visual drop zones
  - Multi-select drag support
  - Progress indicators during drag

- [ ] Drag & Drop Cross-Panel (v1.4.0)
```

### Implementation Approach

For multi-protocol support, drag & drop should:

1. **Use HTML5 Drag API**: `onDragStart`, `onDragOver`, `onDrop`
2. **Identify source/target panels**: Local vs Remote
3. **Handle multi-protocol scenarios**:
   - Local → Remote (any protocol): Upload operation
   - Remote → Local: Download operation
   - Remote Panel A → Remote Panel B: Download+Upload (future multi-session)
4. **Visual feedback**: Highlight valid drop zones
5. **External files**: Use Tauri file drop events

**Required Events:**
```typescript
// On file row
onDragStart={(e) => handleDragStart(e, file)}
draggable={true}

// On panel
onDragOver={(e) => handleDragOver(e)}
onDrop={(e) => handleDrop(e, targetPath)}
```

---

## Protocol-Specific Limitations

### FTP/FTPS
- No native move (uses rename)
- Limited metadata (no creation time)
- Connection keep-alive required

### SFTP (v1.3.0)
- SSH key authentication required for some servers
- Chmod support (unlike cloud providers)
- Full Unix permissions

### WebDAV
- No chmod support
- PROPFIND for metadata
- Some servers have limited MOVE support

### S3
- No native move (copy+delete)
- Pre-signed URLs for sharing (expiry)
- No real directories (prefix-based)

### OAuth Providers (Google/Dropbox/OneDrive)
- Token refresh required
- Rate limits apply
- No chmod support
- Share link permissions vary

### MEGA
- Client-side encryption
- No native share link API exposed
- Large file upload chunking required

---

## Recommendations

### Immediate (v1.2.8)
1. **Fix S3 Share Link**: Add S3 to context menu Share Link providers
2. **Update Description**: Remove "drag & drop" claim until implemented

### Short-term (v1.3.0)
1. **SFTP Integration**: Complete (done)
2. **Keyboard Shortcuts**: F2 (rename), Del (delete), Ctrl+C/V (copy)
3. **Properties Dialog**: Show file metadata

### Medium-term (v1.4.0)
1. **Drag & Drop**: Full implementation with visual feedback
2. **External File Drop**: Tauri file drop integration
3. **Move Operation**: Explicit move vs copy

---

*This document is maintained as part of AeroFTP protocol documentation.*

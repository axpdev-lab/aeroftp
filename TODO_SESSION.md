# AeroFTP - Session TODO

> Aggiornato: 25 gennaio 2026
> Versione Corrente: **v1.2.8**
> Sprint Corrente: **Sprint 2.5 - UX & File Management** ✅ COMPLETATO

---

## 📊 AeroCloud 2.0 Roadmap Status

| Sprint | Feature | Versione | Stato |
|--------|---------|----------|-------|
| **Sprint 1** | Multi-Protocol (WebDAV, S3) | v1.1.0 | ✅ Rilasciato |
| **Sprint 2** | OAuth2 Cloud Providers | v1.2.2 | ✅ Completato |
| **Sprint 2.5** | MEGA + UX + File Management | v1.2.8 | ✅ In Testing |
| **Sprint 3** | Encryption (Cryptomator) | v1.3.0 | ⏳ Prossimo |
| **Sprint 4** | Collaborative Sharing | v1.4.0 | 📋 Pianificato |
| **Sprint 5** | CLI/Automation | v1.5.0 | 📋 Pianificato |

---

## ✅ Sprint 2.5 - Completato (v1.2.8)

### File Management Pro Features
- [x] Properties Dialog - File metadata, MIME type, permissions
- [x] Checksum Verification - MD5/SHA-256 in Properties
- [x] Compress/Archive - ZIP creation and extraction
- [x] Overwrite Confirmation Dialog - Smart file conflict resolution
- [x] Same-panel Drag & Drop - Move files to folders
- [x] Activity Log Move Tracking - MOVE operation type

### Global Multilingual Expansion
- [x] 46 nuove lingue (da 5 a 51 totali)
- [x] Supporto RTL (Arabic, Hebrew, Urdu, Persian)
- [x] Superato FileZilla (47 lingue)!

### UI Polish
- [x] Support Modal icons - Official SVG logos
- [x] Address bar icons - Chrome-style (no backgrounds)
- [x] Disconnect/Connect button translations fixed
- [x] Crypto icons with brand colors

### v1.2.7 (MEGA Integration)
- [x] MEGA.nz provider completo
- [x] Keep-Alive fix per stateless providers
- [x] Terminal Tokyo Night theme
- [x] Protocol selector UX improvements

### v1.2.6 (Auto-Update)
- [x] Auto-Update System
- [x] Smart Format Detection (DEB, AppImage, Snap, etc.)
- [x] Update Toast notifications

---

## 📋 Sprint 3 - Archive & Encryption (Prossimo)

### Obiettivi v1.3.0

#### Archive Features (HIGH PRIORITY)
> See [docs/ARCHIVE-FEATURES-ROADMAP.md](docs/ARCHIVE-FEATURES-ROADMAP.md)
- [ ] Extract to subfolder (best practice)
- [ ] 7z format with AES-256 encryption
- [ ] Password-protected archives
- [ ] Multi-format: TAR.GZ, TAR.XZ, RAR (read)
- [ ] Archive browser dialog
- [ ] Progress tracking

#### Other Features
- [ ] Cryptomator vault support
- [ ] Keyboard Shortcuts (F2, Del, Ctrl+C/V)
- [ ] Cross-panel Drag & Drop
- [ ] Host key verification per SFTP

---

## 📈 Competitor Comparison

### Languages
| Client | Languages | Status |
|--------|-----------|--------|
| **AeroFTP** | **51** | 🥇 Leader |
| FileZilla | 47 | 🥈 |
| WinSCP | ~15 | |
| Cyberduck | ~10 | |

### Unique Features
| Feature | AeroFTP | Others |
|---------|---------|--------|
| MEGA.nz | ✅ | ❌ |
| AeroCloud Sync | ✅ | ❌ |
| Monaco Editor | ✅ | ❌ |
| AI Assistant | ✅ | ❌ |
| 51 Languages | ✅ | ❌ |

---

## 🔧 Quick Commands

```bash
# Build frontend
cd /var/www/html/FTP_CLIENT_GUI && npm run build

# Check Rust
cd src-tauri && cargo check

# Build Tauri app
npm run tauri build

# Dev mode
npm run tauri dev

# Create release tag
git tag v1.2.8 && git push origin v1.2.8
```

---

## 📝 Note Tecniche

### Overwrite Dialog
- Shows when files exist at destination
- Overwrite, Skip, Rename, Cancel options
- "Apply to all" for batch operations
- Tracks skipped files in Activity Log

### Activity Log MOVE
- New operation type for drag-to-folder moves
- Teal color icon in all themes
- Translation keys: move_start, move_success, move_error

### OAuth2 Callback Server
- Porta: 17548
- Redirect URI: http://localhost:17548/callback
- Da configurare nelle console developer di ogni provider

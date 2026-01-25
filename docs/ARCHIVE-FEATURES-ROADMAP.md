# AeroFTP Archive Features Roadmap

> Created: 25 January 2026
> Target Version: v1.3.0
> Priority: HIGH

---

## Current State (v1.2.8)

### Implemented
- ZIP compression (Deflate level 6)
- ZIP extraction
- Multi-file/folder compression
- Recursive folder support

### Issues
1. **Extract to same directory**: Files extracted directly to current folder, not into a subfolder
2. **ZIP only**: No support for 7z, RAR, TAR, GZ
3. **No encryption**: Cannot create password-protected archives
4. **No progress**: No progress callback for large archives

---

## Target Features (v1.3.0)

### 1. Extract to Folder (Best Practice)
**Problem**: When extracting `archive.zip`, files go directly to current folder.
**Solution**: Create `archive/` subfolder and extract there.

```rust
// Current behavior:
// archive.zip -> current_folder/file1.txt, file2.txt

// New behavior:
// archive.zip -> current_folder/archive/file1.txt, file2.txt
```

**Implementation**:
```rust
fn extract_archive(archive_path: &str, output_dir: &str, create_subfolder: bool) -> Result<String> {
    let extract_path = if create_subfolder {
        // Get archive name without extension
        let archive_name = Path::new(archive_path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        Path::new(output_dir).join(archive_name.as_ref())
    } else {
        PathBuf::from(output_dir)
    };
    // ... extraction logic
}
```

### 2. 7z Support with AES-256 Encryption

**Rust Crate**: `sevenz-rust` or `sevenz-rust2`
- Pure Rust implementation
- AES-256 encryption support
- Password protection
- LZMA/LZMA2 compression

**Dependencies** (Cargo.toml):
```toml
sevenz-rust2 = "0.10"  # 7z read/write with encryption
```

**Features**:
- Create 7z archives
- Extract 7z archives
- Password-protected archives (AES-256)
- Solid archives for better compression

**API Design**:
```rust
#[tauri::command]
async fn compress_7z(
    paths: Vec<String>,
    output_path: String,
    password: Option<String>,      // Optional password
    compression_level: Option<u32>, // 0-9, default 6
    solid: Option<bool>,           // Solid archive mode
) -> Result<String, String>;

#[tauri::command]
async fn extract_7z(
    archive_path: String,
    output_dir: String,
    password: Option<String>,
    create_subfolder: bool,
) -> Result<String, String>;
```

### 3. Multi-Format Support

| Format | Read | Write | Encryption | Crate |
|--------|------|-------|------------|-------|
| ZIP | ✅ | ✅ | ✅ (AES-256) | `zip` |
| 7z | ✅ | ✅ | ✅ (AES-256) | `sevenz-rust2` |
| TAR | ✅ | ✅ | ❌ | `tar` |
| TAR.GZ | ✅ | ✅ | ❌ | `tar` + `flate2` |
| TAR.XZ | ✅ | ✅ | ❌ | `tar` + `xz2` |
| TAR.BZ2 | ✅ | ✅ | ❌ | `tar` + `bzip2` |
| RAR | ✅ | ❌* | ✅ | `unrar` |

*RAR write requires proprietary license, read-only is free.

**Dependencies** (Cargo.toml):
```toml
# Archive formats
zip = "2.4"           # ZIP with AES encryption
sevenz-rust2 = "0.10" # 7z with AES encryption
tar = "0.4"           # TAR archives
flate2 = "1.0"        # GZIP compression
xz2 = "0.1"           # XZ/LZMA compression
bzip2 = "0.4"         # BZ2 compression
unrar = "0.5"         # RAR extraction (read-only)
```

### 4. Unified Archive API

```rust
pub enum ArchiveFormat {
    Zip,
    SevenZ,
    Tar,
    TarGz,
    TarXz,
    TarBz2,
    Rar,
}

pub struct ArchiveOptions {
    pub format: ArchiveFormat,
    pub password: Option<String>,
    pub compression_level: u32,      // 0-9
    pub create_subfolder: bool,      // For extraction
    pub encrypt_filenames: bool,     // 7z only
}

#[tauri::command]
async fn create_archive(
    paths: Vec<String>,
    output_path: String,
    options: ArchiveOptions,
) -> Result<ArchiveResult, String>;

#[tauri::command]
async fn extract_archive_v2(
    archive_path: String,
    output_dir: String,
    password: Option<String>,
    create_subfolder: bool,
) -> Result<ArchiveResult, String>;

#[tauri::command]
async fn list_archive_contents(
    archive_path: String,
    password: Option<String>,
) -> Result<Vec<ArchiveEntry>, String>;

pub struct ArchiveResult {
    pub success: bool,
    pub files_processed: u32,
    pub total_size: u64,
    pub output_path: String,
}

pub struct ArchiveEntry {
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    pub is_directory: bool,
    pub is_encrypted: bool,
    pub modified: Option<String>,
}
```

### 5. Frontend UI Enhancements

#### Context Menu Updates
```
Right-click on file(s):
├── Compress
│   ├── Create ZIP archive
│   ├── Create 7z archive
│   ├── Create 7z archive (encrypted)...  → Password dialog
│   ├── Create TAR.GZ archive
│   └── Custom...  → Format selection dialog

Right-click on archive:
├── Extract Here
├── Extract to folder "archive_name/"  ← NEW (default)
├── Extract to...  → Folder picker
└── List Contents...  → Archive browser dialog
```

#### Password Dialog (for encrypted archives)
```
┌─────────────────────────────────────────┐
│  🔐 Create Encrypted Archive            │
├─────────────────────────────────────────┤
│  Archive: documents.7z                  │
│                                         │
│  Password: [••••••••••]  👁             │
│  Confirm:  [••••••••••]  👁             │
│                                         │
│  ☐ Encrypt file names (7z only)        │
│  ☐ Remember password this session      │
│                                         │
│  [Cancel]              [Create Archive] │
└─────────────────────────────────────────┘
```

#### Extract Password Dialog
```
┌─────────────────────────────────────────┐
│  🔐 Password Required                   │
├─────────────────────────────────────────┤
│  Archive: secrets.7z                    │
│  The archive is password protected.     │
│                                         │
│  Password: [••••••••••]  👁             │
│                                         │
│  [Cancel]                    [Extract]  │
└─────────────────────────────────────────┘
```

### 6. Progress Tracking

For large archives, show progress:
```rust
#[tauri::command]
async fn extract_archive_with_progress(
    archive_path: String,
    output_dir: String,
    options: ExtractOptions,
    window: tauri::Window,
) -> Result<ArchiveResult, String> {
    // Emit progress events
    window.emit("archive-progress", ProgressEvent {
        current: i,
        total: archive.len(),
        current_file: file_name,
        bytes_processed: bytes,
    })?;
}
```

---

## Implementation Plan

### Phase 1: Quick Fixes (v1.2.9 patch)
- [ ] Add `create_subfolder` parameter to `extract_archive`
- [ ] Default to creating subfolder with archive name
- [ ] Update frontend to use new parameter
- **Effort**: 2-3 hours

### Phase 2: 7z Support (v1.3.0)
- [ ] Add `sevenz-rust2` dependency
- [ ] Implement `compress_7z` command
- [ ] Implement `extract_7z` command
- [ ] Add password dialog component
- [ ] Update context menu with 7z options
- **Effort**: 1-2 days

### Phase 3: Multi-Format (v1.3.0)
- [ ] Add TAR/GZ/XZ/BZ2 support
- [ ] Add RAR extraction (read-only)
- [ ] Implement `list_archive_contents`
- [ ] Create archive browser dialog
- [ ] Add format auto-detection
- **Effort**: 2-3 days

### Phase 4: Polish (v1.3.0)
- [ ] Progress tracking with events
- [ ] Cancellation support
- [ ] Error handling improvements
- [ ] Activity Log integration
- [ ] Translations for all 51 languages
- **Effort**: 1 day

---

## Files to Modify/Create

### Backend (Rust)
| File | Action |
|------|--------|
| `Cargo.toml` | Add archive dependencies |
| `src/lib.rs` | Update existing commands |
| `src/archive.rs` | **NEW** - Archive module |

### Frontend (React)
| File | Action |
|------|--------|
| `src/App.tsx` | Update context menu handlers |
| `src/components/PasswordDialog.tsx` | **NEW** - Password input dialog |
| `src/components/ArchiveBrowser.tsx` | **NEW** - Archive contents viewer |
| `src/i18n/locales/*.json` | Add archive translations |

---

## Security Considerations

1. **Password Handling**
   - Never log passwords
   - Clear from memory after use
   - Don't store in localStorage

2. **Path Traversal Prevention**
   - Validate extracted paths
   - Reject paths with `..` or absolute paths
   - Stay within output directory

3. **Zip Bomb Protection**
   - Check compression ratio
   - Limit maximum extracted size
   - Warn user if suspicious

---

## Competitor Comparison

| Feature | AeroFTP (target) | 7-Zip | WinRAR | Cyberduck |
|---------|------------------|-------|--------|-----------|
| ZIP | ✅ | ✅ | ✅ | ✅ |
| 7z | ✅ | ✅ | ✅ | ❌ |
| RAR read | ✅ | ✅ | ✅ | ❌ |
| RAR write | ❌ | ❌ | ✅ | ❌ |
| TAR.GZ | ✅ | ✅ | ✅ | ❌ |
| AES-256 | ✅ | ✅ | ✅ | ❌ |
| Integrated | ✅ | ❌ | ❌ | ❌ |

**AeroFTP Advantage**: Archive operations integrated directly in file browser, no external tools needed.

---

## Dependencies Size Impact

| Crate | Size | Notes |
|-------|------|-------|
| `sevenz-rust2` | ~500KB | Pure Rust, no external deps |
| `tar` | ~50KB | Minimal |
| `flate2` | ~200KB | Already in dependency tree |
| `xz2` | ~300KB | LZMA2 support |
| `bzip2` | ~100KB | BZ2 support |
| `unrar` | ~1MB | Requires unrar library |

**Total added**: ~2MB to binary size (acceptable for features gained)

---

## Timeline

| Phase | Target | Status |
|-------|--------|--------|
| Phase 1 (Quick Fix) | v1.2.9 | 📋 Ready to start |
| Phase 2 (7z) | v1.3.0 | 📋 Planned |
| Phase 3 (Multi-Format) | v1.3.0 | 📋 Planned |
| Phase 4 (Polish) | v1.3.0 | 📋 Planned |

**Estimated Total Effort**: 4-6 days

---

## References

- [sevenz-rust2](https://crates.io/crates/sevenz-rust2) - 7z with encryption
- [zip crate](https://crates.io/crates/zip) - ZIP with AES
- [tar crate](https://crates.io/crates/tar) - TAR archives
- [7-Zip Format](https://www.7-zip.org/7z.html) - 7z specification

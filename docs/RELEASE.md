# AeroFTP Release Process

## Quick Reference

```bash
# After updating version files and CHANGELOG.md
git add -A
git commit -m "chore(release): vX.Y.Z Description"
git tag -a vX.Y.Z -m "Release vX.Y.Z - Description"
git push origin main --tags
```

That's it! GitHub Actions handles everything else automatically.

---

## Version Files

Update version in these 4 files before release:

| File | Field |
|------|-------|
| `package.json` | `"version": "X.Y.Z"` |
| `src-tauri/tauri.conf.json` | `"version": "X.Y.Z"` |
| `src-tauri/Cargo.toml` | `version = "X.Y.Z"` |
| `snap/snapcraft.yaml` | `version: 'X.Y.Z'` |

---

## Automated CI/CD Pipeline

### Trigger
The pipeline runs automatically when a tag matching `v*` is pushed.

### Build Matrix

| Platform | Runner | Artifacts |
|----------|--------|-----------|
| Linux | `ubuntu-22.04` | `.deb`, `.rpm`, `.AppImage`, `.snap` |
| Windows | `windows-latest` | `.msi`, `.exe` (NSIS) |
| macOS | `macos-latest` | `.dmg` |

### Distribution

| Destination | Artifacts | Automation |
|-------------|-----------|------------|
| GitHub Releases | All platforms | Automatic via `softprops/action-gh-release` |
| Snap Store | `.snap` (stable channel) | Automatic via `snapcraft upload` |

---

## Snap Store Integration

### How It Works
1. GitHub Actions builds snap using `snapcore/action-build@v1`
2. Uploads to Snap Store with `snapcraft upload --release=stable`
3. Users with AeroFTP installed via snap get auto-updates

### Required Secret
The workflow requires `SNAPCRAFT_STORE_CREDENTIALS` in GitHub repository secrets.

**To generate credentials:**
```bash
# Login to Snapcraft
snapcraft login

# Export credentials (base64 encoded)
snapcraft export-login --snaps=aeroftp --acls=package_upload credentials.txt
cat credentials.txt | base64 -w 0
```

Add the base64 output as `SNAPCRAFT_STORE_CREDENTIALS` secret in:
GitHub Repo → Settings → Secrets and variables → Actions → New repository secret

### Manual Upload (Fallback)
If CI fails or for testing:
```bash
snapcraft login
snapcraft upload aeroftp_X.Y.Z_amd64.snap --release=stable
```

---

## Monitoring Releases

### Check Workflow Status
```bash
# List recent runs
gh run list --limit 5

# Watch specific run
gh run watch <run-id>

# View logs
gh run view <run-id> --log
```

### Verify Snap Store
```bash
# Check published version
snap info aeroftp

# Check pending reviews (if any)
snapcraft status aeroftp
```

### Verify GitHub Release
```bash
gh release view vX.Y.Z
```

---

## Troubleshooting

### Snap Upload Fails
1. Check if `SNAPCRAFT_STORE_CREDENTIALS` secret exists
2. Verify credentials haven't expired (re-export if needed)
3. Check Snap Store review queue for manual review requirements

### Build Fails
1. Check workflow logs: `gh run view <run-id> --log`
2. Common issues:
   - Missing dependencies in `apt-get install`
   - Rust compilation errors
   - TypeScript type errors

### Release Not Appearing
1. Wait for all 3 platform builds to complete
2. Check if tag was pushed: `git tag -l | grep vX.Y.Z`
3. Verify workflow ran: `gh run list`

---

## Release Channels

| Channel | Purpose | Update Frequency |
|---------|---------|------------------|
| `stable` | Production releases | On git tags |
| `edge` | Pre-release testing | Manual uploads |
| `beta` | Public beta testing | Manual uploads |

To release to edge first:
```bash
snapcraft upload aeroftp_X.Y.Z_amd64.snap --release=edge
# After testing, promote to stable:
snapcraft release aeroftp <revision> stable
```

---

*Last updated: February 2026*

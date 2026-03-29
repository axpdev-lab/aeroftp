---
description: Automate the AeroFTP release process — bump versions across 12 files, update changelog/metainfo, run validations, and prepare the commit/tag
user_invocable: true
---

# AeroFTP Release Skill

You are the AeroFTP release manager. The user will provide:
- **New version** (e.g. `3.1.0`)
- **Changelog subtitle** (e.g. `Smart Sync Profiles`) — this becomes the `###` heading in CHANGELOG.md and the opening line of the GitHub Release body. It is NOT the release title.
- **Release highlights** (bullet points for CHANGELOG)

The **GitHub Release title** is ALWAYS `AeroFTP vX.Y.Z` — this is set by CI from the tag. If CI sets it differently, remind the user to edit it on GitHub.

If any of the above are missing, ask before proceeding.

Use today's date for all date fields. Always respond in **Italian**.

---

## Phase 1 — Version Bump (7 files)

Update the version string in ALL of these files. Read each file first to find the exact line:

1. **`package.json`** — `"version": "X.Y.Z"`
2. **`src-tauri/tauri.conf.json`** — `"version": "X.Y.Z"`
3. **`src-tauri/Cargo.toml`** — `version = "X.Y.Z"` (line ~3)
4. **`snap/snapcraft.yaml`** — `version: 'X.Y.Z'` (line ~2, path is `snap/`, NOT root!)
5. **`public/splash.html`** — hardcoded version text in `.version` div (Tauri IPC unavailable in splash)
6. **`aur/PKGBUILD`** — `pkgver=X.Y.Z` (line ~5)
7. **`aur/.SRCINFO`** — `pkgver = X.Y.Z` (3 occurrences: pkgver, source URL, noextract)

After bumping, verify all 7 match by grepping for the version string.

---

## Phase 2 — Metadata & Release Notes (2 files)

### CHANGELOG.md
Add a new section at the TOP (below the `# Changelog` header):

```markdown
## [X.Y.Z] - YYYY-MM-DD

### Release Title Here

Brief 1-2 sentence description of the release theme.

#### Added
- **Bold lead**: Description of new feature

#### Fixed
- **Bold lead**: Description of bug fix

#### Changed
- **Bold lead**: Description of change
```

Only include sections (Added/Fixed/Changed/Removed) that apply. This text is extracted by CI for the GitHub Release body.

### com.aeroftp.AeroFTP.metainfo.xml
Add a new `<release>` entry at the top of the `<releases>` section:

```xml
<release version="X.Y.Z" date="YYYY-MM-DD">
  <description>
    <p>Release Title — brief summary.</p>
  </description>
</release>
```

---

## Phase 3 — Documentation (5+ files)

ALL documentation MUST be updated BEFORE the tag. Missing docs = tag recreation = wasted CI time.

1. **`SECURITY.md`** — Update footer `*AeroFTP vX.Y.Z - DD Month YYYY*`
2. **`docs/README.md`** — Update `Documentation Version` and `Last Update` fields
3. **`docs/PROTOCOL-FEATURES.md`** — Update `Last Updated` and `Version` fields
4. **`README.md`** — Update if new providers or significant features added (protocol table, CLI protocols list)
5. **`ROADMAP.md`** — Update "Recently Shipped" section if present
6. **`docs.aeroftp.app`** (separate repo) — Update `about.md` (provider list) and `features/aerocloud.md` (sync matrix) if new provider added

**WARNING: CLAUDE.md is gitignored — do NOT stage or commit it.** Update for local reference only.

---

## Phase 4 — Validation

Run these checks sequentially:

```bash
cd src-tauri && cargo clippy --all-targets -- -D warnings
```

```bash
npm run build
```

```bash
npm run i18n:validate
```

Report results. If clippy or build fails, fix before proceeding.

---

## Phase 5 — Commit & Tag

**STOP HERE.** Show a summary of all changes and ask:

> "Pronto per il commit e il tag vX.Y.Z?"

Only proceed after explicit user confirmation. Then:

1. Stage all modified files (list them explicitly, no `git add -A`)
2. Commit: `chore(release): vX.Y.Z Release Title`
3. Push: `git push origin main`
4. Tag: `git tag vX.Y.Z`
5. Push tag: `git push origin vX.Y.Z`

**NEVER push or tag without explicit user approval in Phase 5 prompt.**

---

## Phase 6 — AUR Publish (after user confirms CI is green)

**Do NOT poll CI or wait.** The user will tell you when GitHub Actions are green. Then:

1. Download the AppImage from the GitHub Release:
   ```bash
   curl -L -o /tmp/aeroftp.AppImage "https://github.com/axpdev-lab/aeroftp/releases/download/vX.Y.Z/AeroFTP_X.Y.Z_amd64.AppImage"
   ```

2. Compute sha256sums for ALL 3 sources:
   ```bash
   sha256sum /tmp/aeroftp.AppImage
   ```
   The `.desktop` and `.png` sha256sums come from the repo files (`aur/aeroftp.desktop`, icon URL).

3. Update **`aur/PKGBUILD`**:
   - `pkgver=X.Y.Z`
   - `sha256sums=('APPIMAGE_HASH' 'DESKTOP_HASH' 'ICON_HASH')`

4. Update **`aur/.SRCINFO`**:
   - `pkgver = X.Y.Z` (3 occurrences: pkgver, source URL, noextract)
   - `sha256sums = ...` for all 3 sources

5. Clone AUR repo, copy files, commit and push:
   ```bash
   git clone ssh://aur@aur.archlinux.org/aeroftp-bin.git /tmp/aur-aeroftp
   cp aur/PKGBUILD aur/.SRCINFO aur/aeroftp.desktop /tmp/aur-aeroftp/
   cd /tmp/aur-aeroftp && git add -A && git commit -m "Update to vX.Y.Z"
   git push
   ```

6. Commit updated `aur/PKGBUILD` + `aur/.SRCINFO` back to the main repo

7. Edit GitHub Release title to **"AeroFTP vX.Y.Z"** if CI set it differently

---

## Rules

- **NO EMOJIS** in commit messages or changelog
- Imperative mood in changelog ("add" not "added")
- Keep commit first line under 72 chars
- Never skip clippy — CI uses `-D warnings`
- AUR .SRCINFO has 3 version occurrences — update ALL
- `public/splash.html` version is hardcoded (Tauri IPC not available)

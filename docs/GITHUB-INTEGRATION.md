# AeroFTP × GitHub Integration

> Browse repositories as filesystems. Upload files that become commits. Connect with Device Flow, PAT, or GitHub App installation tokens.

---

## Overview

AeroFTP treats GitHub repositories as remote filesystems. Every repository write operation — upload, rename, delete, folder creation — produces a real Git commit. You can browse code, manage release assets, and work with branches from the same interface you use for FTP, SFTP, S3, WebDAV, and the rest of AeroFTP's supported providers.

This is not a Git client. AeroFTP does not clone repositories, manage staging areas, or handle merge conflicts. It is a file manager that happens to speak the GitHub API, making repository content accessible through the same workflow you already use for every other server.

The desktop application is the primary GitHub integration surface. Separately, the official AeroFTP website also uses GitHub-backed flows for public release downloads and OAuth-based issue submission. Those website flows are documented below as separate surfaces so reviewers can distinguish them from the desktop repository-access integration.

---

## Official Website Surfaces

In addition to the desktop application, AeroFTP uses GitHub on the official website in two limited ways.

### 1. Download Page

Page: <https://www.aeroftp.app/page/download>

- Exposes public release assets published on GitHub Releases
- Links users to official release binaries hosted on GitHub
- Surfaces release metadata such as version, date, asset count, and download links
- Uses GitHub as the canonical release distribution source for desktop builds

This website flow is read-only from the user's perspective and does not grant the site write access to the user's repositories.

### 2. Report Issue Page

Page: <https://www.aeroftp.app/page/report-issue>

- Lets a user sign in with GitHub to report bugs, request features, or ask questions
- Submits issues directly to the AeroFTP GitHub repository
- Uses a website OAuth flow that is separate from the desktop application's repository-access integration
- Public page text states that it requests permission to create issues and read the user's profile

This website flow should be considered a separate GitHub surface from the desktop app's repository browsing and commit workflow.

---

## What You Can Do

### Repository as Filesystem

Navigate any GitHub repository as if it were a remote directory:

- **Browse** — list files and folders, navigate into subdirectories, view file metadata
- **Read** — open files directly, preview content, download to local disk
- **Upload** — drag a file from local to remote, a Git commit is created automatically
- **Delete** — remove a file, a Git commit records the deletion
- **Rename / Move** — rename or move files between directories, each operation is a commit
- **Create folders** — directories are created with a `.gitkeep` placeholder (Git does not track empty directories)
- **Batch-friendly commit prompts** — in the GUI, multi-upload and multi-delete flows ask once for a commit message and reuse it across the batch
- **Search** — find files by name pattern across the entire repository tree
- **Tree view** — visualize the repository structure

### Release Asset Management

GitHub Releases appear as virtual directories. Release assets (binaries, packages, archives) are files within them:

- Browse releases as folders
- Download release assets (up to 2 GiB per asset)
- Upload new assets to existing releases
- Delete release assets or entire existing releases

In AeroFTP, releases are exposed through the virtual directory `/.github-releases/`.

### Branch Awareness

AeroFTP detects whether a branch is writable:

- **Direct push** — the branch accepts commits directly. Your changes are committed immediately.
- **Protected branch** — the branch has protection rules. AeroFTP automatically creates a working branch (`aeroftp/{user}/{base-branch}`) and commits there instead of failing or pretending direct push is possible.
- **Read-only** — the token does not have write access. You can browse and download, but not modify.

The UI also exposes the current GitHub write mode and active branch so it is clear when you are writing directly, writing to a working branch, or browsing in read-only mode.

### From the CLI

Every operation is available from the command line:

```bash
# List repository contents
aeroftp ls --profile "My GitHub Repo" /src/ -l

# Upload a file (creates a commit)
aeroftp put --profile "My GitHub Repo" ./fix.py /src/fix.py

# Download a file
aeroftp get --profile "My GitHub Repo" /README.md ./

# Delete a file (creates a commit)
aeroftp rm --profile "My GitHub Repo" /old-file.txt

# Directory tree
aeroftp tree --profile "My GitHub Repo" /src/ -d 3
```

---

## Authentication

The integration supports three authentication modes.

### 1. Device Flow

Used for the official AeroFTP GitHub App authorization flow.

- The user completes authorization in the browser
- access tokens are acquired in Rust and held backend-side
- after authentication, the token is not returned via Tauri IPC
- best for: personal use and normal day-to-day repository access

### 2. Fine-Grained PAT

Generate a Fine-grained PAT at [github.com/settings/personal-access-tokens](https://github.com/settings/personal-access-tokens/new) and paste it in AeroFTP.

Required permissions:

- **Contents**: Read and write
- **Metadata**: Read-only (automatic)

Current handling:

- The user pastes a PAT manually
- the PAT is stored in the local encrypted vault
- on reuse, the PAT is loaded backend-side and consumed only at connection time

### 3. GitHub App Installation Token (.pem)

Create your own GitHub App, install it on the target account or organization, and import its private key into AeroFTP.

Current handling:

- The user imports a GitHub App private key
- the PEM is encrypted in the local vault after import
- AeroFTP generates short-lived installation tokens in Rust only
- installation tokens are held and consumed backend-side only

**Setup:**

1. Go to [github.com/settings/apps/new](https://github.com/settings/apps/new)
2. Create an app with your desired name and logo
3. Set permissions: Contents (Read & Write), Metadata (Read)
4. Install the app on your account or organization
5. Generate a private key (.pem file)
6. In AeroFTP: select "App (.pem)" authentication mode
7. Enter your App ID and Installation ID
8. Import the .pem file

**Current behavior:**

Installation-token mode is supported and audited, but commit attribution is implementation-specific:

- AeroFTP sets commit author and committer fields to the repository owner identity for content writes
- AeroFTP appends a `Co-authored-by: aeroftp[bot]` trailer in bot mode so the bot identity is visible in commit metadata
- the exact avatar and commit presentation remain subject to GitHub's UI rules

- Authentication identity: your GitHub App installation token
- Token management: automatic (1-hour tokens generated from .pem on demand)
- PEM key storage: encrypted in vault (AES-256-GCM) after first import — original .pem file can be deleted
- Token expiry: dynamic badge shows valid/expiring/expired state with auto-refresh on connect

If you need exact reviewer validation of how your app appears in the GitHub web UI, use a test repository and inspect the resulting commit metadata directly on GitHub.

---

## Credential Isolation

AeroFTP's GitHub integration follows the same credential isolation architecture as all other protocols. After initial entry or authorization, credentials are stored in the local AeroFTP vault and resolved inside the Rust backend process.

Security-sensitive details relevant to reviewers:

- Device Flow tokens are obtained in Rust and then held backend-side
- stored PATs are loaded from the encrypted vault and moved into backend-held state only for connection use
- installation tokens derived from PEM files are minted and consumed entirely in Rust
- imported PEM material is encrypted in the vault after first import
- tokens are not returned to frontend JavaScript after authentication completes

When an AI coding agent uses `--profile "GitHub"` from the CLI, it never sees the token. It receives only the operation result: a directory listing, a commit confirmation, a file download.

```bash
# AI agent commits code — zero credentials exposed
aeroftp put --profile "GitHub/myproject" ./fix.py /src/fix.py
```

Read more: [Credential Isolation for AI Agents](CREDENTIAL-ISOLATION.md)

---

## Technical Details

### API Usage

Current public GitHub flows in AeroFTP are built primarily on the GitHub REST API v3. The provider also contains GraphQL foundations for future atomic multi-file commit workflows.

| Operation | API |
| --------- | --- |
| List files | `GET /repos/{owner}/{repo}/contents/{path}` |
| Download | Raw media type on Contents API |
| Upload (commit) | `PUT /repos/{owner}/{repo}/contents/{path}` |
| Delete (commit) | `DELETE /repos/{owner}/{repo}/contents/{path}` |
| Releases | `GET/POST /repos/{owner}/{repo}/releases` |
| Release assets | Upload as raw binary stream |
| Branches | `GET/POST /repos/{owner}/{repo}/branches` |
| Pull requests | `GET/POST /repos/{owner}/{repo}/pulls` |
| Pages | `GET/POST/PUT /repos/{owner}/{repo}/pages` |
| Actions runs | `GET /repos/{owner}/{repo}/actions/runs` |

### Rate Limits

GitHub API allows 5,000 authenticated requests per hour for standard authenticated traffic. AeroFTP tracks rate-limit state from GitHub response headers and exposes the remaining quota in connection information and CLI output.

Typical usage per operation:

- List directory: 1 request
- Upload file: 2 requests (check existing + create/update)
- Download: 1 request

### File Size Limits

| Context | Limit |
| ------- | ----- |
| Repository files (read) | 100 MiB |
| Repository files (write) | 100 MiB |
| Release assets | 2 GiB |

Files larger than 100 MiB cannot be stored in GitHub repositories. Use Release assets for large binaries.

### Commit Identity

| Authentication | Author | Committer | Avatar |
| -------------- | ------ | --------- | ------ |
| PAT | Authenticated user | Authenticated user | User's avatar |
| Device Flow | Authenticated user | Authenticated user | User's avatar |
| Installation token (.pem) | Repository owner identity | Repository owner identity | GitHub UI-dependent |

The final presentation on GitHub is a combination of the token type, the explicit author/committer fields sent by AeroFTP, and GitHub's own UI rendering. Reviewers should treat the table above as the current implementation behavior, not a promise about how every GitHub surface will render avatars.

---

## GitHub App: AeroFTP

AeroFTP's official GitHub App is available at [github.com/apps/aeroftp](https://github.com/apps/aeroftp).

When you click "Authorize with GitHub" in AeroFTP, this app's client ID is used for Device Flow authorization. The integration currently relies on the following repository permissions:

- **Contents**: Read and write (browse, upload, delete)
- **Metadata**: Read-only (repository info)
- **Pull requests**: Read and write (branch-workflow PR creation)
- **Actions**: Read-only (workflow run listing)
- **Pages**: Read and write (Pages status, rebuild, configuration)

The app does not access your email, profile, or any data outside the repositories you grant access to.

---

## Security and Permission Summary

This section is written for GitHub reviewers evaluating the AeroFTP integration for the Developer Program.

Unless explicitly stated otherwise, the points below describe the desktop application's repository-access integration. The official website download page and issue-reporting page are separate GitHub-backed surfaces with narrower scope.

### What the Integration Does

- Lists repository files and directories
- reads and downloads repository content
- uploads or updates files by creating normal Git commits through the Contents API
- deletes files by creating normal Git commits through the Contents API
- renames or moves files through API-backed content operations
- creates folders by writing `.gitkeep` placeholders
- lists releases and manages release assets
- lists GitHub Actions workflow runs
- reads GitHub Pages status and build history
- triggers Pages rebuilds and updates Pages source configuration
- creates pull requests when protected-branch workflow mode is active

### What the Integration Does Not Do

- Does not request email or profile scopes
- does not read user data outside repositories explicitly granted by the user
- does not clone repositories or access local Git credentials
- does not require or expose raw GitHub tokens to AI agents using AeroFTP profiles
- does not send GitHub credentials to frontend JavaScript after authentication is complete

For clarity: the official website's issue-reporting page is a separate OAuth flow and is not the same permission surface as the desktop repository integration described above.

### Credential Handling

- Tokens and imported PEM material are stored in the local AeroFTP vault
- the local vault is encrypted at rest with AES-256-GCM
- Device Flow tokens are held backend-side after authorization
- stored PATs are rehydrated into backend-held state rather than returned to the renderer
- installation tokens are minted and consumed entirely in Rust
- CLI profile-based usage resolves credentials internally; callers receive only operation results

### Requested Repository Permissions

| Permission | Access | Why it is needed |
| ---------- | ------ | ---------------- |
| Contents | Read and write | Required for repository browsing, file upload, file update, file delete, file rename, and folder creation through commit-backed operations |
| Metadata | Read-only | Required to resolve repository identity, repository visibility, and capability information |
| Pull requests | Read and write | Required only for branch-workflow mode when a protected branch requires changes to be proposed through a pull request |
| Actions | Read-only | Required to list workflow runs and display their status in the UI |
| Pages | Read and write | Required to read Pages status, list build history, trigger rebuilds, and update Pages source settings |

### Data Minimization Notes

- The integration is repository-scoped
- it accesses only the repository data necessary to perform the user-requested operation
- it does not request unrelated personal data
- it does not require access to email, contacts, or profile information
- no token value is required by, or exposed to, AI assistant workflows using AeroFTP profiles

The official website download page is limited to public release distribution, and the official website issue-reporting page is limited to user-initiated issue submission.

For implementation references, see the source table below.

---

## Implementation

AeroFTP is open source. The GitHub provider implementation is fully auditable:

| Component | Source |
| --------- | ------ |
| Provider core | [src-tauri/src/providers/github/mod.rs](../src-tauri/src/providers/github/mod.rs) |
| HTTP client | [src-tauri/src/providers/github/client.rs](../src-tauri/src/providers/github/client.rs) |
| Repository operations | [src-tauri/src/providers/github/repo_mode.rs](../src-tauri/src/providers/github/repo_mode.rs) |
| GraphQL batch-commit foundations | [src-tauri/src/providers/github/graphql.rs](../src-tauri/src/providers/github/graphql.rs) |
| Release assets | [src-tauri/src/providers/github/releases_mode.rs](../src-tauri/src/providers/github/releases_mode.rs) |
| Authentication | [src-tauri/src/providers/github/auth.rs](../src-tauri/src/providers/github/auth.rs) |
| Error handling | [src-tauri/src/providers/github/errors.rs](../src-tauri/src/providers/github/errors.rs) |
| CLI integration | [src-tauri/src/bin/aeroftp_cli.rs](../src-tauri/src/bin/aeroftp_cli.rs) |

The implementation was cross-reviewed during the March 2026 GitHub remediation cycle, with critical findings resolved before publication of this document.

---

*AeroFTP — [github.com/axpdev-lab/aeroftp](https://github.com/axpdev-lab/aeroftp) — GPL-3.0*

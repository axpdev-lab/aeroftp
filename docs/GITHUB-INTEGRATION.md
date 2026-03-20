# AeroFTP × GitHub Integration

> Browse repositories as filesystems. Upload files that become commits. Use your own GitHub App identity for bot-style commit attribution.

---

## Overview

AeroFTP treats GitHub repositories as remote filesystems. Every repository write operation — upload, rename, delete, folder creation — produces a real Git commit. You can browse code, manage release assets, and work with branches from the same interface you use for FTP, SFTP, S3, WebDAV, and the rest of AeroFTP's supported providers.

This is not a Git client. AeroFTP does not clone repositories, manage staging areas, or handle merge conflicts. It is a file manager that happens to speak the GitHub API, making repository content accessible through the same workflow you already use for every other server.

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

Three ways to connect, each suited to different workflows.

### 1. Authorize with GitHub (recommended)

One-click browser authorization. AeroFTP opens your browser, you authorize the app, and the token is saved in the encrypted vault.

- Best for: personal use, daily development
- Commit identity: your GitHub username and avatar
- Token management: automatic, stored in vault

### 2. Personal Access Token

Generate a Fine-grained PAT at [github.com/settings/personal-access-tokens](https://github.com/settings/personal-access-tokens/new) and paste it in AeroFTP.

Required permissions:

- **Contents**: Read and write
- **Metadata**: Read-only (automatic)

- Best for: automation, CI/CD, scripting
- Commit identity: your GitHub username and avatar
- Token management: manual, you control expiry

### 3. GitHub App with Custom Identity (.pem)

Create your own GitHub App with a custom name and logo. Commits made through AeroFTP will show your app's identity and avatar in the repository's contributor list.

**Setup:**

1. Go to [github.com/settings/apps/new](https://github.com/settings/apps/new)
2. Create an app with your desired name and logo
3. Set permissions: Contents (Read & Write), Metadata (Read)
4. Install the app on your account or organization
5. Generate a private key (.pem file)
6. In AeroFTP: select "App (.pem)" authentication mode
7. Enter your App ID and Installation ID
8. Import the .pem file

**Result:**

Your commits will show your app's name with a `[bot]` suffix and your custom logo in the repository's contributor list. This is the same GitHub installation-token attribution model used by GitHub-native automations.

- Best for: teams, branded automation, CI/CD bots, open-source projects
- Commit identity: `yourapp[bot]` with your custom logo
- Token management: automatic (1-hour tokens generated from .pem on demand)
- PEM key storage: encrypted in vault (AES-256-GCM) after first import — original .pem file can be deleted
- Token expiry: dynamic badge shows valid/expiring/expired state with auto-refresh on connect
- Co-authoring: commits show **both** the human user (author) and `aeroftp[bot]` (committer) with dual avatars on GitHub

**Example:**

If you create a GitHub App called "DeployBot" with a rocket logo, your commits will appear as:

```text
🚀 deploybot[bot]  Create index.html    2 minutes ago
🚀 deploybot[bot]  Update styles.css     5 minutes ago
👤 axpnet          Fix typo in README    1 hour ago
```

Your app appears in the repository's Contributors section alongside human contributors.

---

## Credential Isolation

AeroFTP's GitHub integration follows the same credential isolation architecture as all other protocols. The token — whether PAT, Device Flow, or installation token — is stored in the AES-256-GCM encrypted vault and resolved exclusively inside the Rust backend process.

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
| Pull request helper foundation | `GET/POST /repos/{owner}/{repo}/pulls` |

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
| Installation token (.pem) | Repository owner | `yourapp[bot]` | Both avatars (user + app) |

The commit identity is determined by GitHub based on the token used, not by AeroFTP. Installation tokens produce bot-attributed commits with the app's logo because they represent the app itself, not a user.

---

## GitHub App: AeroFTP

AeroFTP's official GitHub App is available at [github.com/apps/aeroftp](https://github.com/apps/aeroftp).

When you click "Authorize with GitHub" in AeroFTP, this is the app that handles the authorization. It requests only the permissions needed for file operations:

- **Contents**: Read and write (browse, upload, delete)
- **Metadata**: Read-only (repository info)
- **Pull requests**: Read and write (create PRs from branch workflow)
- **Issues**: Read and write (future)
- **Actions**: Read-only (future)
- **Pages**: Read and write (future)

The app does not access your email, profile, or any data outside the repositories you grant access to.

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

The implementation has been reviewed by multiple independent auditors (Claude Opus 4.6 and GPT 5.4) with all critical findings resolved.

---

*AeroFTP — [github.com/axpnet/aeroftp](https://github.com/axpnet/aeroftp) — GPL-3.0*

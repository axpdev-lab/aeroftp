# Contributing to AeroFTP

> _Last updated: 2026-09-04_

First off, thank you for considering contributing to AeroFTP!

## Code of Conduct

Be respectful, inclusive, and professional. We're here to build great software together.

## How Can I Contribute?

### Reporting Bugs

- Use the issue tracker
- Include steps to reproduce
- Describe expected vs actual behavior
- Include screenshots if relevant

### Suggesting Features

- Check if the feature was already requested
- Describe the use case clearly
- Explain why this would be useful

### Pull Requests

1. Fork the repo
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Run the local checks in [Development and Pre-Push Gates](#development-and-pre-push-gates)
5. Commit **with a sign-off** (`git commit -s -m 'Add amazing feature'`), see [Sign-off](#sign-off-developer-certificate-of-origin)
6. Push (`git push origin feature/amazing-feature`)
7. Open a Pull Request

## Development and Pre-Push Gates

Owner, 2026-09-04. For a feature/fix PR, GitHub Actions is the green. Do not
also run a full local `npm run ci:pre-push` (~45 minutes of Clippy + the
complete Rust suite) on a PR branch: that duplicates CI.

`checks.yml` is not the full PR green. Clippy, Rust tests, `tsc`,
`i18n:validate`, and the security-regression suite live in `build.yml` on
Linux. Merge only when those jobs are actually green, not when Checks and
DCO alone are green.

How to drive the product from an agent (profiles, `ls` / `get` / `put`, no
credentials) is `AGENTS.md`. That file is not this gate.

### PR branches

Locally, before push, run the short set for what you touched:

- `cargo fmt --all -- --check` in `src-tauri`
- `npx tsc --noEmit` if TypeScript changed
- `npm run i18n:validate` if locales changed
- focused vitest / `cargo test --lib <filter>` for the files you touched

That is about 40 seconds. Do not start a second full Cargo/Tauri compile while
another worktree is already compiling.

### Direct `main`, release, pre-tag

Full `npm run ci:pre-push` remains required for:

- a push or merge directly to `main`
- a release or pre-tag
- CI cannot run, or the change is in a platform-specific area CI does not cover

Do not push while that command is red. If a required gate cannot run, stop and
report the blocker.

## Development Setup

### Prerequisites

Core toolchain (all platforms):

- **Node.js 20+** and npm (CI builds on Node 20).
- **Rust** stable toolchain via [rustup](https://rustup.rs/) (minimum 1.85.0).
- A **C/C++ compiler toolchain** and **Perl**: two native dependencies build
  from source. `ssh2` vendors OpenSSL (needs `perl` on `PATH`) and
  `whisper-rs-sys` vendors whisper.cpp and runs `bindgen` (needs **libclang**
  from LLVM). On most Linux/macOS setups Perl is already present; on Windows it
  is not, so it must be installed explicitly.

Platform-specific native build tools:

- **Windows**:
  - [Strawberry Perl](https://strawberryperl.com/) (fixes
    `openssl-sys ... Command 'perl' not found`).
  - [LLVM](https://github.com/llvm/llvm-project/releases) for `clang.dll` /
    `libclang.dll` (fixes `whisper-rs-sys ... Unable to find libclang`). If the
    build still cannot find it, set `LIBCLANG_PATH` to the LLVM `bin` folder
    (e.g. `C:\Program Files\LLVM\bin`).
  - Visual Studio Build Tools 2022 (MSVC, "Desktop development with C++").
  - WebView2 Runtime (preinstalled on Windows 11 and recent Windows 10).
  - The build links the static MSVC runtime (`+crt-static`), and
    `src-tauri/.cargo/config.toml` forces whisper.cpp to the static runtime too
    so the speech feature links cleanly (GitHub #344). A fresh checkout needs
    nothing extra. If you have a tree that ALREADY built whisper before this fix
    landed, run `cargo clean -p whisper-rs-sys` once after pulling, then rebuild,
    otherwise the cached dynamic-runtime objects re-trigger the `LNK2038` /
    `LNK1120` link error. As a fallback you can always build without the speech
    stack: `cargo build --no-default-features --features aerorsync`.
- **Linux** (Debian/Ubuntu):
  ```bash
  sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev \
    librsvg2-dev patchelf libfuse3-dev
  ```
  Optional (MTP / portable devices, APPENDIX-MTP):
  ```bash
  sudo apt-get install -y libmtp-dev libmtp9t64
  ```
  `pkg-config libmtp` must succeed for the Linux libmtp backend to link. Without
  it the app still builds and runs; portable-device discovery returns an empty
  list and transfer ops report that the MTP backend is not linked. Set
  `AEROFTP_DISABLE_LIBMTP=1` to force the Null backend even when libmtp is
  installed. Runtime package `libmtp9` / `libmtp9t64` is required on end-user
  systems that use the linked backend.
- **macOS**: Xcode Command Line Tools (`xcode-select --install`).

> First build note: `npm run tauri dev` compiles the full Rust dependency tree,
> including OpenSSL and whisper.cpp from source. The first run can take many
> minutes even on a fast CPU and looks like it has stalled. This is expected;
> let it finish. Subsequent builds are incremental and fast.

```bash
# Clone the repo
git clone https://github.com/axpdev-lab/aeroftp.git
cd aeroftp

# Install dependencies
npm install

# Run in development mode
npm run tauri dev

# Build
npm run tauri build
```

## Code Style

- Use TypeScript for frontend code
- Use Rust for backend (Tauri) code
- Follow existing code patterns
- Add comments for complex logic
- Keep functions small and focused

## Commit Messages

- Use clear, descriptive messages
- Start with a verb (Add, Fix, Update, Remove)
- Reference issues when relevant (#123)

## Sign-off (Developer Certificate of Origin)

Every commit must carry a `Signed-off-by` line. Adding one is a single flag:

```bash
git commit -s -m "fix(webdav): handle 405 on root PROPFIND"
```

which appends:

```
Signed-off-by: Your Name <your.email@example.com>
```

The name and email must match the commit author. If you forget, `git commit --amend -s --no-edit` fixes the last commit and `git rebase --signoff <base>` fixes a whole branch. A CI check enforces this on every pull request.

The sign-off certifies the [Developer Certificate of Origin](DCO) 1.1: in short, that you wrote the contribution yourself, or that you have the right to submit it under the project's licence. It is a statement about the origin of the code, not a transfer of anything: **you keep the copyright in your contribution**.

AeroFTP deliberately uses a DCO rather than a Contributor Licence Agreement. A CLA would ask you to grant rights beyond the project's licence, and that is not something this project needs from you.

## Test Requirements

All pull requests should include tests for new features and bug fixes where applicable:

- **Backend (Rust)**: Add unit tests in `#[cfg(test)]` modules. Run with `cargo test` from the `src-tauri/` directory.
- **Security checks**: Run `npm run security:regression` to verify security invariants.
- **i18n**: Run `npm run i18n:validate` to ensure every translation key in the English reference (`en.json`) is present in all 46 translation locales (the tool reports `46/46`).
- **Type checking**: Run `npx tsc --noEmit` to verify TypeScript types.

Pull requests that reduce test coverage or break existing tests will not be merged.

## Response Times

- **Bug reports**: We aim to acknowledge bug reports within 7 days.
- **Security vulnerabilities**: We respond within 48 hours (see [SECURITY.md](SECURITY.md) for details).
- **Pull requests**: We aim to review pull requests within 14 days.

## Questions?

Open a discussion or reach out to the maintainers.

Thank you for contributing!

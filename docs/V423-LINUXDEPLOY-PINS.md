# Linux packaging helper verification (requested 28)

Rechecked 2026-10-07 against `origin/main` `1ac52cceb`, tracker
[#1081](https://github.com/axpdev-lab/aeroftp/issues/1081) and
[#1072](https://github.com/axpdev-lab/aeroftp/pull/1072). The earlier triage was
a snapshot: main still had unchecked linuxdeploy helper downloads.

## Actual bundler contract

`package-lock.json` installs Tauri CLI **2.11.2**, which embeds
`tauri-bundler` **2.9.2** (the application's Cargo.lock does not select this
CLI-side crate). Its bundled implementation is
[linuxdeploy.rs at 499df79](https://github.com/tauri-apps/tauri/blob/499df79be65ef8c0670abc0207cd9e37b55d8491/crates/tauri-bundler/src/bundle/linux/appimage/linuxdeploy.rs).
`prepare_tools` downloads five helpers only when their cache names are absent;
it does not verify existing or downloaded content. AppRun and linuxdeploy come
from Tauri binary-releases, GTK and GStreamer from `master`, and the AppImage
plugin from `continuous`. An absent AppImage plugin can fall back to a bundled
copy. GTK is always invoked; GStreamer is invoked when `bundleMediaFramework`
is enabled, but is downloaded regardless.

[useLocalToolsDir](https://v2.tauri.app/reference/config/#bundleconfig) selects
Cargo's `target_directory/.tauri`, rather than the user's shared cache. The
[CLI interface](https://github.com/tauri-apps/tauri/blob/499df79be65ef8c0670abc0207cd9e37b55d8491/crates/tauri-cli/src/interface/mod.rs)
uses `cargo metadata` for that path, including `CARGO_TARGET_DIR`/Cargo config.
The [bundle hook](https://github.com/tauri-apps/tauri/blob/499df79be65ef8c0670abc0207cd9e37b55d8491/crates/tauri-cli/src/bundle.rs)
runs before either `tauri build` or `tauri bundle` packages the application.

The Linux platform config enables that directory. The existing dispatcher hook
calls `scripts/prepare-linuxdeploy.cjs` after staging its binaries. The helper
gate checks the installed and locked CLI versions, the architecture and the
local-cache setting. Upgrading the CLI requires rechecking the download names,
locations, transformations and discovery rules; an unreviewed version fails.
Only x86_64 has pins, matching the Linux release matrix. Other Linux targets
need their own reviewed helper digests before packaging.

## Pins and provenance

The authoritative URL/hash list is `scripts/linuxdeploy-pins.json`.

| Cache name | Pin | SHA-256 |
|---|---|---|
| `linuxdeploy-plugin-appimage.AppImage` | [1-alpha-20250213-1](https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/tag/1-alpha-20250213-1), source `a96502d15ce19ec1df3d46e6cbe87ddf38589a71`, asset ID 228937581 | `992d502a248e14ab185448ddf6f6e7d25558cb84d4623c354c3af350c25fccb3` |
| `linuxdeploy-plugin-gtk.sh` | [dda522bce37387f1b853d9095713bfaa924c8423](https://github.com/tauri-apps/linuxdeploy-plugin-gtk/commit/dda522bce37387f1b853d9095713bfaa924c8423) | `7804c9eef13e59bf2783aad9882ef9db8f3f3f9e8d631874b1d348d550a3693f` |
| `linuxdeploy-plugin-gstreamer.sh` | [2a2e67491c32995a3f279ad0ecbe77abd512b42a](https://github.com/tauri-apps/linuxdeploy-plugin-gstreamer/commit/2a2e67491c32995a3f279ad0ecbe77abd512b42a) | `c107b49d84edbffc6ab226ed1007e0626a4f7aa2c3a36b7782bef62351d49e94` |
| `AppRun-x86_64` | [apprun-old](https://github.com/tauri-apps/binary-releases/releases/tag/apprun-old), asset ID 274691722, matches GitHub's asset digest | `f30140a43a0a59e46db21bdefdf749b9e9f2c6946e92afabbacf98b8ae73fb4f` |
| `linuxdeploy-x86_64.AppImage` | [linuxdeploy](https://github.com/tauri-apps/binary-releases/releases/tag/linuxdeploy), asset ID 182515537 (2024-07-29) | `e762bea85c8eb0d4b3508d46e5c1f037f717d0f9303ae3b4aafc8b04991fa1ef` |

The AppImage plugin has a dated release, so the rolling June 2026 `continuous`
asset is unnecessary. GitHub publishes no digest for this older plugin or
linuxdeploy asset: their hashes were calculated from downloads of those
official release assets. The GTK/GStreamer URLs use full commit IDs. Tauri's
linuxdeploy and AppRun release labels are not immutable; a replaced asset will
fail the committed digest instead of being accepted. No claim of an upstream
signature or reproducible source build is made by this check.

Tauri zeroes exactly bytes 8..10 of linuxdeploy with `dd`. A warm cache may
therefore match the original digest above or the full transformed digest
`20eebde3c18ae2e44279bd624fc72482503aece216d5d77f10932235342f71c1`.
Downloads must match the original. Verification never masks bytes to make an
otherwise modified file pass.

## Failure behavior

- Empty cache: download to non-executable temporary files, verify, chmod 0700,
  then atomically rename into the owner-writable-only local cache. Interrupted
  downloads leave no partial helper to be trusted on the next attempt.
- Existing cache: rehash every helper before downloading any missing member.
  A bad checksum is fatal and the bad file remains available for inspection.
  It is not silently deleted or repaired. Network failures alone are retried.
- Require all five: no optional AppImage-plugin fallback. `--verify` also
  refuses missing members without fetching, and CI runs it after packaging.
- Reject symlinks/hardlinks and unknown cache entries. Reject external
  `linuxdeploy-plugin-*` files in PATH and the bundler's working directories:
  [linuxdeploy discovery](https://github.com/linuxdeploy/linuxdeploy/blob/659c9db374666be3f29432316a859221eac28e53/src/plugin/plugin.cpp)
  executes API probes even for a duplicate plugin it will not select.

The bundled tools inside the verified AppImages are covered by the parent
artifact's digest. System build tools/libraries remain supplied by the build
environment. This gate is a download/cache integrity contract, not a sandbox
against another process running as the build user and changing files after
verification. The shared `~/.cache/tauri` is not used or modified.

To investigate a mismatch, preserve the file and compare its hash and official
provenance first. Remove a reviewed obsolete cache entry only after that
investigation; never update a pin to whichever bytes happened to arrive.

## Requested 27: still upstream-dependent

The [type2-runtime release API](https://api.github.com/repos/AppImage/type2-runtime/releases)
was checked again on 2026-10-07. It still lists `continuous`, `20251108` and
`old`. `continuous` targets
[`8f39b89e2ac31e1640b3d3f7e9a5108e6ce805fa`](https://github.com/AppImage/type2-runtime/commit/8f39b89e2ac31e1640b3d3f7e9a5108e6ce805fa),
the 0700 extraction-directory fix. The dated `20251108` targets
`dd6cebedcbddde9c82f89b011e8e1d40b6e43868`, before that fix. No qualifying
dated release exists. The final AppImage runtime URL and digest in `build.yml`
remain unchanged, and requested **27 stays open**. This helper change does not
claim to close the runtime extraction-permission gap.

## Verification

`node --test .github/scripts/test-linuxdeploy-pins.cjs` checks empty/warm cache,
each corrupted helper, incorrect and interrupted downloads, absent AppImage
plugin, unexpected cache/PATH plugins, symlinks/hardlinks, unsafe cache
permissions, the exact linuxdeploy transformation, unsupported architecture,
CLI drift and a disabled local-cache override. Checks runs this suite on PRs.

The 17 local regression tests pass. In a temporary copy, disabling the digest
comparison makes seven tests fail (the five corrupt-cache cases, bad download
and linuxdeploy transformation). Real packaging, launch and complete CI results
are recorded in the pull request evidence. The PR's complete multi-platform CI
remains the authoritative PR gate; the full local gate is required before an
authorized merge to main.

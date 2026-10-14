# Windows associations and installer templates

Issue #454 is implemented through two independent choices: AeroFTP's five file
formats, and the eight general archive formats plus Explorer extract menus.
Historical NSIS/MSI ProgID aliases remain registered while enabled so an existing
Windows default choice still resolves after an upgrade. Registration is additive. Neither installer nor Settings writes `UserChoice` or
replaces a foreign extension default. Windows Default apps remains the route to
choose a default handler.

`association-registry.json` is the registration catalog consumed by the Rust
backend. After changing it, run `node scripts/generate-windows-associations.mjs`.
This regenerates the NSIS macros and WiX registry features. The unit suite checks
that the outputs are current and cover every format in `tauri.conf.json`.

## NSIS

The setup has an explicit association page. Normal, silent, manual reinstall and
`/UPDATE` installs use `HKCU\Software\AeroFTP\Settings` DWORD values `OwnedFormats`
and `ArchiveFormats` (0/1). Missing values preserve the historical enabled
initial choice; malformed values are disabled. `/NOASSOC` explicitly disables
both groups and saves that choice for later updates.

Settings applies the same catalog immediately and journals original raw registry
values for rollback on failure. Disabling removes matching AeroFTP values and
its own OpenWith history, preserves foreign values and protected defaults, and
restores an old non-AeroFTP extension handler from Tauri's backup when present.
Uninstall removes registrations but retains these two integration preferences
so a subsequent reinstall respects the opt-out.

## MSI

The feature-tree page exposes `AeroFTPOwnedAssociations` and
`AeroFTPArchiveAssociations`. Silent initial opt-out is
`msiexec /i AeroFTP.msi AEROFTP_NOASSOC=1 /qn`; `ADDLOCAL` and `REMOVE` can select
individual groups. Opt-out raises the feature level above the default install
level without disabling publication (level 0 would yield UNKNOWN and prevent
Settings maintenance). A single common application component is referenced by
both features; its icon/open command stay until both groups are removed.
Major upgrades migrate the installed feature states.

Settings uses Windows Installer maintenance for these two features, with normal
Windows elevation and cancellation. It verifies the resulting feature states
before reporting success. An explicit installed path takes precedence over a
saved per-user path, and its value is quoted for Windows Installer's parser.
Integration read errors keep the per-format status rows and Default apps action
available, with the diagnostic shown in Settings. Registry components remain MSI-owned. A bounded
pre-Tauri helper cleans the invoking user's own Explorer history on full
uninstall; it never removes protected `UserChoice`. Portable builds offer no
installer-integration mutation.

The Windows packaging CI runs `scripts/test-windows-msi-associations.ps1` against
the built MSI on its disposable hosted runner. It requires fresh NOASSOC feature
states of ABSENT (2), cycles all independent choices, checks the entire catalog
including shared icon/open command, verifies mandatory binaries and a path with
spaces, then uninstalls the exact product. A same-hive foreign registry fixture
must survive. Logs are uploaded even when the check fails. Local execution
requires elevation and `-AllowMachineChanges`, and refuses an existing AeroFTP
installation. This command-line regression supplements the native Settings and
Explorer acceptance tests; it does not certify GUI cancellation or upgrades.

## Upstream template maintenance

The custom templates start from Tauri CLI 2.11.2, under Tauri's Apache-2.0/MIT
licenses (the upstream MIT notice is preserved in `TEMPLATE-LICENSE-MIT.txt`):

- https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.11.2/crates/tauri-bundler/src/bundle/windows/nsis/installer.nsi
- https://github.com/tauri-apps/tauri/blob/tauri-cli-v2.11.2/crates/tauri-bundler/src/bundle/windows/msi/main.wxs

`TEMPLATE-UPSTREAM.json` records the original source hashes. A CLI dependency
upgrade must rebase the customizations on its templates and update that pin;
the contract test deliberately fails until this is done. The NSIS delta adds
choice initialization/page and removes unconditional APP_ASSOCIATE /
APP_UNASSOCIATE. The WiX delta moves associations out of the mandatory binary
component, exposes the two optional features and selects WixUI_FeatureTree.
Keep Tauri's updater, process, WebView2, installation and signing logic intact.

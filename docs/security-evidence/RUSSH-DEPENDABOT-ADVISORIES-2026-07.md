# russh Dependabot advisories - security evidence (2026-07)

Records the three **moderate** Dependabot alerts open on the default branch at the v4.1.6
release audit, why the fix is blocked upstream rather than deferred by choice, and the
reachability analysis behind shipping without it. Investigated 2026-07-25 during the
v4.1.6 pre-release audit; every cargo experiment restored the tree clean.

## Context

`russh` is the SSH transport under the SFTP provider, `ssh_exec`, `ssh_shell`, the host-key
check and the aerorsync russh session transport (6 call sites, 45 references). The app is an
SSH **client** only - it never listens as a server.

Shipped version: **russh 0.61.2** (`src-tauri/Cargo.toml:145`). Unchanged since before
v4.1.5, so these alerts are pre-existing and not a v4.1.6 regression.

## The three advisories

| GHSA | Summary | Side | Reachable from AeroFTP |
|------|---------|------|------------------------|
| GHSA-g9hv-x236-4qp3 | Client wrong-length X25519 `clone_from_slice` panic (pre-auth DoS) | client | **Yes** |
| GHSA-5xvq-cp9x-6p6r | Pre-auth remote panic via all-zero Curve25519 peer public value (`encode_mpint` OOB) | both | **Yes** |
| GHSA-cqjc-rmpq-xprq | Post-auth remote panic via `pty-req` with more than 130 terminal-mode records | server | **No** - AeroFTP runs no SSH server |

All three are **panics, i.e. availability only**. There is no memory-safety escape, no key
disclosure and no authentication bypass in any of the three. Worst realistic case: a user
deliberately connects to a hostile or badly broken SSH server and the transfer task panics.

Vulnerable range `<= 0.62.3`, first patched **0.62.4**.

## Why the bump is blocked: an upstream version conflict, verified by cargo resolution

`russh 0.62.4` is the **latest published version** (`cargo info russh`) - there is no 0.61.x
backport to take instead.

Reproduce with `.github/scripts/check-russh-unblock.sh`. Note that the transcript below is
the resolution failure, which only surfaces once the manifest requirement has been raised to
`^0.62.4`; the script does that in a scratch copy and restores the tree on exit. Running
`cargo update -p russh --precise 0.62.4` against the manifest as committed instead fails on
the *requirement* (`failed to select a version for the requirement russh = "^0.61.2"`) and
never reaches resolution, so it tells you nothing about upstream.

```
$ .github/scripts/check-russh-unblock.sh
error: failed to select a version for `ed25519-dalek`.
    ... required by package `n0-mainline v0.5.0`
    ... which satisfies dependency `n0-mainline = "^0.5"` of package `iroh-mainline-address-lookup v0.4.0`
    ... which satisfies dependency `iroh-mainline-address-lookup = "^0.4"` of package `aeroftp-peer-l0`
    ... which satisfies path dependency `aeroftp-peer-l0` of package `aeroftp`
    versions that meet the requirements `=3.0.0-rc.0` are: 3.0.0-rc.0
    previously selected package `ed25519-dalek v3.0.0`
    ... which satisfies dependency `ed25519-dalek = "^3"` of package `russh v0.62.4`
```

The two requirements are mutually unsatisfiable:

1. `russh 0.62.4` requires `ed25519-dalek = "^3"`. A caret requirement does **not** match a
   pre-release, so this resolves to `3.0.0` final and cannot resolve to `3.0.0-rc.0`.
2. `n0-mainline 0.5.0` requires `ed25519-dalek = "=3.0.0-rc.0"` - an exact pin on the release
   candidate.
3. Cargo puts `3.0.0-rc.0` and `3.0.0` in the same compatibility class, so it will not select
   both. One version of `ed25519-dalek` must serve the whole graph.

Both upstreams are already at their newest release: `n0-mainline` 0.5.0 and
`iroh-mainline-address-lookup` 0.4.0 (`cargo search`). There is no newer version to bump into.

Worth knowing when judging how long this will last: `n0-mainline` 0.5.0 was published
2026-06-15, *before* `ed25519-dalek` 3.0.0-rc.1 (2026-06-18) and 3.0.0 final (2026-07-06)
existed. Its exact pin is not a deliberate rejection of the final release, it is a pin that
upstream has not revisited since the final shipped. Re-checked 2026-07-25: unchanged.

### Paths deliberately NOT taken

- **Dropping `aeroftp-peer-l0`.** Rejected: WI-3d promoted it from `[dev-dependencies]` to a
  normal dependency so the iroh P2P drive engine ships in the binary (`src/peer/`). Removing it
  removes AeroShare P2P from the build - a functional regression far larger than the alerts.
- **Making `iroh-mainline-address-lookup` optional and default-off.** Rejected: it is the
  Mainline-DHT discovery backend (WI-5a), the decentralized-discovery independence move.
  Silently disabling it trades a crash-on-hostile-server for a real loss of discovery.
- **`[patch.crates-io]` onto a forked `n0-mainline` with the pin relaxed to `^3.0.0-rc.0`.**
  This is the only route that actually lands the bump, and it is the recommended one - but it
  introduces a forked git dependency into a *public release* build, which also has to be
  carried through `cargo-sources.json` (flatpak), the snap build and the AUR `PKGBUILD`.
  Deferred to a reviewed change on its own branch, not folded into a release tag.

## Ruling for v4.1.6

**Ship.** Two of the three are reachable, all three are availability-only panics, they require
the user to connect to an attacker-controlled SSH endpoint, and they are pre-existing rather
than introduced by this release. The fix is blocked on an upstream pre-release pin that no
amount of local version selection can resolve.

Note that `cargo audit` - the gate in `.github/scripts/pre-push-gate.sh` and `checks.yml` -
reports **clean** here: these three GHSA entries have no RustSec counterpart, so the Rust-side
gate is blind to them and only the GitHub Dependabot graph surfaces them. Treat the Dependabot
alert page as a distinct pre-release check, not as something the gate already covers.

## Resolution (2026-07-28)

**All three are closed on `main`.** Everything above this section is the record of the v4.1.6
audit and is left as written; this section says what happened afterwards.

Option 2 of the follow-up below is what landed, in PR #489: `russh` is at **0.62.4**, and the
`=3.0.0-rc.0` pin is relaxed by a `[patch.crates-io]` on `axpdev-lab/n0-mainline`, pinned by
`rev` and consisting of the v0.5.0 release commit plus the single line that widens the
requirement. `3.0.0-rc.0` is gone from the lockfile entirely.

Two things this does **not** settle, and neither is visible from a green build:

1. `cargo audit` is still blind here, for the reason given above. Nothing about this fix
   changes that, and it stays true for any future advisory in the same shape.
2. The patch section is a fork we now carry, with an exit condition that has to be acted on.

## Follow-up: the patch section, and how it is watched

The one open item is deleting `[patch.crates-io]` once it is no longer needed.

That is checked by `.github/scripts/check-n0-mainline-patch.sh`, run every Monday by
`.github/workflows/n0-mainline-patch-watch.yml` and runnable by hand at any time. It strips the
patch section in a scratch copy of the manifest, re-resolves the graph from scratch, and
restores `Cargo.toml`/`Cargo.lock` unconditionally on the way out. Exit 1 is the expected state
(same `ed25519-dalek` conflict, nothing to do), exit 0 means the section can go, exit 2 means
the failure changed shape and the output needs reading. The workflow stays silent on 1, fails
on 2, and files a single chore issue on 0 rather than one per week.

Testing resolution directly, rather than asking GitHub what n0-mainline has published, is
deliberate: it answers correctly however the block clears, and it cannot drift out of date the
way a hardcoded expectation about upstream would. Note the exit-0 report prints the resolved
`n0-mainline` version, because a *downgrade* also resolves the conflict and is not the outcome
being waited for.

When it does clear: delete the section and its comment block, `cargo update -p n0-mainline`,
run the pre-push gate and the peer/AeroShare live lane (this moves the crypto crate under the
iroh P2P stack as well as under russh), regenerate `cargo-sources.json`, and delete the script,
the workflow and this section together.

This replaced `.github/scripts/check-russh-unblock.sh`, which asked whether `russh 0.62.4`
resolves. PR #489 answered that question permanently, so from the moment it merged the old
script exited 0 on every run and printed instructions that had already been carried out -
verified on 2026-07-28 before removing it. It was a correct check that its own success turned
into a false green, which is worth recording as a shape rather than as an incident: a check
whose subject has been fixed does not become harmless, it becomes a liar.

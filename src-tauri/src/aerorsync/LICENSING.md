# Licensing and provenance of `aerorsync`

This file ships with the crate. It states what licence the code carries, who may
state it, and what the "clean-room" claim in the README does and does not cover.

## The expression

```
MPL-2.0 OR GPL-3.0-or-later
```

Every source file under `src/aerorsync/` carries that expression as an SPDX tag,
the emitted manifest declares it in `[package].license`, and both licence texts
travel with the crate as `LICENSE-MPL-2.0` and `LICENSE-GPL-3.0`. The emitter
refuses to produce a crate if any file is missing the tag, because a file without
a licence header compiles exactly like a file with one.

`OR` is the SPDX disjunction: the recipient chooses. Take the crate under
MPL-2.0 as a dependency, or under GPL-3.0-or-later, whichever fits.

## Why two licences and not one

The same source has two consumers, and each needs a different answer.

- **As a dependency.** The reserved repository `axpdev-lab/aerorsync` is MPL-2.0,
  decided 2026-08-28. A file-level copyleft licence is the right fit for a
  protocol library that other projects should be able to link.
- **Inside AeroFTP.** The application is GPL-3.0-or-later and these files compile
  into it. Emitting them under MPL alone would leave the application's own repo
  holding files whose header disagrees with the repository licence.

The dual expression makes both true at once and reopens neither decision. It also
makes the MPL-2.0 section 3.3 question moot: the GPL option is stated outright
rather than reached through the secondary-licence clause, and Exhibit B
("Incompatible With Secondary Licenses") is not applied to any file.

## Who may state this

Sole authorship, measured rather than assumed on 2026-09-04: the 25 source files
of the module carry four e-mail identities in the git history, all belonging to
the project owner, whose canonical address is `45786925+axpnet@users.noreply.github.com`.
The only known external contribution to the repository, PR #97, never touched
this module. No third-party code was merged into it and no contributor licence
agreement is outstanding, so the copyright holder can license the work under
both terms.

Contributions accepted into this module in the future must be made under the same
dual expression, and this file is the place that says so.

## What "clean-room" covers, and what it does not

The README calls the module a clean-room re-implementation of the rsync wire
protocol. That claim is about the **protocol**: no rsync source code was read
into this implementation, which was built against captured bytes and published
specifications, and the wire format itself is an interface, not an expression
(the README cites the reasoning and the `openrsync` precedent).

The claim is **not** about the emitted crate. `scripts/aerorsync-emit.sh`
performs a mechanical copy of these files into a standalone package: that is a
build step, not an independent re-implementation, and calling it clean-room would
stretch a true claim over something it does not describe. Anywhere the extraction
work is named, it is a copy.

## Third-party dependencies

The crate depends only on permissively licensed crates (`russh`, `ssh2`,
`tokio`, `zstd`, `flate2`, `xxhash-rust`, and the digest crates). It does not
link against librsync and does not spawn the rsync binary. The rsync project
itself is GPL-3.0-or-later and is not a dependency: it is the peer this crate
talks to over the wire.

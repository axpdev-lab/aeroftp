# Licensing and provenance of `aerorsync`

This file ships with the crate. It states what licence the code carries, who may
state it, and what the "clean-room" claim in the README does and does not cover.

## The expression

```text
MPL-2.0 OR GPL-3.0-or-later
```

Every **Rust** source file (`*.rs`) under `src/aerorsync/` carries that expression
as an SPDX tag, the emitted manifest declares it in `[package].license`, and both
licence texts travel with the crate as `LICENSE-MPL-2.0` and `LICENSE-GPL-3.0`.
The emitter refuses to produce a crate if any `*.rs` file is missing the tag,
because a file without a licence header compiles exactly like a file with one.

The other tracked files under the module (the capture harness scripts, the
Dockerfiles and compose files, the frozen transcript artifacts and these
documents) carry no per-file tag and are not checked by that guard. They are
covered by the package-level `license` field and the two licence texts, which is
what a recipient reads. Stating it here rather than leaving the earlier sentence
to imply otherwise: the guard is narrower than "every file", and a guard
described as wider than it is invites exactly the assumption it cannot support.

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

The dual statement is made by the project's copyright holder. Two things support
it, and they are different in kind, so they are written separately.

**What the history records, measured on 2026-09-04.** Every commit touching
`src-tauri/src/aerorsync` is authored under one of four e-mail addresses, all of
them the project owner's, whose canonical address is
`45786925+axpnet@users.noreply.github.com`. A spot check of PR #97, an external
contribution to this repository, shows it changed only
`src-tauri/src/providers/webdav.rs`, outside this module.

**What that does and does not establish.** It is evidence about who committed,
not a proof that no third-party code was ever incorporated, and a spot check of
one pull request is not an enumeration of every external contribution. Read it as
what it is: the history shows no external authorship in this module, which is
consistent with sole authorship without demonstrating it.

**What licenses the work.** The project accepts contributions under the Developer
Certificate of Origin (see [`DCO`](../../../DCO) and
[`CONTRIBUTING.md`](../../../CONTRIBUTING.md)), not a contributor licence
agreement: each contribution carries a `Signed-off-by` line in which its author
certifies the right to submit it under the project's licence. That, together with
the copyright the owner holds in their own contributions, is what makes the dual
expression theirs to state.

Contributions accepted into this module in the future are made under the same dual
expression, and this file is the place that says so.

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

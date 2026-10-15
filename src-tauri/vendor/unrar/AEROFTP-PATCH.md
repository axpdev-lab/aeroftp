# AeroFTP's unrar wrapper patch

Source: crates.io `unrar` 0.5.8, upstream commit
`8fe139781b92d0678afecc568496f5805401a6c4`
([muja/unrar.rs](https://github.com/muja/unrar.rs/tree/8fe139781b92d0678afecc568496f5805401a6c4)).
The crate archive has SHA-256
`92ec61343a630d2b50d13216dea5125e157d3fc180a7d3f447d22fe146b648fc`.
The seven Rust source files, README and MIT/Apache license texts are retained.
Cargo.toml contains the same runtime requirements; the upstream examples and
test data are omitted. The modified file is formatted with the project's rustfmt; the other six Rust files are unchanged.

The patch is confined to `src/open_archive.rs`. UnRAR declares its header
with one-byte packing, while the upstream Rust binding uses `repr(C)`. That
adds padding before pointers on 64-bit targets and displaces the redirection
fields. A private packed header matches `dll.hpp` at this FFI boundary, then
copies the public fields into the original Rust structure without unaligned
references. The upstream public binding and native library remain unchanged.

Before `RARReadHeaderEx`,
provide a bounded native-wide target buffer, then copy its value into owned
`FileHeader.redirection_target` alongside `redirection_type`. A target filling
the buffer is refused, since UnRAR truncates rather than reports its length.
The buffer is live throughout the FFI call and no native pointer escapes it.

Native `unrar_sys` 0.5.8 and its UnRAR C++ source stay on the existing registry
dependency. Extraction, passwords, callbacks and Unicode path conversion in
this wrapper are unchanged.

This lets AeroFTP validate references before extracting them. On Linux, the
existing wrapper supplies a full destination name to preserve Unicode without
changing the process locale, but no extraction root. Native hardlink lookup
then uses the CWD. AeroFTP creates a hardlink or file copy only from an earlier
regular member inside its private staging root, and skips native processing
of that reference. Symbolic links and junctions stay refused.

Removal condition: a released upstream wrapper exposes owned redirection kind
and target before extraction, including an explicit truncation error. Replace
this patch with that release and keep AeroFTP's root-confinement regressions.
No upstream contribution has been filed by this change.

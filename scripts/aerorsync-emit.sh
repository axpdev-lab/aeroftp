#!/usr/bin/env bash
# Re-emit `src-tauri/src/aerorsync/` as a standalone crate.
#
# The module is on its way out of this repository (phase E of the standalone
# crate plan). Until it leaves, the invariant that it does not depend on the
# application is checked by two guards that read the source as text, and a text
# scan can only refuse the spellings someone thought of: a review got one of
# them to pass while the module really did import `crate::settings`, with
# `use crate as app;`. This script exists so the invariant is proved by the
# compiler instead. Emitted here, the module is its own crate: any path that
# reaches the application does not resolve.
#
# The emitted crate keeps the module's own directory, `src/aerorsync/`, with a
# `lib.rs` that declares it. That is not cosmetic. The module addresses its own
# fixtures through `CARGO_MANIFEST_DIR/src/aerorsync/...` and two of its tests
# read that directory to scan the source; flattening the tree and rewriting
# `crate::aerorsync::` broke both, and the rewrite itself was the weak point a
# reviewer went for, because a `sed` that hides a dependency blinds the
# compiler that is supposed to find it. Nothing is rewritten now: the paths the
# code declares are the paths it gets.
#
# Idempotent: two runs from the same source produce the same tree and the same
# manifest, which is what phase E0 needs from it.
#
# Usage: scripts/aerorsync-emit.sh [outdir]     (default: target/aerorsync-standalone)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/src-tauri/src/aerorsync"
OUT="${1:-$ROOT/target/aerorsync-standalone}"

[ -d "$SRC" ] || { echo "sorgente assente: $SRC" >&2; exit 1; }

# The script removes the output directory before writing it, and then writes
# into it, so its guard is all that stands between a mistyped argument and a
# deleted or overwritten tree. Four versions of it have been walked past, and
# each refusal below is one of them:
#
#   - a denylist that enumerated the trees to protect accepted `public/` and
#     replaced 89 tracked files with the crate;
#   - an allowlist that exempted the default destination from every check
#     deleted an external directory a symlink on the default pointed at;
#   - that exemption ran only with no arguments, so naming the same default
#     explicitly walked around it;
#   - and the resolver re-appended the missing components of a path verbatim,
#     so `sibling-that-does-not-exist/../<repo>` matched none of the refusals,
#     and `mkdir -p` then created the missing component and made `..`
#     traversable, which put the emission in the repository root.
#
# So the destination is validated twice, once as a lexical path with `.` and
# `..` collapsed, and once as the physical path it resolves to, and both have
# to pass. A symlink defeats the lexical form and `..` defeats the physical
# one, which is why neither alone is enough.
#
# None of this creates anything: an earlier version canonicalised with
# `mkdir -p` and a refused invocation still left a directory inside the tree it
# was protecting. A refusal must not write.
ROOT_P="$(cd "$ROOT" && pwd -P)"
DEFAULT_P="$ROOT_P/target/aerorsync-standalone"

# Lessicale: assoluta, con `.` e `..` collassati senza toccare il filesystem.
# python3 e' gia' un requisito di questo script (il manifest).
lex_path() {
    python3 -c 'import os,sys
p = sys.argv[1]
if not os.path.isabs(p):
    p = os.path.join(os.getcwd(), p)
sys.stdout.write(os.path.normpath(p))' "$1"
}
# Fisica: risale al primo antenato che esiste e riappende il resto. Serve
# perche' su un checkout appena fatto non esiste ne' la destinazione ne'
# `target/` che la contiene, e una stesura che pretendeva il genitore esistente
# ha fatto rosso su tre OS con la destinazione ORDINARIA, non con un attacco.
phys_path() {
    local p="$1" tail=""
    while [ ! -d "$p" ]; do
        tail="$(basename "$p")${tail:+/$tail}"
        p="$(dirname "$p")"
    done
    p="$(cd "$p" && pwd -P)" || return 1
    if [ -n "$tail" ]; then printf '%s\n' "$p/$tail"; else printf '%s\n' "$p"; fi
}

OUT_REQ="$OUT"
OUT_LEX="$(lex_path "$OUT_REQ")" || { echo "outdir non utilizzabile: $OUT_REQ" >&2; exit 1; }
OUT="$(phys_path "$OUT_LEX")" || { echo "outdir non utilizzabile: $OUT_REQ" >&2; exit 1; }

# 1. Rifiuti assoluti, su ENTRAMBE le forme, per ogni destinazione, default
#    compreso.
for candidate in "$OUT_LEX" "$OUT"; do
    case "$candidate" in
        /|"$HOME")
            echo "rifiuto: l'outdir e' la radice o la home ($candidate)" >&2; exit 1;;
    esac
    case "$ROOT_P" in
        "$candidate"/*)
            echo "rifiuto: l'outdir contiene il repository ($candidate)" >&2; exit 1;;
    esac
done

# 2. Chi nomina il default deve ottenere il default. Vale comunque sia stato
#    invocato lo script, senza argomenti o nominando quella stessa path: una
#    stesura precedente controllava solo il primo caso. Il target di default e'
#    ignorato da git, quindi puo' essere sostituito da un symlink senza che
#    `git status` dica niente.
if [ "$OUT_LEX" = "$DEFAULT_P" ] && [ "$OUT" != "$DEFAULT_P" ]; then
    echo "rifiuto: il target di default non e' una directory dentro il repository" >&2
    echo "  atteso:   $DEFAULT_P" >&2
    echo "  risolto:  $OUT" >&2
    echo "  un componente della path e' un symlink che porta fuori" >&2
    exit 1
fi

# 3. Dentro il repository e' ammesso solo il default fisico, e la path lessicale
#    conta quanto quella fisica: `..` porta dentro senza che la stringa lo dica.
if [ "$OUT" != "$DEFAULT_P" ]; then
    for candidate in "$OUT_LEX" "$OUT"; do
        case "$candidate" in
            "$ROOT_P"|"$ROOT_P"/*)
                echo "rifiuto: l'outdir sta dentro il repository e non e' il target di default" >&2
                echo "  richiesto: $OUT_REQ" >&2
                echo "  risolto:   $candidate" >&2
                echo "  ammessi:   $DEFAULT_P, oppure una destinazione fuori da $ROOT_P" >&2
                exit 1;;
        esac
    done
    # Fuori dal repository il nome e' arbitrario, quindi risponde il contenuto:
    # una directory gia' piena che non porta il marcatore di una emissione
    # precedente non e' una destinazione di scratch, e non viene cancellata.
    # Il target di default e' escluso da questa regola: li' una emissione
    # interrotta a meta' lascia file senza EMITTED.sha256, e rifiutare la
    # successiva bloccherebbe la lane invece di proteggerla.
    if [ -n "$(ls -A "$OUT" 2>/dev/null)" ] && [ ! -f "$OUT/EMITTED.sha256" ]; then
        echo "rifiuto: $OUT non e' vuota e non contiene EMITTED.sha256, quindi non e' l'output di una emissione precedente" >&2
        exit 1
    fi
fi

# La path fisica e quella lessicale coincidono da qui in poi: se differissero,
# uno dei due rami sopra avrebbe gia' rifiutato oppure la differenza e' un
# symlink su un antenato che esiste, e in quel caso e' la fisica quella su cui
# lo script opera.
rm -rf "$OUT"
mkdir -p "$OUT/src/aerorsync"

# 1. The module keeps its place inside the crate. Only the feature gate goes:
#    here the crate IS the feature.
for f in "$SRC"/*.rs; do
    sed -e '/^#!\[cfg(feature = "aerorsync")\]$/d' \
        -e 's/#\[cfg(feature = "aerorsync")\]//g' \
        -e 's/cfg(all(test, feature = "aerorsync"))/cfg(test)/g' \
        "$f" > "$OUT/src/aerorsync/$(basename "$f")"
done

cat > "$OUT/src/lib.rs" <<'LIB'
// Generated by scripts/aerorsync-emit.sh. Do not edit by hand: edit the script.
//
// The crate is the module and nothing else. If any file under `src/aerorsync/`
// names an application module, the path does not resolve here and the build
// fails, which is the whole point of emitting this.
pub mod aerorsync;
LIB

# 2. The fixtures, at the path the code asks for rather than a path chosen
#    here. The constant is read from the source, so the two cannot drift: if
#    the module moves its transcript, this follows without being edited.
FROZEN_REL="$(sed -n 's/^pub const REAL_RSYNC_FROZEN_TRANSCRIPT_REL: &str = "\(.*\)";$/\1/p' \
    "$SRC/fixtures.rs")"
[ -n "$FROZEN_REL" ] || { echo "non trovo REAL_RSYNC_FROZEN_TRANSCRIPT_REL in fixtures.rs" >&2; exit 1; }
FROZEN_SRC="$ROOT/src-tauri/$FROZEN_REL"
if [ -d "$FROZEN_SRC" ]; then
    mkdir -p "$OUT/$FROZEN_REL"
    cp -a "$FROZEN_SRC/." "$OUT/$FROZEN_REL/"
    emitted="$(find "$OUT/$FROZEN_REL" -type f | wc -l)"
    origin="$(find "$FROZEN_SRC" -type f | wc -l)"
    if [ "$emitted" != "$origin" ] || [ "$emitted" -eq 0 ]; then
        echo "fixture incomplete: $emitted file emessi contro $origin" >&2
        exit 1
    fi
    echo "fixture congelate: $emitted file in $FROZEN_REL"
else
    # Not a warning on stderr that a caller can ignore: without the transcript
    # the oracle tests return early AND PASS, so an emission without fixtures
    # produces a green that means nothing.
    echo "fixture congelate assenti in $FROZEN_SRC: la suite emessa sarebbe verde senza provare nulla" >&2
    exit 1
fi

# 3. The manifest. Versions are read from the application's own Cargo.toml, not
#    from a list kept by hand here: a list would drift the day someone bumps a
#    dependency, and the drift would show up as a compile error nobody expects.
APP_MANIFEST="$ROOT/src-tauri/Cargo.toml"
dep() {
    local name="$1" line
    line="$(grep -m1 "^$name = " "$APP_MANIFEST" || true)"
    [ -n "$line" ] || { echo "dipendenza non trovata nel manifest dell'app: $name" >&2; exit 1; }
    line="${line%%#*}"
    echo "${line//, optional = true/}" | sed 's/optional = true, //; s/[[:space:]]*$//'
}

{
    cat <<'HEAD'
# Generated by scripts/aerorsync-emit.sh. Do not edit by hand: edit the script.
[package]
name = "aerorsync"
version = "0.1.0-alpha.1"
edition = "2021"
publish = false

[lints.rust]
# The lane 3 tests are gated on a cfg the harness sets with RUSTFLAGS. Declared
# for the same reason the application declares it: without this, `-D warnings`
# turns every one of those gates into an error.
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(ci_lane3)'] }

[dependencies]
HEAD
    for d in async-trait russh secrecy serde sha2 ssh2 tokio xxhash-rust \
             libc zstd tracing md-5 md4 sha1 base64 hex filetime; do
        dep "$d"
    done
    # flate2 needs a zlib backend for `Compress::new_with_window_bits`, which
    # `real_wire.rs` calls to speak the rsync deflate stream. In the
    # application that backend is on by accident: `zip`, pulled for archive
    # support, enables `zlib-rs`, and cargo unifies the features. Nothing
    # declares the dependency, so the day `zip` changes the module stops
    # compiling for a reason nobody would connect to it. Declared here.
    echo 'flate2 = { version = "1.0", features = ["zlib-rs"] }'
    cat <<'ACL'

[target.'cfg(target_os = "linux")'.dependencies]
ACL
    for d in acl-sys posix-acl; do
        dep "$d"
    done
    cat <<'DEV'

[dev-dependencies]
DEV
    # Test-only, and only visible once the tests are compiled here.
    for d in tempfile serde_json rand_010; do
        dep "$d"
    done
} > "$OUT/Cargo.toml"

# 3b. La configurazione Cargo dell'applicazione, copiata e non riscritta. La
#     build Windows x86_64 dell'app gira da `src-tauri`, che porta
#     `-C target-feature=+crt-static`; il crate emesso sta altrove e la ricerca
#     gerarchica di Cargo non visita la directory sorella, quindi
#     `cfg(target_feature = "crt-static")` era accesa nel binario spedito e
#     spenta qui. Una review ha nascosto proprio li' una dipendenza reale da un
#     sorgente dell'applicazione, e check dev, check release, clippy e la
#     guardia testuale erano tutti verdi. Copiata cosi' com'e', per la stessa
#     ragione per cui le versioni delle dipendenze si leggono dal manifest
#     dell'app: un elenco tenuto a mano qui driftrebbe.
APP_CARGO_CONFIG="$ROOT/src-tauri/.cargo/config.toml"
if [ -f "$APP_CARGO_CONFIG" ]; then
    mkdir -p "$OUT/.cargo"
    {
        echo "# Copiata da src-tauri/.cargo/config.toml da scripts/aerorsync-emit.sh."
        echo "# Il crate emesso deve compilare sotto gli stessi rustflags del prodotto"
        echo "# spedito, altrimenti una cfg che il prodotto accende resta spenta qui."
        cat "$APP_CARGO_CONFIG"
    } > "$OUT/.cargo/config.toml"
    echo "configurazione cargo: copiata da src-tauri/.cargo/config.toml"
else
    echo "configurazione cargo dell'app assente: $APP_CARGO_CONFIG" >&2
    exit 1
fi

# 4. A manifest of what was emitted. Every entry that is not a directory, with
#    its mode, so a symlink or a permission change is part of what two runs
#    compare. It certifies the emitted TREE, not the dependency resolution:
#    no lockfile is emitted, so `cargo check` resolves compatible versions from
#    the registry and can pick different ones over time with this manifest
#    unchanged.
#    Written with python3 rather than `find -printf` plus `sha256sum`: the
#    first is GNU-only and the second is absent on macOS, and this script runs
#    on all three operating systems the module ships on. The program goes to a
#    file before it runs, and not into a here-document inside `$(...)`: the
#    bash macOS 14 ships (3.2) does not parse that nesting and failed the lane
#    with `unexpected EOF while looking for matching`, which is the same
#    portability problem the python was introduced to solve.
PY_TMP="$(mktemp "${TMPDIR:-/tmp}/aerorsync-emit-manifest.XXXXXX")"
# Con `set -e` un fallimento di `cat` o di python3 uscirebbe senza cancellarlo.
trap 'rm -f "$PY_TMP"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cat > "$PY_TMP" <<'PYEOF'
import hashlib, os, sys

out = sys.argv[1]
rows = []
# followlinks stays off: a symlink to a directory is an entry to certify, not a
# tree to walk into. os.walk puts it in `dirs`, so listing only `files` left it
# out of the manifest entirely, and two trees that differ by one could share a
# hash.
for root, dirs, files in os.walk(out):
    dirs.sort()
    linked_dirs = [d for d in dirs if os.path.islink(os.path.join(root, d))]
    for name in sorted(files + linked_dirs):
        if name in ("EMITTED.sha256",):
            continue
        full = os.path.join(root, name)
        rel = os.path.relpath(full, out)
        mode = oct(os.lstat(full).st_mode & 0o7777)[2:]
        if os.path.islink(full):
            # A symlink's target is what two emissions must agree on, not the
            # content it happens to point at today.
            digest = "symlink:" + os.readlink(full)
        else:
            h = hashlib.sha256()
            with open(full, "rb") as f:
                for block in iter(lambda: f.read(1 << 20), b""):
                    h.update(block)
            digest = h.hexdigest()
        rows.append("%s %s %s" % (digest, mode, rel.replace(os.sep, "/")))

rows.sort()
body = "\n".join(rows) + "\n"
with open(os.path.join(out, "EMITTED.sha256"), "w", encoding="utf-8") as f:
    f.write(body)
print(hashlib.sha256(body.encode("utf-8")).hexdigest())
PYEOF
MANIFEST_SHA="$(python3 "$PY_TMP" "$OUT")"

echo "emesso in $OUT"
echo "voci: $(grep -c . "$OUT/EMITTED.sha256")"
echo "manifest: $MANIFEST_SHA"

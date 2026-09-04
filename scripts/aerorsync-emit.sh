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

# `pwd -P` e non `pwd`: la radice va confrontata con destinazioni risolte
# fisicamente, e una review ha lanciato lo script da una path symlinkata verso
# il repository, cosa che rendeva la forma logica e quella fisica diverse per
# sempre. Il confronto che riconosce il target di default non scattava mai e si
# ricadeva sull'euristica del contenuto: stessa manomissione, stesso symlink
# sul default, e la vittima esterna veniva cancellata a seconda di come si
# scriveva la path da cui lo script era invocato.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
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
# `$ROOT` e' gia' fisico (vedi sopra), quindi il default lo e' per costruzione.
DEFAULT_P="$ROOT/target/aerorsync-standalone"

# Lessicale: assoluta, con `.` e `..` collassati, senza toccare il filesystem.
# In bash puro e non in python3: su Windows il python dell'host e' quello
# nativo e MSYS traduce gli argomenti che sembrano path, quindi la funzione
# avrebbe restituito una stringa in sintassi Windows che bash poi confronta con
# path POSIX. Nessuno dei due confronti sarebbe mai stato vero, e su quel job
# la meta' lessicale del guardiano, che e' l'unica a fermare
# `componente-inesistente/../<repo>`, sarebbe stata inerte senza dirlo.
lex_path() {
    local p="$1" out="" seg rest drive restw
    # Una path in sintassi Windows arriva davvero: su quel runner `RUNNER_TEMP`
    # vale `D:\a\_temp`, e sotto il bash di Git for Windows tutto il resto
    # (`$PWD`, la radice del repository) e' invece in forma MSYS, `/d/a/...`.
    # Senza questa conversione la path non comincia per `/`, viene presa per
    # relativa e incollata a `$PWD`, ed e' cosi' che la lane Windows si e'
    # fermata con `risolto: /d/a/aeroftp/aeroftp/D:\a\_temp/...`. La
    # conversione dei backslash si fa SOLO dentro questo ramo: su un sistema
    # POSIX un backslash e' un carattere legittimo di un nome di file.
    case "$p" in
        [A-Za-z]:[\\/]*)
            drive="${p%%:*}"
            restw="${p#?:}"
            drive="$(printf '%s' "$drive" | tr '[:upper:]' '[:lower:]')"
            p="/$drive$(printf '%s' "$restw" | tr '\\' '/')"
            ;;
    esac
    case "$p" in
        /*) ;;
        *) p="$PWD/$p";;
    esac
    # Taglio a mano invece di `for seg in $p` con IFS=/: quella e' una
    # espansione NON quotata, quindi oltre al field splitting fa anche il GLOB,
    # e una review ha misurato `/tmp/*` diventare `/tmp/alfa/beta/gamma`. La
    # normalizzazione esiste per rendere `..` visibile ai rifiuti, e li' il
    # numero di componenti fra il `..` e il suo bersaglio lo decideva il
    # contenuto della directory corrente. `set -f` non va bene come rimedio:
    # piu' avanti lo script ha bisogno del glob per `for f in "$SRC"/*.rs`.
    rest="$p"
    while [ -n "$rest" ]; do
        seg="${rest%%/*}"
        if [ "$seg" = "$rest" ]; then rest=""; else rest="${rest#*/}"; fi
        case "$seg" in
            ""|.) ;;
            ..) out="${out%/*}";;
            *) out="$out/$seg";;
        esac
    done
    printf '%s\n' "${out:-/}"
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
#    compreso. La home si confronta anch'essa nelle due forme: se sta dietro un
#    symlink, nominarne una sola delle due sfuggirebbe. E se `HOME` non e'
#    impostata lo script deve rifiutare o proseguire, non morire con
#    `unbound variable` sotto `set -u`.
HOME_LEX="${HOME:-}"
if [ -n "$HOME_LEX" ] && [ -d "$HOME_LEX" ]; then
    HOME_PHYS="$(cd "$HOME_LEX" && pwd -P)"
else
    HOME_PHYS="$HOME_LEX"
fi
for candidate in "$OUT_LEX" "$OUT"; do
    if [ "$candidate" = "/" ]; then
        echo "rifiuto: l'outdir e' la radice del filesystem" >&2; exit 1
    fi
    if [ -n "$HOME_LEX" ] && { [ "$candidate" = "$HOME_LEX" ] || [ "$candidate" = "$HOME_PHYS" ]; }; then
        echo "rifiuto: l'outdir e' la home ($candidate)" >&2; exit 1
    fi
    case "$ROOT" in
        "$candidate"/*)
            echo "rifiuto: l'outdir contiene il repository ($candidate)" >&2; exit 1;;
    esac
done

# 2. Chi nomina il default deve ottenere il default. Vale comunque sia stato
#    invocato lo script, senza argomenti o nominando quella stessa path: una
#    stesura precedente controllava solo il primo caso. Il target di default e'
#    ignorato da git, quindi puo' essere sostituito da un symlink senza che
#    `git status` dica niente.
if [ "$OUT_LEX" = "$DEFAULT_P" ] && { [ "$OUT" != "$DEFAULT_P" ] || [ -L "$DEFAULT_P" ]; }; then
    echo "rifiuto: il target di default non e' una directory dentro il repository" >&2
    echo "  atteso:   $DEFAULT_P" >&2
    echo "  risolto:  $OUT" >&2
    echo "  un componente della path e' un symlink" >&2
    echo "  se target/ sta su un altro disco per scelta, passa la destinazione esplicitamente" >&2
    exit 1
fi

# 3. Dentro il repository e' ammesso solo il default fisico, e la path lessicale
#    conta quanto quella fisica: `..` porta dentro senza che la stringa lo dica.
if [ "$OUT" != "$DEFAULT_P" ]; then
    for candidate in "$OUT_LEX" "$OUT"; do
        case "$candidate" in
            "$ROOT"|"$ROOT"/*)
                echo "rifiuto: l'outdir sta dentro il repository e non e' il target di default" >&2
                echo "  richiesto: $OUT_REQ" >&2
                echo "  risolto:   $candidate" >&2
                echo "  ammessi:   $DEFAULT_P, oppure una destinazione fuori da $ROOT" >&2
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
# La licenza non e' una riga di metadati, e' la sola cosa che dice a chi riceve
# il crate cosa puo' farne. Il crate esce come `MPL-2.0 OR GPL-3.0-or-later`:
# MPL per chi lo prende come dipendenza, GPL perche' gli stessi file compilano
# dentro AeroFTP, che e' GPL. Un file senza il tag esce senza licenza e nessuno
# se ne accorge, perche' un crate senza header compila esattamente come uno con
# l'header.
#
# Il controllo sta PRIMA di `rm -rf "$OUT"` e guarda il sorgente, non la copia:
# nella versione precedente girava dopo, quindi un rifiuto lasciava una
# destinazione mezza scritta, senza manifest e senza licenze, che somiglia a un
# crate abbastanza da poter essere scambiata per uno. Un cancello che scatta
# dopo il passo distruttivo protegge il prossimo passo, non questo.
SPDX_EXPECTED='// SPDX-License-Identifier: MPL-2.0 OR GPL-3.0-or-later'
missing_spdx=""
for f in "$SRC"/*.rs; do
    grep -qF "$SPDX_EXPECTED" "$f" || missing_spdx="$missing_spdx $(basename "$f")"
done
if [ -n "$missing_spdx" ]; then
    echo "emissione rifiutata: file senza '$SPDX_EXPECTED':$missing_spdx" >&2
    echo "aggiungi l'header in $SRC. Niente e' stato scritto in $OUT." >&2
    exit 1
fi

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

# 1b. I due testi di licenza, copiati verbatim. Il crate ne porta due perche'
#     l'espressione ne nomina due: un `OR` con un solo testo presente e' una
#     promessa che il pacchetto non mantiene.
cp "$SRC/LICENSE-MPL-2.0" "$OUT/LICENSE-MPL-2.0"
cp "$ROOT/LICENSE" "$OUT/LICENSE-GPL-3.0"
cp "$SRC/LICENSING.md" "$OUT/LICENSING.md"

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
# La destinazione delle fixture e' la SECONDA cosa che questo script scrive, e
# arriva da una costante nel sorgente invece che dal guardrail sopra. Con un
# refuso (o una manomissione) che la porta fuori da `$OUT` l'emissione scrive
# altrove e restituisce comunque 0, e il manifest, che cammina solo `$OUT`, non
# se ne accorge. Una review ci ha messo "../src" e ha scritto 632 file fuori
# dalla destinazione. Relativa e senza risalite: e' tutto quello che quella
# costante puo' legittimamente essere.
case "$FROZEN_REL" in
    /*) echo "REAL_RSYNC_FROZEN_TRANSCRIPT_REL e' assoluta: $FROZEN_REL" >&2; exit 1;;
esac
case "/$FROZEN_REL/" in
    */../*) echo "REAL_RSYNC_FROZEN_TRANSCRIPT_REL risale fuori dalla destinazione: $FROZEN_REL" >&2; exit 1;;
esac
FROZEN_SRC="$ROOT/src-tauri/$FROZEN_REL"
if [ -d "$FROZEN_SRC" ]; then
    mkdir -p "$OUT/$FROZEN_REL"
    cp -a "$FROZEN_SRC/." "$OUT/$FROZEN_REL/"
    emitted="$(find "$OUT/$FROZEN_REL" -type f | wc -l)"
    origin="$(find "$FROZEN_SRC" -type f | wc -l)"
    if [ "$emitted" != "$origin" ] || [ "$emitted" -eq 0 ]; then
        echo "fixture incomplete: $emitted righe emesse contro $origin in $FROZEN_REL" >&2
        echo "  origine:      $FROZEN_SRC" >&2
        echo "  destinazione: $OUT/$FROZEN_REL" >&2
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
authors = ["axpdev"]
description = "Native Rust implementation of the rsync wire protocol 31"
repository = "https://github.com/axpdev-lab/aerorsync"
license = "MPL-2.0 OR GPL-3.0-or-later"

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

# 3b-bis. Il lockfile dell'applicazione, copiato per la stessa ragione del
#     config: senza, il crate emesso risolve libero e compila sorgenti diversi
#     da quelli spediti. Non e' ipotetico, e' vivo: il prodotto pinna
#     `russh 0.63.1` (il crate su cui il manifest porta la nota GHSA e che
#     questo repository tiene pinnato per storia) e la lane risolveva 0.63.2.
#     I requisiti del manifest sono larghi (`tokio = "1"`, `libc = "0.2"`),
#     quindi la superficie e' molto piu' ampia di quel singolo caso.
#     Il verso di questo limite va detto: il drift di versione non puo'
#     produrre un falso VERDE, perche' un import applicativo non risolve
#     qualunque sia la versione di tokio. Costa parita' con il prodotto e un
#     rosso che puo' arrivare senza che nessuno abbia committato niente.
#     Cargo accetta un lock con un root package diverso e lo pota da solo
#     (1189 pacchetti in ingresso, 251 dopo la potatura). NON aggiungere
#     `--locked` agli step: con il lock non ancora potato la prima corsa
#     fallirebbe, perche' `--locked` vieta proprio la potatura che serve. Il
#     valore sta nei pin, non nel flag.
cp "$ROOT/src-tauri/Cargo.lock" "$OUT/Cargo.lock"
echo "lockfile: copiato da src-tauri/Cargo.lock"

# 3c. Un overlay per le regioni `ci_lane3`. NON viene caricato da solo: cargo
#     legge `.cargo/config.toml`, non `.cargo/lane3.toml`, quindi questo file
#     esiste solo per essere passato con `cargo --config` negli step di check e
#     clippy, e `cargo test` non lo vede mai. Serve perche' quel cfg lo accende
#     solo la lane 3 dell'applicazione, il manifest emesso lo DICHIARA nella
#     check-cfg, e quindi le regioni dietro di lui vengono elise qui senza
#     nemmeno un warning: una review ci ha nascosto dentro una chiamata vera a
#     un modulo dell'applicazione, e check dev, check release, clippy e la
#     guardia testuale erano tutti verdi.
#     Si passa con `--config` e non con RUSTFLAGS perche' la variabile
#     d'ambiente SOSTITUISCE i rustflags del config invece di fonderli, e
#     spegnerebbe `+crt-static` proprio sul job Windows che la copia del config
#     esiste per riprodurre. I blocchi [target.*] sono derivati dal file che lo
#     script sta gia' copiando e non elencati qui: servono perche' un
#     [target.<triple>] del file base ha la precedenza su [build] e lo ignora,
#     mentre con il blocco omonimo nell'overlay i due array si fondono.
{
    echo "# Generated by scripts/aerorsync-emit.sh. Do not edit by hand: edit the script."
    echo "# Passed with \`cargo --config\` on the check and clippy steps only, never on"
    echo "# \`cargo test\`."
    echo
    echo "# I triple senza un blocco proprio nel config dell'app rispondono qui."
    echo "[build]"
    echo 'rustflags = ["--cfg", "ci_lane3"]'
    sed -n 's/^\[target\.\(.*\)\]$/\1/p' "$APP_CARGO_CONFIG" | while read -r triple; do
        echo
        echo "# Fuso con l'array omonimo del config copiato, non lo sostituisce."
        echo "[target.$triple]"
        echo 'rustflags = ["--cfg", "ci_lane3"]'
    done
} > "$OUT/.cargo/lane3.toml"

# 4. A manifest of what was emitted. Every entry that is not a directory, with
#    its mode, so a symlink or a permission change is part of what two runs
#    compare. It now covers the dependency resolution too, because the
#    application's lockfile is one of the entries: two emissions of the same
#    source carry the same pins. What it does NOT cover is what cargo does to
#    that lockfile afterwards, since building prunes it in place, so a manifest
#    taken after a build differs from the one taken at emission.
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

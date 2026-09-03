#!/usr/bin/env bash
# The scenarios `scripts/aerorsync-emit.sh` must keep refusing, as a gate.
#
# The emitter deletes its destination before writing it, so its guard is the
# only thing between a mistyped argument and a deleted directory. Two reviews
# walked past two successive versions of that guard: the first was a denylist
# and accepted `public/`, replacing 89 tracked files with the crate; the second
# was an allowlist but exempted the default destination from every check, and a
# symlink planted on the default (a gitignored path, so `git status` says
# nothing) then had the exemption delete a directory outside the repository.
# Both were found by hand. A property checked by hand is checked once, so the
# scenarios live here and run in the lane.
#
# Hardening the refusals is also how the ordinary path broke: a version that
# demanded the destination's parent exist failed all three jobs of the lane on
# a fresh checkout, where `target/` does not exist yet. The happy paths are
# scenarios too, and they come first.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EMIT="$ROOT/scripts/aerorsync-emit.sh"
SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/aerorsync-guard.XXXXXX")"
DEFAULT="$ROOT/target/aerorsync-standalone"
ok=0; ko=0

cleanup() {
    rm -rf "$SCRATCH"
    # A symlink left on the default would make the next emission refuse.
    [ -L "$DEFAULT" ] && rm -f "$DEFAULT"
    rm -rf "$ROOT/src-tauri/mai-esistita"
    return 0
}
trap cleanup EXIT

chk() {
    if [ "$1" = "$2" ]; then
        echo "  ok    $3"; ok=$((ok + 1))
    else
        echo "  FALLITO $3 (atteso $1, ottenuto $2)"; ko=$((ko + 1))
    fi
}

echo "== quello che deve funzionare =="
rm -rf "$ROOT/target/aerorsync-standalone"
bash "$EMIT" >/dev/null 2>&1; chk 0 $? "destinazione di default"
bash "$EMIT" >/dev/null 2>&1; chk 0 $? "seconda emissione sopra la prima"
first="$(bash "$EMIT" "$SCRATCH/fuori" 2>/dev/null | sed -n 's/^manifest: //p')"
second="$(bash "$EMIT" "$SCRATCH/fuori" 2>/dev/null | sed -n 's/^manifest: //p')"
chk 0 "$([ -n "$first" ] && echo 0 || echo 1)" "una destinazione esterna nuova emette"
chk "$first" "$second" "e due emissioni danno lo stesso manifest"
bash "$EMIT" "$SCRATCH/a/b/c" >/dev/null 2>&1; chk 0 $? "una destinazione annidata che non esiste ancora"

echo "== quello che deve essere rifiutato =="
bash "$EMIT" "$ROOT/public" >/dev/null 2>&1; chk 1 $? "un albero tracciato dentro il repository"
chk 0 "$(cd "$ROOT" && git status --porcelain -- public | wc -l | tr -d ' ')" "e quell'albero e' intatto"

bash "$EMIT" "$ROOT/src-tauri/mai-esistita" >/dev/null 2>&1; chk 1 $? "una path nuova dentro un albero sorgente"
chk 1 "$([ -e "$ROOT/src-tauri/mai-esistita" ] && echo 0 || echo 1)" "e il rifiuto non ha creato niente"

bash "$EMIT" / >/dev/null 2>&1; chk 1 $? "la radice del filesystem"
bash "$EMIT" "$HOME" >/dev/null 2>&1; chk 1 $? "la home"
bash "$EMIT" "$(dirname "$ROOT")" >/dev/null 2>&1; chk 1 $? "un antenato del repository"

mkdir -p "$SCRATCH/estranea"; echo prezioso > "$SCRATCH/estranea/keep.txt"
bash "$EMIT" "$SCRATCH/estranea" >/dev/null 2>&1; chk 1 $? "una directory esterna piena senza EMITTED.sha256"
chk 0 "$([ -f "$SCRATCH/estranea/keep.txt" ] && echo 0 || echo 1)" "e il suo contenuto e' intatto"

# Il caso che la seconda stesura della allowlist non vedeva: il default e' una
# path ignorata da git, quindi puo' essere sostituita da un symlink senza che
# `git status` dia alcun segnale, e l'esenzione del default copriva allora la
# destinazione esterna.
mkdir -p "$SCRATCH/vittima-piena"; echo prezioso > "$SCRATCH/vittima-piena/keep.txt"
rm -rf "$DEFAULT"; ln -s "$SCRATCH/vittima-piena" "$DEFAULT"
bash "$EMIT" >/dev/null 2>&1; chk 1 $? "il default e' un symlink verso una directory esterna piena"
chk 0 "$([ -f "$SCRATCH/vittima-piena/keep.txt" ] && echo 0 || echo 1)" "e la vittima e' intatta"

mkdir -p "$SCRATCH/vittima-vuota"
rm -f "$DEFAULT"; ln -s "$SCRATCH/vittima-vuota" "$DEFAULT"
bash "$EMIT" >/dev/null 2>&1; chk 1 $? "il default e' un symlink verso una directory esterna vuota"
chk 0 "$([ -d "$SCRATCH/vittima-vuota" ] && echo 0 || echo 1)" "e la vittima esiste ancora"

rm -f "$DEFAULT"; ln -s "$HOME" "$DEFAULT"
bash "$EMIT" >/dev/null 2>&1; chk 1 $? "il default e' un symlink verso la home"
rm -f "$DEFAULT"

echo "scenari: $ok superati, $ko falliti"
[ "$ko" -eq 0 ]

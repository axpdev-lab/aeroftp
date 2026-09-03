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
# EXIT da solo non basta su ogni shell, e attaccare `cleanup` direttamente a INT
# e TERM lo eseguirebbe DUE volte (il gestore del segnale, poi EXIT) e senza
# fermare lo script, perche' un gestore di segnale in bash torna al punto in cui
# era. Uscire e' quello che serve: l'uscita fa scattare EXIT una volta sola.
trap 'exit 130' INT
trap 'exit 143' TERM

chk() {
    if [ "$1" = "$2" ]; then
        echo "  ok    $3"; ok=$((ok + 1))
    else
        echo "  FALLITO $3 (atteso $1, ottenuto $2)"; ko=$((ko + 1))
    fi
}

# `lex_path` e' meta' della guardia (l'altra meta' e' la risoluzione fisica) e
# non e' raggiungibile invocando l'emettitore senza fargli scrivere qualcosa,
# perche' le forme interessanti puntano fuori dal repository. Viene estratta dal
# sorgente e provata qui, sulle forme che l'hanno gia' rotta due volte: il glob
# di una espansione non quotata, e una path in sintassi Windows che, non
# cominciando per `/`, veniva presa per relativa e incollata a `$PWD`.
echo "== la normalizzazione lessicale =="
lex_cases() {
    sed -n '/^lex_path() {/,/^}/p' "$EMIT" > "$SCRATCH/lex.sh"
    cat >> "$SCRATCH/lex.sh" <<'PROBE'
cd /tmp || exit 1
lex_path "$1"
PROBE
    bash "$SCRATCH/lex.sh" "$1"
}
chk "/d/a/_temp/x"      "$(lex_cases 'D:\a\_temp/x')"   "una path assoluta in sintassi Windows diventa la forma MSYS"
chk "/c/Users/x"        "$(lex_cases 'C:\Users\x')"     "e la lettera di drive va in minuscolo"
chk "/d/a/b"            "$(lex_cases 'd:/a/b')"          "anche con gli slash normali"
chk "/tmp/b"            "$(lex_cases '/tmp/a/../b')"     "un .. viene collassato"
chk "/tmp/a"            "$(lex_cases '//tmp//a')"        "gli slash doppi si riducono"
chk "/"                 "$(lex_cases '/..')"             "un .. sopra la radice resta la radice"
chk "/tmp/con spazio/x" "$(lex_cases '/tmp/con spazio/x')" "uno spazio non spezza la path"
chk "/tmp/*"            "$(lex_cases '/tmp/*')"          "un asterisco resta un asterisco e non si espande"
chk "/tmp/relativa/x"   "$(lex_cases 'relativa/x')"      "una path relativa parte dalla directory corrente"
chk "/tmp/back\slash"    "$(lex_cases '/tmp/back\slash')"  "su POSIX un backslash e' un carattere di nome, non un separatore"

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

# Il quarto giro: nominare esplicitamente la stessa path del default saltava il
# controllo fisico, che girava solo con zero argomenti. Una vittima VUOTA, o una
# che porta il marcatore, arrivava fino a `rm -rf`.
mkdir -p "$SCRATCH/vittima-esplicita"
rm -rf "$DEFAULT"; ln -s "$SCRATCH/vittima-esplicita" "$DEFAULT"
bash "$EMIT" "$ROOT/target/aerorsync-standalone" >/dev/null 2>&1; chk 1 $? "il default nominato esplicitamente, ed e' un symlink"
chk 0 "$([ -d "$SCRATCH/vittima-esplicita" ] && echo 0 || echo 1)" "e la vittima esiste ancora"
touch "$SCRATCH/vittima-esplicita/EMITTED.sha256"
bash "$EMIT" "$ROOT/target/aerorsync-standalone" >/dev/null 2>&1; chk 1 $? "lo stesso, con il marcatore di una emissione dentro la vittima"
chk 0 "$([ -d "$SCRATCH/vittima-esplicita" ] && echo 0 || echo 1)" "e la vittima esiste ancora"
rm -f "$DEFAULT"

# Lo stesso attacco, ma cambiando soltanto la path da cui lo script viene
# invocato. Una review ha lanciato l'emettitore attraverso un symlink verso il
# repository: la radice era logica e il default fisico, non si incontravano mai,
# il confronto che riconosce il default non scattava e si ricadeva
# sull'euristica del contenuto, che un `EMITTED.sha256` piantato nella vittima
# soddisfa. Stessa manomissione, esito opposto, per una spelling.
ln -s "$ROOT" "$SCRATCH/repolink"
mkdir -p "$SCRATCH/vittima-da-symlink"; echo prezioso > "$SCRATCH/vittima-da-symlink/keep.txt"
touch "$SCRATCH/vittima-da-symlink/EMITTED.sha256"
rm -rf "$DEFAULT"; ln -s "$SCRATCH/vittima-da-symlink" "$DEFAULT"
bash "$SCRATCH/repolink/scripts/aerorsync-emit.sh" >/dev/null 2>&1; chk 1 $? "lo script invocato attraverso un symlink verso il repository"
chk 0 "$([ -f "$SCRATCH/vittima-da-symlink/keep.txt" ] && echo 0 || echo 1)" "e la vittima e' intatta"
rm -f "$DEFAULT"

# Un symlink penzolante sul default: non porta da nessuna parte, ma va rifiutato
# con lo stesso messaggio di uno che punta a una directory, non accettato in
# silenzio perche' `-d` su di lui e' falso.
ln -s "$SCRATCH/non-esiste-affatto" "$DEFAULT"
bash "$EMIT" >/dev/null 2>&1; chk 1 $? "il default e' un symlink penzolante"
rm -f "$DEFAULT"

# Il glob: `lex_path` faceva una espansione non quotata, quindi una path che
# contiene `*` veniva sostituita dai nomi presenti nella directory, e quei nomi
# diventavano COMPONENTI. Il rifiuto stampava un "risolto" inventato e
# `rm -rf` e `mkdir -p` finivano su una path che nessuno aveva scritto.
mkdir -p "$SCRATCH/globdir/alfa" "$SCRATCH/globdir/beta"
bash "$EMIT" "$SCRATCH/globdir/*" >/dev/null 2>&1 || true
chk 1 "$([ -e "$SCRATCH/globdir/alfa/Cargo.toml" ] && echo 0 || echo 1)" "un asterisco nella path non diventa i nomi della directory"
chk 1 "$([ -e "$SCRATCH/globdir/beta/Cargo.toml" ] && echo 0 || echo 1)" "e nemmeno il secondo"

# `HOME` non impostata: sotto `set -u` il confronto con la home moriva con
# `unbound variable` invece di dare un rifiuto o proseguire.
( unset HOME; bash "$EMIT" "$SCRATCH/senza-home" >/dev/null 2>&1 ); chk 0 $? "senza HOME l'emissione funziona invece di morire"

# E il bypass peggiore: un componente che non esiste seguito da `..`. Nessun
# rifiuto lo vedeva, perche' la stringa non comincia per la radice del
# repository; poi `mkdir -p` creava il componente mancante, `..` diventava
# attraversabile e l'emissione scriveva nella radice, `Cargo.toml` compreso.
sibling="$(dirname "$ROOT")/aerorsync-guard-nonexistent"
bash "$EMIT" "$sibling/../$(basename "$ROOT")" >/dev/null 2>&1; chk 1 $? "un componente inesistente seguito da .. che rientra nel repository"
chk 1 "$([ -e "$sibling" ] && echo 0 || echo 1)" "e il componente inesistente non e' stato creato"
chk 0 "$(cd "$ROOT" && git status --porcelain -- Cargo.toml src 2>/dev/null | wc -l | tr -d ' ')" "e la radice del repository e' intatta"

echo "scenari: $ok superati, $ko falliti"
[ "$ko" -eq 0 ]

#!/usr/bin/env python3
"""Apply a batch of translations to src/i18n/locales/<code>.json.

Reads a JSON batch of the shape {"<locale>": {"<leafKey>": "<translation>"}} and
rewrites only the value of each key, in place, as a text edit. It is deliberately
NOT a json.dump round-trip: that would reflow all 46 files and bury a 20-key
change in a 5000-key diff.

Refuses to touch a key it cannot find exactly once, and refuses to overwrite a
value that is not still marked [NEEDS TRANSLATION], so re-running it cannot
silently clobber a human correction.
"""
import json
import re
import sys
from pathlib import Path

LOCALES = Path(__file__).resolve().parent.parent / "src" / "i18n" / "locales"


def esc(value: str) -> str:
    """Escape a string for embedding in a JSON document, keeping UTF-8 literal."""
    return json.dumps(value, ensure_ascii=False)[1:-1]


def apply_batch(batch: dict, *, force: bool = False) -> int:
    changed = 0
    for locale, entries in batch.items():
        path = LOCALES / f"{locale}.json"
        if not path.exists():
            sys.exit(f"unknown locale: {locale}")
        text = path.read_text(encoding="utf-8")
        for key, translation in entries.items():
            pattern = re.compile(r'("' + re.escape(key) + r'": ")((?:[^"\\]|\\.)*)(")')
            found = pattern.findall(text)
            if len(found) != 1:
                sys.exit(f"{locale}: key {key!r} found {len(found)} times, expected 1")
            current = found[0][1]
            if "NEEDS TRANSLATION" not in current and not force:
                sys.exit(
                    f"{locale}: key {key!r} is already translated; refusing to overwrite "
                    f"(pass --force if that is really what you want)"
                )
            text = pattern.sub(lambda m: m.group(1) + esc(translation) + m.group(3), text, count=1)
            changed += 1
        path.write_text(text, encoding="utf-8")
        json.loads(text)  # the file must still parse
    return changed


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if a != "--force"]
    if len(args) != 1:
        sys.exit("usage: apply-i18n-batch.py [--force] <batch.json>")
    payload = json.loads(Path(args[0]).read_text(encoding="utf-8"))
    n = apply_batch(payload, force="--force" in sys.argv)
    print(f"applied {n} translations across {len(payload)} locale(s)")

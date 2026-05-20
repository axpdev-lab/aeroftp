#!/usr/bin/env python3
"""Append one parity cell row to cells.json.

Called by parity_harness.sh; positional argv:
  1  cells_json_path
  2  surface
  3  protocol
  4  file_tag
  5  wall_clock_s (float)
  6  baseline_wall_clock_s (float, "" -> null)
  7  speedup (float, "" -> null)
  8  band_floor (float, "" -> null)
  9  band_ceiling (float, "" -> null)
 10  sha256_source (str)
 11  sha256_roundtrip (str)
 12  byte_identical ("true"/"false")
 13  expected_kind (str)
 14  exit_code (int)
 15  passed ("true"/"false")
"""
import json
import sys
from pathlib import Path


def f_or_none(v: str):
    if v is None or v == "" or v.lower() == "null":
        return None
    try:
        return float(v)
    except ValueError:
        return None


def bool_or_false(v: str):
    return str(v).strip().lower() in ("true", "1", "yes")


def main(argv):
    # argv[0] script, argv[1] cells.json, argv[2..15] = 14 values
    if len(argv) != 16:
        print(f"emit_cell: expected 15 args (cells.json + 14), got {len(argv) - 1}", file=sys.stderr)
        return 2
    (
        cells_path,
        surface,
        protocol,
        file_tag,
        wall_clock_s,
        baseline_s,
        speedup,
        band_floor,
        band_ceiling,
        sha_src,
        sha_rt,
        byte_identical,
        expected_kind,
        exit_code,
        passed,
    ) = argv[1:16]

    path = Path(cells_path)
    cells = json.loads(path.read_text()) if path.exists() else []
    cells.append(
        {
            "surface": surface,
            "protocol": protocol,
            "file": file_tag,
            "wall_clock_s": f_or_none(wall_clock_s) or 0.0,
            "baseline_wall_clock_s": f_or_none(baseline_s),
            "speedup": f_or_none(speedup),
            "speedup_band_floor": f_or_none(band_floor),
            "speedup_band_ceiling": f_or_none(band_ceiling),
            "sha256_source": sha_src,
            "sha256_roundtrip": sha_rt,
            "byte_identical": bool_or_false(byte_identical),
            "expected_kind": expected_kind,
            "exit_code": int(exit_code) if exit_code.strip() else None,
            "passed": bool_or_false(passed),
        }
    )
    path.write_text(json.dumps(cells, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

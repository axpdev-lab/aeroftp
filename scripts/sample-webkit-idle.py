#!/usr/bin/env python3
"""Read-only Linux renderer sampler; CPU 100% means one fully used core."""
import argparse
import datetime
import json
import os
from pathlib import Path
import time


def snapshot(pid):
    root = Path('/proc') / str(pid)
    raw = (root / 'stat').read_text()
    fields = raw[raw.rfind(')') + 2:].split()
    status = dict(line.split(':', 1) for line in (root / 'status').read_text().splitlines())
    return {
        'identity': fields[19],
        'ticks': int(fields[11]) + int(fields[12]),
        'rss_kib': int(status['VmRSS'].split()[0]),
        'swap_kib': int(status.get('VmSwap', '0').split()[0]),
        'threads': int(status['Threads']),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('pid', type=int)
    parser.add_argument('--interval', type=float, default=5)
    parser.add_argument('--samples', type=int, default=12)
    args = parser.parse_args()
    if args.interval <= 0 or args.samples < 1:
        parser.error('interval and samples must be positive')
    previous = snapshot(args.pid)
    last = time.monotonic()
    for _ in range(args.samples):
        time.sleep(args.interval)
        current = snapshot(args.pid)
        now = time.monotonic()
        if current['identity'] != previous['identity']:
            raise SystemExit('PID reused; stopping')
        print(json.dumps({
            'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
            'pid': args.pid,
            'cpu_percent': round((current['ticks'] - previous['ticks']) /
                                 os.sysconf('SC_CLK_TCK') / (now - last) * 100, 2),
            **{key: current[key] for key in ('rss_kib', 'swap_kib', 'threads')},
        }), flush=True)
        previous, last = current, now


if __name__ == '__main__':
    main()

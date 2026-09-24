#!/usr/bin/env python3
"""Self-tests for the capture instruments: rsync_proxy.py and decode_rsync_wire.py.

The wire evidence of the multi-entry work (appendix documents 12 and 13) is
only as good as the proxy that recorded it and the decoder that read it, so
both are tested here, each case against the defect it guards:

- the proxy must not deadlock when both peers write before reading (it did,
  twice: first with blocking writes, then with a back-pressure gate that
  stopped reading both directions at once);
- the proxy must deliver output that is still queued when the server exits
  (the idle-exit branch once dropped it);
- the decoder must refuse, not label, anything it cannot account for.

Run: python3 src-tauri/src/aerorsync/capture/test_capture_tools.py -v
PROXY_UNDER_TEST=<path> runs the proxy cases against another proxy file, which
is how each case was seen failing on the version that had the defect;
DECODER_UNDER_TEST=<path> does the same for the decoder cases.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PROXY = Path(os.environ.get("PROXY_UNDER_TEST", HERE / "rsync_proxy.py"))
DECODER = Path(os.environ.get("DECODER_UNDER_TEST", HERE / "decode_rsync_wire.py"))
FROZEN = HERE / "artifacts_real" / "frozen"


def start_proxy(server_cmd: str, capture_dir: str) -> subprocess.Popen:
    env = dict(os.environ, AEROFTP_CAPTURE_DIR=capture_dir,
               AEROFTP_SSH_ORIGINAL_COMMAND=server_cmd)
    return subprocess.Popen([sys.executable, str(PROXY)], stdin=subprocess.PIPE,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env)


class ProxyTests(unittest.TestCase):
    def setUp(self):
        self.cap = tempfile.mkdtemp(prefix="proxy-test-")

    def tearDown(self):
        shutil.rmtree(self.cap, ignore_errors=True)

    def test_full_duplex_backlog_does_not_deadlock(self):
        # Server writes 20 MiB before reading; client writes 20 MiB before
        # reading. Every pipe fills in both directions at once.
        n = 20 * 1024 * 1024
        p = start_proxy(f"head -c {n} /dev/zero; cat > /dev/null", self.cap)
        done = threading.Event()

        def writer():
            try:
                p.stdin.write(b"\0" * n)
                p.stdin.close()
                done.set()
            except BrokenPipeError:
                pass

        threading.Thread(target=writer, daemon=True).start()
        if not done.wait(30):
            p.kill()
            self.fail("proxy deadlocked: the client's write never completed")
        out = p.stdout.read()
        p.wait(timeout=30)
        self.assertEqual(len(out), n)
        self.assertEqual(os.path.getsize(Path(self.cap) / "capture_in.bin"), n)
        self.assertEqual(os.path.getsize(Path(self.cap) / "capture_out.bin"), n)

    def test_output_queued_at_server_exit_is_delivered(self):
        # Server writes 200 KiB and exits at once; the client keeps its stdin
        # open and reads only after 2.5 s, longer than the proxy's 1 s idle
        # poll, so the server is gone while bytes are still queued.
        n = 200 * 1024
        p = start_proxy(f"head -c {n} /dev/zero", self.cap)
        time.sleep(2.5)
        out = p.stdout.read()
        p.stdin.close()
        p.wait(timeout=30)
        self.assertEqual(len(out), n, "bytes queued for the client were dropped at server exit")
        self.assertEqual(os.path.getsize(Path(self.cap) / "capture_out.bin"), n)


class DecoderTests(unittest.TestCase):
    """Built on the frozen p1-up capture; each case damages a copy of it."""

    def setUp(self):
        self.src = FROZEN / "b0c0" / "p1-up"
        self.work = Path(tempfile.mkdtemp(prefix="decoder-test-"))
        shutil.copytree(self.src, self.work / "cap")
        self.cap = self.work / "cap"

    def tearDown(self):
        shutil.rmtree(self.work, ignore_errors=True)

    def decode(self, path: Path, *extra) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(DECODER), str(path), *extra],
                              capture_output=True, text=True, timeout=120)

    def test_intact_capture_decodes_to_the_last_byte(self):
        r = self.decode(self.cap)
        self.assertEqual(r.returncode, 0, r.stdout[-400:])
        self.assertNotIn("!! STOP", r.stdout)

    def test_truncated_capture_is_refused(self):
        f = self.cap / "capture_out.bin"
        f.write_bytes(f.read_bytes()[:-3])
        r = self.decode(self.cap)
        self.assertEqual(r.returncode, 3)
        self.assertIn("!! STOP", r.stdout)

    def test_unknown_mux_tag_is_refused(self):
        # capture_out: 4-byte version, 2-byte compat varint, 36-byte checksum
        # vstring, 4-byte seed; the first mux header follows. Tag 50 is not
        # a message rsync defines.
        f = self.cap / "capture_out.bin"
        raw = bytearray(f.read_bytes())
        hdr = 4 + 2 + 36 + 4
        self.assertEqual(raw[hdr + 3], 7, "fixture layout changed: expected a MSG_DATA header")
        raw[hdr + 3] = 7 + 50
        f.write_bytes(bytes(raw))
        r = self.decode(self.cap)
        self.assertEqual(r.returncode, 3)
        self.assertIn("unsupported mux tag 50", r.stdout)

    def test_compressed_stream_is_refused_by_name(self):
        r = self.decode(FROZEN / "upload")
        self.assertEqual(r.returncode, 3)
        self.assertIn("(-z) is not modelled", r.stdout)

    def test_every_frozen_campaign_capture_decodes(self):
        runs = sorted(p for p in (FROZEN / "b0c0").iterdir() if p.is_dir())
        self.assertEqual(len(runs), 35)
        for run in runs:
            extra = ["--files-from"] if run.name == "c11-dl" else []
            if run.name.startswith(("p8", "p9big")):
                extra = ["--max-entries", "2", "--max-items", "2"]
            with self.subTest(run=run.name):
                r = self.decode(run, *extra)
                self.assertEqual(r.returncode, 0, r.stdout[-300:])


class CampaignTests(unittest.TestCase):
    def test_argv_is_sanitized_on_decoded_strings(self):
        sys.path.insert(0, str(HERE))
        import b0c0_wire_campaign as c
        argv = ["rsync", "-e", f"ssh -i {c.CAPTURE}/keys/id_ed25519", "--filter=- tmp/",
                f'{c.CAPTURE}/src/"quoted"\\path']
        out = json.loads(c.sanitize_argv_json(json.dumps(argv).encode()))
        self.assertEqual(out[3], "--filter=- tmp/")
        self.assertFalse(any(str(c.CAPTURE) in a for a in out), out)
        self.assertEqual(out[4], '<capture>/src/"quoted"\\path')


if __name__ == "__main__":
    unittest.main()

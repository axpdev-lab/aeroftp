#!/usr/bin/env python3
"""Run a real CLI get without a destination, including on native Windows CI."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from threading import Thread
from urllib.parse import urlsplit
from xml.sax.saxutils import escape


PAYLOAD = b"Requested 161: default get destination\n"


class Handler(BaseHTTPRequestHandler):
    def reply(self, status, body=b"", content_type="application/octet-stream"):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_PROPFIND(self):
        path = urlsplit(self.path).path
        is_dir = path.endswith("/")
        kind = "<d:collection/>" if is_dir else ""
        body = (f'<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response>'
                f'<d:href>{escape(path)}</d:href><d:propstat><d:prop>'
                f'<d:resourcetype>{kind}</d:resourcetype>'
                f'<d:getcontentlength>{0 if is_dir else len(PAYLOAD)}</d:getcontentlength>'
                '<d:getlastmodified>Wed, 21 Oct 2015 07:28:00 GMT</d:getlastmodified>'
                '</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>'
                '</d:response></d:multistatus>').encode()
        self.reply(207, body, "application/xml")

    def do_GET(self):
        self.reply(200, PAYLOAD) if urlsplit(self.path).path == "/c/d/ok.txt" else self.reply(404)

    do_HEAD = do_GET

    def log_message(self, *_):
        pass


def main():
    source = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="get-cwd-", dir=source.parent) as directory:
        root = Path(directory)
        cli = root / source.name
        try:
            os.link(source, cli)
        except OSError:
            shutil.copy2(source, cli)
        (root / "portable.marker").touch()
        destination = root / "downloads"
        destination.mkdir()
        env = dict(os.environ, AEROFTP_MASTER_PASSWORD="synthetic get cwd fixture",
                   XDG_CONFIG_HOME=str(root / "config"), XDG_DATA_HOME=str(root / "data"),
                   XDG_CACHE_HOME=str(root / "cache"), DBUS_SESSION_BUS_ADDRESS="unix:path=/nonexistent-get-cwd")
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            url = f"http://fixture:synthetic@127.0.0.1:{server.server_port}/"
            result = subprocess.run([str(cli), "--no-banner", "--timeout", "15", "get", url,
                                     "/c/d/ok.txt", "--quiet"], cwd=destination, env=env,
                                    capture_output=True, text=True, timeout=40)
            assert result.returncode == 0, (result.returncode, result.stdout, result.stderr)
            assert (destination / "ok.txt").read_bytes() == PAYLOAD
            assert sorted(path.name for path in destination.iterdir()) == ["ok.txt"]
            print("Full remote path with no local destination downloads into cwd with exact bytes.")
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()

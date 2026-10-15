#!/usr/bin/env python3
"""Loopback FTP faults for the real GUI Mirror kept-folder check.

Needs pyftpdlib. The client still talks to an actual FTP server and filesystem:
one rename fails, one entry is hidden from listings, and RMD uses os.rmdir.
"""
import argparse
import json
from pathlib import Path

from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.filesystems import AbstractedFS
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.servers import FTPServer


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--events", required=True, type=Path)
    parser.add_argument("--ready", required=True, type=Path)
    args = parser.parse_args()
    root = args.root.resolve(strict=True)

    class Filesystem(AbstractedFS):
        def listdir(self, path):
            return [name for name in super().listdir(path) if name != "invisible.keep"]

        def rename(self, source, destination):
            if Path(source).name == "only-copy.txt":
                raise PermissionError(13, "Fixture backup move denied")
            return super().rename(source, destination)

    class Handler(FTPHandler):
        abstracted_fs = Filesystem

        def log_cmd(self, cmd, arg, respcode, respstr):
            if cmd in ("DELE", "RMD", "RNFR", "RNTO", "MLSD", "LIST"):
                with args.events.open("a") as stream:
                    stream.write(json.dumps({
                        "cmd": cmd, "path": str(Path(arg).relative_to(root)),
                        "code": respcode, "reply": respstr,
                    }) + "\n")
            super().log_cmd(cmd, arg, respcode, respstr)

    authorizer = DummyAuthorizer()
    authorizer.add_user("mirror-test", "mirror-artificial-password", str(root), perm="elradfmwMT")
    Handler.authorizer = authorizer
    server = FTPServer(("127.0.0.1", 0), Handler)
    args.ready.write_text(str(server.socket.getsockname()[1]))
    try:
        server.serve_forever()
    finally:
        server.close_all()


if __name__ == "__main__":
    main()

"""Minimal MLSD/MLST-speaking FTP server for the listing tests.

pyftpdlib is used rather than a packaged daemon for one reason: it is a
library, so a handler can be subclassed to emit listing rows of our choosing.
That is what makes the "no row could be read" case reachable at all, since a
real server always emits a dialect its own client can parse.
"""

from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.servers import FTPServer

authorizer = DummyAuthorizer()
authorizer.add_user("testuser", "testpass", "/workdir", perm="elradfmw")

handler = FTPHandler
handler.authorizer = authorizer
handler.passive_ports = range(30100, 30110)
# The advertised address must stay literal so the mapped ports are reachable
# from the host loopback, exactly as in the vsftpd fixture.
handler.masquerade_address = "127.0.0.1"
handler.banner = "aeroftp mlsd fixture ready"

FTPServer(("0.0.0.0", 21), handler).serve_forever()

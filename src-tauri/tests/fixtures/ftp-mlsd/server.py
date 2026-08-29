"""Minimal MLSD/MLST-speaking FTP server for the listing tests.

pyftpdlib is used rather than a packaged daemon for one reason: it is a
library, so a handler can be subclassed to emit listing rows of our choosing.
That is what makes the "no row could be read" case reachable at all, since a
real server always emits a dialect its own client can parse.
"""

import os

from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler
from pyftpdlib.servers import FTPServer

# A RETR of this name answers 150, leaves the data connection open, sends
# nothing, closes nothing, and then refuses on the control channel.
#
# That is the state a hanging transfer was captured in: the reply already
# waiting unread on the control socket, the data socket open and silent, and
# no timer armed on either. It could not be reproduced in the lab, because
# every server available here closes the data connection when it has nothing
# to send, and a closed socket ends the wait by itself.
#
# The condition is therefore not "a remote link" but "a server that does not
# close". Once that is said plainly it can be asked for, and a defect nobody
# could recreate becomes a line in a test.
STALL_NAME = "stall-forever.bin"
STALL_REFUSAL_DELAY = 0.5


class StallMixin:
    def ftp_RETR(self, file):
        if os.path.basename(file) != STALL_NAME:
            return super().ftp_RETR(file)
        # 150 first: the client is told the transfer is starting, so it stops
        # reading the control channel and waits on data. Everything after this
        # point is what the client cannot see.
        self.respond("150 File status okay. About to open data connection.")
        self.ioloop.call_later(
            STALL_REFUSAL_DELAY,
            lambda: self.respond("550 Failed to open file."),
            _errback=self.handle_error,
        )
        # No data is pushed and the passive socket is left as it is: from the
        # client's side the data connection is established and mute.


authorizer = DummyAuthorizer()
authorizer.add_user("testuser", "testpass", "/workdir", perm="elradfmw")

# TLS is opt-in through the environment so one image serves both transports.
# The control channel being encrypted is not a detail here: a defence that
# inspects the raw control socket sees TLS records rather than FTP replies, so
# the same test has to be runnable both ways to show the difference.
if os.environ.get("AEROFTP_FIXTURE_TLS") == "1":
    from pyftpdlib.handlers import TLS_FTPHandler

    # The mixin comes first so its `ftp_RETR` wins, and the TLS handler is the
    # base so its own setup runs: the other order leaves the data channel
    # without its handshake and the client is reset before it ever reaches
    # RETR, which looks like a finding about the client and is a fault in the
    # fixture.
    class Handler(StallMixin, TLS_FTPHandler):
        certfile = "/cert.pem"

else:

    class Handler(StallMixin, FTPHandler):
        pass


handler = Handler
handler.authorizer = authorizer
handler.passive_ports = range(30100, 30110)
# The advertised address must stay literal so the mapped ports are reachable
# from the host loopback, exactly as in the vsftpd fixture.
handler.masquerade_address = "127.0.0.1"
handler.banner = "aeroftp mlsd fixture ready"

FTPServer(("0.0.0.0", 21), handler).serve_forever()

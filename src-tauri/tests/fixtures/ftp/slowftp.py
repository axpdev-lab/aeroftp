#!/usr/bin/env python3
"""Minimal FTP server for exercising the branches a real server hides.

Written by hand rather than on pyftpdlib for one reason: every branch this has
to reach is decided by exactly what the server says and when it says it, so the
fixture has to own every byte and every pause. Subclassing a library to make it
slow enough, and to make one command hang while the rest works, is more fiddly
than writing the twelve replies the client actually needs.

Three FEAT positions, because the listing branch is chosen by the server:
  nomlsd     FEAT omits MLSD          -> client must fall back to LIST
  mlsd       FEAT advertises MLSD     -> client takes the MLSD branch
  mlsd-hang  FEAT advertises MLSD and then never serves it
             -> the client marks it broken, may reconnect, and continues to
                LIST inside the SAME call: one list() opening two data channels.
                That path has no test today.

The include_hidden axis is NOT the server's: it is our own `LIST -a`, so this
serves dotfiles only when the client asks for them, and the two branches are
exercised by calling two different operations against one server.

--list-delay makes the listing arrive slowly without ever failing, so a total
(not idle) timeout can be tested for truncation: the rows are real and correct,
they simply take longer than the limit. What is lost is real data.

--late-final is the part that makes the third case possible, and it is the whole
point. When our client gives up reading, this server does NOT close and does NOT
cancel: it finishes its work and sends the closing 226 late, on the same control
connection. That reply is then sitting in the socket when the NEXT command asks
its own question, and the next command reads it as its own answer. A fixture that
tears down at the deadline cannot produce this: the green it returns would only
be saying "the fixture closed", which is the earlier trap wearing a new face.
"""
import argparse, os, select, socket, ssl, threading, time, sys

def log(msg):
    sys.stderr.write("[slowftp %.2f] %s\n" % (time.time() % 1000, msg))
    sys.stderr.flush()

class Counters:
    """What the fixture actually DID, in a form a test can assert on.

    A test that drives the client cannot see the server log, so without this it
    would stay green if the fixture regressed to never reading the ABOR at all:
    at the client, "took it and said nothing" and "never read it" are the same
    silence. That regression is not hypothetical, this fixture had it.

    The file is rewritten at startup, so a stale file from an earlier run can
    never be mistaken for this run's result.
    """
    def __init__(self, path):
        self.path = path
        self.lock = threading.Lock()
        self.abor_read = 0
        self.stor_bytes = 0
        self.unsolicited_sent = 0
        self.pasv_announced = 0
        self.data_accepted = 0
        self.write()

    def write(self):
        if not self.path:
            return
        tmp = self.path + ".tmp"
        with open(tmp, "w") as fh:
            fh.write('{"abor_read": %d, "stor_bytes": %d, "unsolicited_sent": %d, '
                     '"pasv_announced": %d, "data_accepted": %d}\n'
                     % (self.abor_read, self.stor_bytes, self.unsolicited_sent,
                        self.pasv_announced, self.data_accepted))
        os.replace(tmp, self.path)  # atomic: a reader never sees a half file

    def bump_abor(self):
        with self.lock:
            self.abor_read += 1
            self.write()

    def bump_unsolicited(self):
        """Counted for the same reason `abor_read` is.

        If the injection never fired, a test asserting "the size is still right
        afterwards" would pass without ever having exercised anything. Asserting
        `unsolicited_sent == 1` is what stops that test from being green and
        empty, which is the failure this whole fixture keeps running into.
        """
        with self.lock:
            self.unsolicited_sent += 1
            self.write()

    def bump(self, field):
        with self.lock:
            setattr(self, field, getattr(self, field) + 1)
            self.write()

    def set_stor_bytes(self, n):
        with self.lock:
            self.stor_bytes = n
            self.write()


class Session(threading.Thread):
    def __init__(self, conn, addr, cfg, counters):
        super().__init__(daemon=True)
        self.conn, self.addr, self.cfg = conn, addr, cfg
        self.counters = counters
        self.f = conn.makefile("rwb", buffering=0)
        self.data_sock = None
        self.cwd = "/"
        self.tls = False
        self.prot_private = False
        self.pending_silent = []

    def send(self, line):
        self.f.write((line + "\r\n").encode())

    def maybe_inject(self, sent, already):
        """Send a refusal on the control channel NOBODY ASKED FOR.

        This is the path where a client is supposed to peek at the control
        channel and consume the reply only once it has decided to fail. A client
        that consumes it speculatively cannot be caught in the act, because the
        read happens entirely on its own side of the socket: it is caught by
        what comes next, a session shifted by one reply.
        """
        if already or not self.cfg.unsolicited_refusal_after:
            return already
        if sent < self.cfg.unsolicited_refusal_after:
            return already
        # The marker matters: if a test ever goes red here, the first question is
        # whether the reply that got consumed was THIS one or an ordinary one,
        # and a generic 550 cannot answer it.
        self.send("%d SLOWFTP-INJECTED unsolicited refusal at %d bytes"
                  % (self.cfg.unsolicited_code, sent))
        self.counters.bump_unsolicited()
        log("INJECTED %d SLOWFTP-INJECTED after %d bytes, unasked"
            % (self.cfg.unsolicited_code, sent))
        return True

    def hold_unserved(self, what):
        """Hold an opening that was announced and never accepted, then release it.

        There are two ways this fixture leaves a client waiting on a data
        channel, and only one of them had a bound. When `accept_data()` runs and
        returns nothing, the `if c is None` guard holds and releases. But RETR
        takes an earlier branch that never calls `accept_data()` at all, so that
        guard was unreachable there and `--pending-hold` bounded nothing on the
        download path: measured, a `get` sat for the full 400s ceiling while a
        `ls` against the same fixture came back in two holds.

        That is the same shape as the crash this file already documents: a guard
        placed on a path an earlier branch returns before reaching. It was
        introduced by the fix for that crash, which is worth saying plainly.

        Here the client is parked in the listening socket's backlog rather than
        on an accepted socket, so the release is closing the listener.
        """
        log("%s: holding the unserved opening %ss" % (what, self.cfg.pending_hold))
        self.wait_watching_control(self.cfg.pending_hold)
        if self.data_sock is not None:
            try:
                self.data_sock.close()
            except OSError:
                pass
            self.data_sock = None
        self.release_pending()

    def release_pending(self):
        """End a deliberate hang by closing the sockets nobody ever answered.

        Holding the handler is not enough: the client is stuck in a TLS
        handshake on the DATA socket, and letting the server-side wait expire
        does nothing to it. Measured: the handler released after 6s and the
        client was still waiting at 40. So the hold has to end the way the
        client can observe, which is the socket closing.

        The closure is an EOF, which elsewhere in this fixture is the accident
        to avoid. Here it is the intended end of a bounded hang, and the
        difference is only that it happens when the option says it should.
        """
        for s in self.pending_silent:
            try:
                s.close()
            except OSError:
                pass
        self.pending_silent = []
        # And the listener this path opened in order to accept. Closing what was
        # accepted and leaving open what was opened to accept it is the same
        # omission this function was written to fix, one object further out: the
        # listener survives until the control session ends or the next PASV
        # overwrites the reference, so several data operations on one session
        # accumulate them. Measured: 2 sockets before, 4 after one cycle.
        if self.data_sock is not None:
            try:
                self.data_sock.close()
            except OSError:
                pass
            self.data_sock = None

    def wait_watching_control(self, seconds):
        """Sleep, but keep taking commands that arrive meanwhile.

        A bare `time.sleep` here makes the fixture DEAF: an ABOR sent during a
        stall would sit unread, and the client cannot tell "the server took my
        command and said nothing" from "the server never read it". Those are
        the same silence at one end and two different servers at the other, and
        this fixture exists to be the second end.
        """
        end = time.time() + seconds
        while True:
            left = end - time.time()
            if left <= 0:
                return
            r, _, _ = select.select([self.conn], [], [], min(left, 0.2))
            if r:
                self.poll_control()

    def poll_control(self):
        """Consume a command that arrives WHILE a data transfer is running.

        Both anomalies 25 asked for are a reply (or a silence) on the control
        channel while the data channel is open, and this server is otherwise
        strictly sequential: without this, an ABOR sent mid-transfer would sit
        unread until the handler returned, which is not what a real server does.

        Note for anyone tempted to drop this and rely on the socket buffer: at
        the client the two are indistinguishable, because bytes land in the
        receive buffer whether or not anyone reads them. It is kept because the
        fixture should do what it claims, not merely look the same from one end.
        """
        r, _, _ = select.select([self.conn], [], [], 0)
        if not r:
            return None
        # PEEK, never a bare read. Consuming whatever turned up would swallow
        # the ordinary commands a client sends while a transfer is in flight,
        # and then answer none of them. It also destroys the stale-reply case:
        # the retry's SIZE would be eaten here instead of arriving after the
        # late 226, and the desync this fixture exists to show would vanish.
        # Verified the hard way: an earlier version read unconditionally and
        # the reproduction stopped reproducing.
        if self.tls:
            # MSG_PEEK cannot see through TLS framing, and reading here would
            # consume commands this fixture must leave alone. Declared rather
            # than silently skipped: under TLS the ABOR case does not work, and
            # `abor_read` staying 0 is the honest answer, not a regression.
            return None
        try:
            peeked = self.conn.recv(512, socket.MSG_PEEK)
        except (BlockingIOError, ConnectionResetError):
            return None
        if b"\n" not in peeked:
            return None
        line = peeked.split(b"\n", 1)[0].decode("utf-8", "replace").strip()
        cmd = line.split(" ")[0].upper()
        if cmd != "ABOR":
            # Not ours: leave every byte where it was for the normal loop.
            return None
        self.f.readline()  # now consume it, it really is an ABOR
        self.counters.bump_abor()
        log("<- (during transfer) %s" % line)
        if cmd == "ABOR":
            if self.cfg.abor_silent:
                # CASE A: take the command and say NOTHING. The client's abort
                # stays pending inside its own budget, which is the only state
                # in which a future dropped mid-abort can be observed.
                log("ABOR received: staying silent on purpose")
            else:
                self.send("226 Abort successful.")
        return cmd

    def entries(self, show_hidden):
        n = self.cfg.lines
        names = ["file-%03d.txt" % i for i in range(n)]
        if show_hidden:
            names = [".hidden-a", ".hidden-b"] + names
        return names

    def open_pasv(self):
        """Announce a data port, and optionally never serve it.

        Three ways a data connection can fail to happen, and they are NOT
        interchangeable: the client sees a different failure in each.

          tls-silent    accept the TCP and then answer nothing. This is the one
                        measured live: the client sends a TLS ClientHello and
                        gets zero bytes back. Needs no TLS here, since the point
                        is that nothing ever answers.
          connect-hang  listen with a full accept queue, so the kernel drops the
                        SYN and connect() itself hangs. This is the plaintext
                        case, where the wait is one step earlier still.
          refuse        announce a port with nothing listening: connect() gets
                        RST and fails at once. Not a hang, and worth having as
                        the contrast that shows the other two ARE hangs.
        """
        mode = self.cfg.pasv_no_accept
        s = socket.socket()
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        if mode == "refuse":
            s.close()  # the port is announced, nothing is listening on it
            self.data_sock = None
        elif mode == "connect-hang":
            s.listen(0)
            # Fill the accept queue so the next SYN is dropped rather than
            # completed. Without this the kernel would finish the handshake on
            # its own and connect() would return, which is a different case.
            filler = socket.socket()
            try:
                filler.settimeout(1)
                filler.connect(("127.0.0.1", port))
            except Exception:
                pass
            self.data_sock = s
            self._filler = filler
        else:
            s.listen(1)
            self.data_sock = s
        self.counters.bump("pasv_announced")
        log("PASV announced port %d (mode=%s)" % (port, mode))
        self.send("227 Entering Passive Mode (127,0,0,1,%d,%d)." % (port >> 8, port & 0xFF))

    def accept_data(self):
        """Accept the data connection, and wrap it in TLS unless told not to.

        `--pasv-no-accept tls-silent` with TLS on is the live case and the whole
        reason this fixture learned to speak TLS: the TCP is accepted, so the
        client's connect() succeeds, and then the handshake is never answered.
        The client sits inside the handshake, which is BEFORE the first data
        read, so a guard placed on the read loop never runs. In plaintext this
        case cannot exist, and the fixture would exercise the fixed path while
        looking like it exercised the broken one.
        """
        self.data_sock.settimeout(20)
        try:
            c, _ = self.data_sock.accept()
        except (socket.timeout, TimeoutError, OSError):
            # A client that asks for PASV and then never connects is a legitimate
            # thing for a server to survive. Letting this propagate killed the
            # session thread, which is the same failure this file keeps meeting:
            # the fixture dies and everything after it looks like a server that
            # stopped answering. Treated as "no data channel", which the callers
            # already know how to hold and release.
            log("nobody connected to the data port, treating as no data channel")
            return None
        self.counters.bump("data_accepted")
        if self.tls and self.prot_private:
            if self.cfg.pasv_no_accept == "tls-silent":
                # Keep a reference. Dropping this socket has it garbage
                # collected and closed, so the client gets EOF on its handshake
                # and fails FAST, which is the opposite of what this mode is for
                # and still looks like a working fixture.
                self.pending_silent.append(c)
                log("data channel accepted, TLS handshake deliberately NOT answered")
                return None  # held open, silent: the live hang
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            ctx.load_cert_chain(self.cfg.tls_cert, self.cfg.tls_key)
            c = ctx.wrap_socket(c, server_side=True)
        return c

    def serve_lines(self, lines):
        """Serve the data channel, then close the FINAL reply late if asked.

        The control connection is never touched here: whatever the client does
        with its own deadline, this side keeps the session alive and still owes
        it a 226. Delivering that debt late is what plants the stale reply.
        """
        c = self.accept_data()
        if c is None:
            # tls-silent: there is no usable data channel by design.
            # Touching it here raised AttributeError and killed the
            # session thread, which is the same failure as the broken
            # pipe: the fixture dies and the client gets a prompt EOF
            # instead of the wait it is supposed to see.
            log("no data channel by design, holding %ss" % self.cfg.pending_hold)
            self.wait_watching_control(self.cfg.pending_hold)
            self.release_pending()
            return
        try:
            per_row = self.cfg.list_delay
            if self.cfg.list_total and lines:
                per_row = self.cfg.list_total / float(len(lines))
            try:
                for ln in lines:
                    c.sendall((ln + "\r\n").encode())
                    if per_row:
                        time.sleep(per_row)
            except (BrokenPipeError, ConnectionResetError):
                log("LIST: client dropped the data channel, still owing a reply")
        finally:
            c.close()
            self.data_sock.close()
            self.data_sock = None
        if self.cfg.late_final:
            log("data done, holding the 226 for %ss (client has probably given up)"
                % self.cfg.late_final)
            self.wait_watching_control(self.cfg.late_final)
            log("hold finished, the 226 goes out now")

    def run(self):
        try:
            self.serve()
        except (BrokenPipeError, ConnectionResetError):
            log("control connection dropped by the client")

    def serve(self):
        self.send("220 (slowftp fixture)")
        while True:
            raw = self.f.readline()
            if not raw:
                return
            line = raw.decode("utf-8", "replace").strip()
            if not line:
                continue
            cmd, _, arg = line.partition(" ")
            cmd = cmd.upper()
            log("<- %s" % line)

            if cmd == "USER":   self.send("331 Please specify the password.")
            elif cmd == "PASS": self.send("230 Login successful.")
            elif cmd == "AUTH":
                if not self.cfg.tls_cert:
                    self.send("500 AUTH not available, fixture started without --tls-cert")
                elif arg.strip().upper() not in ("TLS", "TLS-C", "SSL"):
                    self.send("504 Unsupported AUTH type.")
                else:
                    self.send("234 AUTH TLS OK, starting TLS.")
                    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                    ctx.load_cert_chain(self.cfg.tls_cert, self.cfg.tls_key)
                    self.conn = ctx.wrap_socket(self.conn, server_side=True)
                    self.f = self.conn.makefile("rwb", buffering=0)
                    self.tls = True
                    log("control channel upgraded to TLS")
            elif cmd == "PBSZ": self.send("200 PBSZ=0")
            elif cmd == "PROT":
                self.prot_private = arg.strip().upper() == "P"
                self.send("200 PROT now %s." % ("P" if self.prot_private else "C"))
            elif cmd == "SYST": self.send("215 UNIX Type: L8")
            elif cmd == "TYPE": self.send("200 Switching to Binary mode.")
            elif cmd == "PWD":  self.send('257 "%s" is the current directory' % self.cwd)
            elif cmd == "CWD":
                self.cwd = arg if arg.startswith("/") else self.cwd.rstrip("/") + "/" + arg
                self.send("250 Directory successfully changed.")
            elif cmd == "FEAT":
                feats = ["211-Features:", " EPSV", " PASV", " SIZE", " TVFS", " UTF8"]
                if self.cfg.tls_cert and not self.tls:
                    feats[1:1] = [" AUTH TLS", " PBSZ", " PROT"]
                if self.cfg.feat in ("mlsd", "mlsd-hang"):
                    feats.insert(1, " MLSD")
                feats.append("211 End")
                for x in feats: self.send(x)
            elif cmd == "PASV": self.open_pasv()
            elif cmd == "MLSD":
                if self.cfg.feat == "mlsd-hang":
                    # Announced and then not served. The control channel stays
                    # silent: no 150, no error, nothing to read.
                    log("MLSD requested: hanging on purpose, no reply")
                    time.sleep(self.cfg.hang)
                    self.send("425 Cannot open data connection.")
                else:
                    self.send("150 Here comes the directory listing.")
                    rows = ["type=dir;sizd=4096; ." , "type=pdir;sizd=4096; .."]
                    rows += ["type=file;size=10; %s" % n for n in self.entries(True)]
                    self.serve_lines(rows)
                    self.send("226 Directory send OK.")
            elif cmd == "LIST":
                show_hidden = "-a" in arg.split()
                self.send("150 Here comes the directory listing.")
                rows = ["-rw-r--r--    1 u  g  10 Aug 30 01:00 %s" % n
                        for n in self.entries(show_hidden)]
                self.serve_lines(rows)
                self.send("226 Directory send OK.")
            elif cmd == "STOR":
                # CASE B: does a server that refuses a STOR also stop reading?
                # Nothing in the protocol says so. This server can do either,
                # so the assumption becomes a measurement instead of a sentence
                # in a PR body.
                self.send("150 Ok to send data.")
                c = self.accept_data()
                if c is None:
                    # tls-silent: there is no usable data channel by design.
                    # Touching it here raised AttributeError and killed the
                    # session thread, which is the same failure as the broken
                    # pipe: the fixture dies and the client gets a prompt EOF
                    # instead of the wait it is supposed to see.
                    log("no data channel by design, holding %ss" % self.cfg.pending_hold)
                    self.wait_watching_control(self.cfg.pending_hold)
                    self.release_pending()
                    continue
                total = 0
                refused = False
                try:
                    while True:
                        self.poll_control()
                        if self.cfg.stor_read_rate:
                            # Without this the case is not measurable on
                            # loopback: the client writes the whole file into
                            # the socket buffers before the refusal can matter,
                            # so "it uploaded everything" would be a property of
                            # the buffers and not of the client. Throttling the
                            # READ makes the client's writes actually block, and
                            # only then does it mean something that it did or did
                            # not notice the 552 while writing.
                            time.sleep(65536.0 / self.cfg.stor_read_rate)
                        chunk = c.recv(65536)
                        if not chunk:
                            break
                        total += len(chunk)
                        if (self.cfg.stor_refuse_after
                                and not refused
                                and total >= self.cfg.stor_refuse_after):
                            refused = True
                            log("STOR: refusing after %d bytes (%s)"
                                % (total, self.cfg.stor_after_refuse))
                            self.send("552 Requested file action aborted: "
                                      "exceeded storage allocation.")
                            if self.cfg.stor_after_refuse == "stop":
                                # Stop reading but leave the socket OPEN, so the
                                # client's writes fill the window and block.
                                # Closing here would deliver EPIPE instead, which
                                # is a different failure and would answer a
                                # question nobody asked.
                                log("STOR: no longer reading, socket left open %ss"
                                    % self.cfg.stor_stop_hold)
                                self.wait_watching_control(self.cfg.stor_stop_hold)
                                break
                            # else: keep draining to the end, which is the half
                            # where the whole file goes up for nothing.
                except (BrokenPipeError, ConnectionResetError):
                    log("STOR: client dropped the data channel after %d bytes" % total)
                finally:
                    c.close()
                    self.data_sock.close()
                    self.data_sock = None
                log("STOR: %d bytes read, refused=%s" % (total, refused))
                self.counters.set_stor_bytes(total)
                if not refused:
                    self.send("226 Transfer complete.")
            elif cmd == "ABOR":
                self.counters.bump_abor()
                if self.cfg.abor_silent:
                    log("ABOR received outside a transfer: staying silent")
                else:
                    self.send("226 Abort successful.")
            elif cmd in ("DELE", "RMD", "MKD"):
                # Enough for the recursive delete to run end to end, which is
                # how the include_hidden axis gets exercised: that operation
                # passes include_hidden and issues `LIST -a`, a plain listing
                # does not, and this server serves the dotfiles only to `-a`.
                self.send("250 Requested file action okay, completed.")
            elif cmd == "SIZE":
                self.send("213 %d" % self.cfg.file_size)
            elif cmd == "RETR":
                # The route that actually reaches the stale reply on the main
                # road. providers/ftp.rs has no list timeout at all, so a slow
                # listing cannot strand a reply there; a download can. The
                # per-file timeout in provider_transfer_executor.rs drops the
                # download future mid-RETR, the FtpProvider survives with its
                # control stream intact, and the retry loop immediately reuses
                # it for the next attempt. So: hand out part of the file, stall
                # past their deadline, then pay the 226 late into a connection
                # they are already asking the next question on.
                if self.cfg.refuse_before_data:
                    # The refusal is ALREADY on the control channel while the
                    # client is still waiting on a data channel that will never
                    # work. That is the live signature: the answer was in our
                    # own socket buffer and nobody was looking at it.
                    self.send("550 Failed to open file.")
                    log("RETR refused up front; data channel deliberately not served")
                    if self.cfg.pasv_no_accept != "none":
                        self.hold_unserved("RETR refused up front")
                        continue  # do not touch the data channel at all
                self.send("150 Opening BINARY mode data connection.")
                if self.cfg.pasv_no_accept != "none":
                    log("RETR: data channel announced but not served (%s)"
                        % self.cfg.pasv_no_accept)
                    self.hold_unserved("RETR")
                    continue
                c = self.accept_data()
                if c is None:
                    # tls-silent: there is no usable data channel by design.
                    # Touching it here raised AttributeError and killed the
                    # session thread, which is the same failure as the broken
                    # pipe: the fixture dies and the client gets a prompt EOF
                    # instead of the wait it is supposed to see.
                    log("no data channel by design, holding %ss" % self.cfg.pending_hold)
                    self.wait_watching_control(self.cfg.pending_hold)
                    self.release_pending()
                    continue
                try:
                    sent = 0
                    injected = False
                    chunk = b"x" * 1024
                    while sent < self.cfg.retr_before_stall:
                        n = min(len(chunk), self.cfg.retr_before_stall - sent)
                        c.sendall(chunk[:n])
                        sent += n
                        injected = self.maybe_inject(sent, injected)
                    self.poll_control()
                    if self.cfg.retr_stall:
                        log("RETR: %d bytes out, stalling %ss with the channel open"
                            % (sent, self.cfg.retr_stall))
                        self.wait_watching_control(self.cfg.retr_stall)
                    rest = self.cfg.file_size - sent
                    try:
                        while rest > 0:
                            n = min(len(chunk), rest)
                            c.sendall(chunk[:n])
                            rest -= n
                            sent += n
                            injected = self.maybe_inject(sent, injected)
                    except (BrokenPipeError, ConnectionResetError):
                        # The client gave up and dropped its end of the DATA
                        # channel. A real server does not die here: it notes the
                        # aborted transfer and goes on serving the CONTROL
                        # connection, which is the whole point of this fixture.
                        # Letting the thread fall over on this write is itself a
                        # way of tearing down at the deadline, just an
                        # accidental one, and it silently removes the case.
                        log("RETR: client dropped the data channel, still owing a reply")
                finally:
                    c.close()
                    self.data_sock.close()
                    self.data_sock = None
                if self.cfg.late_final:
                    log("RETR data done, holding the 226 for %ss" % self.cfg.late_final)
                    time.sleep(self.cfg.late_final)
                self.send("226 Transfer complete.")
            elif cmd in ("QUIT",):
                self.send("221 Goodbye.")
                return
            elif cmd == "SITE" and arg.strip().upper() == "STATUS":
                # In-band twin of --status-file, for a test that can send a raw
                # command but cannot read a file (or the other way round).
                self.send("211 abor_read=%d stor_bytes=%d"
                          % (self.counters.abor_read, self.counters.stor_bytes))
            elif cmd in ("OPTS", "NOOP"): self.send("200 Ok.")
            else: self.send("502 Command not implemented.")

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=2130)
    p.add_argument("--feat", choices=["nomlsd", "mlsd", "mlsd-hang"], default="nomlsd")
    p.add_argument("--list-delay", type=float, default=0.0, help="seconds between listing rows")
    p.add_argument("--lines", type=int, default=5)
    p.add_argument("--hang", type=float, default=90.0, help="seconds MLSD stays silent in mlsd-hang")
    p.add_argument("--list-total", type=float, default=0.0,
                   help="spread the listing over this many seconds total, to sit just "
                        "under or just over a client deadline")
    p.add_argument("--file-size", type=int, default=1048576,
                   help="size RETR/SIZE report and serve")
    p.add_argument("--retr-before-stall", type=int, default=4096,
                   help="bytes handed over before RETR stalls")
    p.add_argument("--retr-stall", type=float, default=0.0,
                   help="seconds RETR holds the data channel open and silent, to outlast "
                        "the per-file download timeout")
    p.add_argument("--tls-cert", default=None,
                   help="PEM certificate: enables AUTH TLS / PBSZ / PROT, so the data "
                        "channel can accept the TCP and then never answer the handshake, "
                        "which is the live case and cannot exist in plaintext")
    p.add_argument("--tls-key", default=None, help="PEM private key for --tls-cert")
    p.add_argument("--pasv-no-accept", choices=["none", "tls-silent", "connect-hang", "refuse"],
                   default="none",
                   help="announce a PASV port and then fail to serve it, three different ways")
    p.add_argument("--refuse-before-data", action="store_true",
                   help="send 550 on the control channel BEFORE the data phase, so the "
                        "refusal is already queued while the client waits on the data channel")
    p.add_argument("--unsolicited-refusal-after", type=int, default=0,
                   help="bytes of RETR data after which the server sends a refusal on the "
                        "control channel WITHOUT being asked (0 = never). Exercises the "
                        "peek-do-not-consume path directly")
    p.add_argument("--unsolicited-code", type=int, default=550,
                   help="status code of the injected refusal (default 550)")
    p.add_argument("--status-file", default=None,
                   help="path this fixture rewrites with what it actually did "
                        '(\'{"abor_read": N, "stor_bytes": N}\'), so a test that drives '
                        "the client can assert the server read the ABOR instead of "
                        "trusting it. Rewritten at startup, so a stale file cannot pass.")
    p.add_argument("--abor-silent", action="store_true",
                   help="CASE A: accept ABOR and send no reply at all, so the abort stays "
                        "pending inside its budget")
    p.add_argument("--stor-refuse-after", type=int, default=0,
                   help="CASE B: bytes after which STOR is refused with 552 (0 = never)")
    p.add_argument("--stor-after-refuse", choices=["drain", "stop"], default="drain",
                   help="CASE B: after the 552, keep draining the data channel (drain) or "
                        "stop reading with the socket still open (stop)")
    p.add_argument("--stor-read-rate", type=int, default=0,
                   help="CASE B: bytes per second the server reads from the data channel "
                        "(0 = as fast as possible). Needed on loopback, where otherwise the "
                        "client finishes writing before the refusal can matter")
    p.add_argument("--stor-stop-hold", type=float, default=30.0,
                   help="CASE B only: seconds the STOPPED socket is held open after the "
                        "server stops reading a refused STOR")
    p.add_argument("--pending-hold", type=float, default=30.0,
                   help="seconds a LIST, RETR or STOR is held pending when tls-silent "
                        "leaves no usable data channel. Separate from --stor-stop-hold on "
                        "purpose: one is how long a stopped socket lingers, the other is how "
                        "long a deliberate hang lasts, and wanting one short and the other "
                        "long is legitimate")
    p.add_argument("--late-final", type=float, default=0.0,
                   help="after the data channel closes, wait this long before sending the "
                        "closing 226, WITHOUT closing the control connection, so the reply "
                        "is still owed when the next command asks its own question")
    cfg = p.parse_args()
    counters = Counters(cfg.status_file)
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", cfg.port))
    srv.listen(5)
    log("listening on 127.0.0.1:%d feat=%s delay=%ss total=%ss late_final=%ss lines=%d"
        % (cfg.port, cfg.feat, cfg.list_delay, cfg.list_total, cfg.late_final, cfg.lines))
    while True:
        c, a = srv.accept()
        Session(c, a, cfg, counters).start()

if __name__ == "__main__":
    main()

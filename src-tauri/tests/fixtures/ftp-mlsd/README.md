# MLSD / MLST fixture

An FTP server that speaks MLSD and MLST, which the vsftpd fixture next door does not.

```bash
docker compose up -d --build      # control on :2199, PASV 30100-30109
cd ../../.. && cargo test --test integration_ftp_mlsd -- --ignored --nocapture
cd tests/fixtures/ftp-mlsd && docker compose down -v
```

Credentials are `testuser` / `testpass`, fixture-only and deliberately hardcoded, the same as the vsftpd fixture beside it.

## Why a second FTP fixture

`list_inner_opts` tries MLSD first and only falls back to LIST, and the anti-hang MLST probe runs only when the server advertises MLST. vsftpd announces neither: `FEAT` on it answers with `MDTM` and `SIZE` only.

So the branch most servers take was never executed by any test here, and a defect lived on it through two rounds of review for no other reason. A lab that cannot reach a code path decides what can be found in it.

## Why pyftpdlib rather than a packaged daemon

It is a library, so a handler can be subclassed and told what to emit. That is what makes conditions reachable that no real server will produce on request.

`stall-forever.bin` is the one that exists today. A RETR of that name answers 150, leaves the data connection open, sends nothing, closes nothing, and then refuses on the control channel. That is the state a hanging transfer was captured in on a remote server: the reply already waiting unread on the control socket, the data socket open and silent, no timer armed on either.

It could not be reproduced here before, and the reason was not the network. Every other server available closes the data connection when it has nothing to send, and a closed socket ends the wait by itself. The condition is "a server that does not close", and once that is said plainly it can be asked for.

## TLS

`AEROFTP_FIXTURE_TLS=1` turns on explicit FTPS, so one image serves both transports. That matters because a defence which inspects the raw control socket sees TLS records rather than FTP replies, and the same test has to be runnable both ways to show the difference.

**The stall does not yet reproduce under TLS.** With the data handler never created, the client's data-channel handshake finds nobody, and the connection is reset before RETR is reached. That looks like a finding about the client and is a fault in the fixture.

Reproducing it faithfully means letting pyftpdlib create the data handler, so the data-channel handshake completes, and then suppressing the sending, rather than skipping the creation as `StallMixin` does now. Changing the inheritance order does not help: the problem is not the MRO. This is written down so the next attempt starts from the answer instead of the question.

## What is not covered yet

A listing whose rows the parser cannot read. The same technique that made the stall reachable, a handler that decides what to emit, is what would produce it: no real server emits a dialect its own client cannot parse.

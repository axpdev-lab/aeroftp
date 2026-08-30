# slowftp: the FTP server for the branches a real server hides

`slowftp.py` is a hand-written FTP server, no dependencies. It exists because the
branches we need to test are decided by exactly what the server says and when it
says it, and a real server will not say those things on demand.

Run it, point a client at `127.0.0.1:<port>`, any username and password.

## The three FEAT positions

The listing branch is chosen by the server, so it takes a server to switch it.

| `--feat` | What the server does | What our client does |
|---|---|---|
| `nomlsd` | FEAT omits MLSD | falls back to `LIST` |
| `mlsd` | FEAT advertises MLSD, MLSD works | takes the MLSD branch |
| `mlsd-hang` | FEAT advertises MLSD, MLSD never answers | marks it broken, reconnects, and runs `LIST` inside the same call |

All three verified against `aeroftp-cli ls`, entries checked, not just the path.
The third opens two data channels in one `list()` and has no test today.

## The include_hidden axis is not the server's

`LIST -a` versus `LIST` is our own parameter, not a server capability, so it is
exercised by calling two of our own operations against one server. This fixture
serves the dotfiles only when the client asks with `-a`, so the difference is
visible instead of assumed. Both operations verified against one running server:

```
aeroftp-cli ls    ->  LIST                    2 entries, no dotfiles
aeroftp-cli rm -r ->  CWD /sub
                      LIST -a                 4 entries, dotfiles included
                      CWD /
                      DELE /sub/.hidden-a
                      DELE /sub/.hidden-b
                      DELE /sub/file-000.txt
                      DELE /sub/file-001.txt
                      RMD  /sub
```

That is the `CWD` in, bare `LIST -a`, restore-the-working-directory sequence
`providers/ftp.rs:622-632` describes, doing what it says: without the `-a` the
two dotfiles would survive and the final `RMD` would fail on a directory the
client believed was empty.

## The late reply, and why the fixture must not tear down

`--late-final N` makes the server finish its work and send the closing `226`
N seconds late, **without closing the control connection**. That reply is then
sitting in the socket when the next command asks its own question, and the next
command reads it as its own answer. This is the "phantom files" defect that
`src/ftp.rs:218-221` already documents and already patches with a pre-LIST NOOP.

A fixture that closed at the deadline could not produce this case, and the green
it returned would only be saying "the fixture closed".

## Reaching that defect on the main road: RETR, not LIST

`providers/ftp.rs` (the GUI and CLI path) has **no listing timeout at all**, so a
slow listing cannot strand a reply there. A download can:
`provider_transfer_executor.rs` races each download against the user cancel token
and a per-file timeout and drops the future mid-`RETR`. The `FtpProvider`
survives with its control stream mid-reply, and the retry loop reuses that same
instance for the next attempt.

So the stall belongs on `RETR`:

```
--retr-stall N       hold the data channel open and silent for N seconds,
                     to outlast the per-file download timeout
--retr-before-stall  bytes handed over before the stall (default 4096)
--late-final N       then pay the 226 N seconds late, connection still open
```

## Sitting on a threshold

`--list-total S` spreads the listing over S seconds, so a run can be placed just
under or just over a deadline without recomputing a per-row delay. The rows are
real and correct throughout: what a total timeout costs here is real data, not
an error.

## The two numbers without which this fixture reproduces nothing

The stall has to end **after** the client's per-file timeout fires and **before**
the client gives up on the entry altogether. Against the 30s default:

| `--retr-stall` | per-file timeout | what you see |
|---|---|---|
| `35` | 30s | the late `226` lands while a retry is waiting: the desync |
| `45` | 30s | all three retries are exhausted first: **nothing at all** |

Same code, same path, same fixture. The 45s run looks clean and proves nothing.
This is the single easiest thing to lose from this file.

## Surviving the broken pipe, which is the half hour you do not have to repeat

When the client's timeout fires it drops its end of the **data** channel. The
server is still mid-`sendall`, so the write raises `BrokenPipeError`. Left
alone, that exception kills the session thread, and a dead thread never sends
the late reply.

The result is a run that looks completely clean and reproduces nothing, and the
trace from the client's side is **identical** to a correct run in which the
server simply had nothing more to say. "The server stopped talking" and "the
server had nothing left to say" are the same bytes at the client. It cost half
an hour and was found only by reading the **server** log.

So the fixture catches `BrokenPipeError` and `ConnectionResetError` in three
places, and none of them is optional:

- around the `RETR` body, so an aborted download still owes its `226`;
- around the listing loop, for the same reason on the `LIST` path;
- around the whole session loop, so a client that drops the **control**
  connection closes one session instead of taking the handler down.

The general form, worth more than the three catches: **a fixture that falls over
and a fixture that closes on purpose tear down at the same instant and produce
the same clean trace.** Whenever the instrument is a dialogue between two ends,
read the outcome from the end you are NOT measuring. The end you measure always
shows you a coherent trace, including when the other one is dead.

## Does #655 make this obsolete? No, and #655 says so itself

`#655` adds `abandon_transfer`, which cleans the session up on the way out of a
failed transfer. It would be reasonable to assume that closes the desync above
and retires this fixture. It does not, and the PR states the limit in its own
doc comment rather than leaving it to be discovered:

> It runs on exits that RETURN. A future dropped from outside never reaches it:
> dropping an async fn runs the destructors of its locals and none of the code
> that follows. The transfer executor wraps a download in `tokio::time::timeout`
> and lets the future fall on expiry, so on that path the session is left exactly
> as this function exists to prevent, and the retry that follows reuses the same
> connection.

So the reproduction stays live after `#655` lands, with the same two numbers.
It closes when the deadline moves **inside** the data loop, where expiry becomes
an ordinary error return and passes through the cleanup like every other exit.
That is where the shared data-loop primitive will put it, and this fixture is
the instrument for checking that it actually got there.

## One thing this fixture cannot do, from the same PR

> On loopback the same code returns in two seconds, because there the data
> socket closes at once and the wait ends by itself. Same server, same version,
> opposite outcome, decided by who closes first: a fixture cannot exercise this,
> which is worth knowing before trusting a green test here.

That is about the hanging download of a missing file (F9), not about the desync.
This fixture runs on loopback, so it is the wrong instrument for that one and a
green here would mean nothing. F9 is measured against a real remote.

## Two anomalies on the control channel while the data channel is open

Both were asked for by the session working on `STOR`, and they are one handler
because they are the same shape: an unusual reply, or an unusual silence, on the
control channel during a transfer.

### `--abor-silent`: take the ABOR and say nothing

The server reads the `ABOR` and sends neither `226` nor `426`, so the client's
abort stays pending inside its own budget. That is the only state in which a
future dropped mid-abort can be observed at all.

Verified: `<- (during transfer) ABOR` in the server log, and no reply at the
client after 8s.

The first version passed this test while being wrong. It never read the `ABOR`
at all, it merely never answered, and at the client those are the same silence.
Reading it required the stalls to keep watching the control channel, which is
what `wait_watching_control` is for: a bare `time.sleep` makes the fixture deaf.

### `--stor-refuse-after N`, with `--stor-after-refuse drain|stop`

Mid-`STOR` the server sends `552` on the control channel and then either keeps
draining the data channel (`drain`) or stops reading with the socket still open
(`stop`). Closing it instead would deliver `EPIPE`, a different failure that
answers a question nobody asked.

`--stor-read-rate BYTES_PER_SEC` throttles the server's reading, and on loopback
it is **not optional**: without it the client writes the whole file into the
socket buffers before the refusal can matter, so "it uploaded everything" would
be a property of the buffers rather than of the client.

Measured with the read throttled to 16 KB/s, a 300000 byte file, refusal at
65536 bytes:

| mode | bytes the server received after refusing | what ended the client |
|---|---|---|
| `drain` | **all 300000** | still running at the 300s ceiling |
| `stop` | 65536 | the data socket closing, not the `552` |

So neither half of the assumption holds up: refusing on the control channel does
not, by itself, end the upload. In `drain` the whole file goes up after the
refusal even with the write genuinely blocked. In `stop` the write does jam, but
the client waits for the socket rather than acting on a refusal it already has.

### `poll_control` peeks, it never reads

It uses `MSG_PEEK` and consumes only an `ABOR`, leaving every other byte where
it was. An earlier version read whatever had arrived, which swallowed the
ordinary commands a client sends during a transfer and answered none of them.

## Asserting what the fixture actually did, not what it looks like

A test that drives the client cannot see this server's log. Without something
more, such a test would stay green if this fixture regressed to never reading
the `ABOR` at all, because at the client "took it and said nothing" and "never
read it" are the same silence. That regression is not hypothetical: this fixture
shipped it once.

Two ways to close it, use whichever the harness can reach:

```
--status-file PATH      the fixture rewrites PATH with what it did:
                        {"abor_read": 1, "stor_bytes": 0}
SITE STATUS             the same counters in-band: 211 abor_read=1 stor_bytes=0
```

The file is rewritten **at startup**, so a stale file from an earlier run can
never be read as this run's result, and it is replaced atomically, so a reader
never sees a half-written file.

Assert `abor_read == 1`, not `>= 1`: the exact count also catches the client
sending more `ABOR`s than it should.

**What this counter does not cover, said here because it was already read as
covering it.** It increments in exactly one place: when the server *reads an
`ABOR` sent by the client*. It counts commands travelling one way. A client that
speculatively consumes a server *reply* it should have left alone does so
entirely on its own side of the socket, takes nothing away from what the server
reads, and moves no counter here. That is a different property and needs a
different instrument: speculative consumption is not observable as consumption,
only as its consequence, a session shifted by one reply from then on.

### Verified against the failure it exists to catch

The counter was checked by rebuilding the deaf regression on purpose (the stall
reverted to a bare `time.sleep`) and running the identical probe against both:

| fixture | what the client saw | `abor_read` |
|---|---|---|
| correct | silence for 6s | `1` |
| deaf (regression) | silence for 6s | `0` |

Same silence at the client, different counter. An instrument that has never been
shown to go red for the fault it is meant to report is a counter, not a guard.

## `--unsolicited-refusal-after N`: a refusal nobody asked for

The path where a client is meant to *peek* at the control channel and consume a
reply only once it has decided to fail. Mid-`RETR`, after N bytes of data, the
server sends a refusal on the control channel unasked:

```
550 SLOWFTP-INJECTED unsolicited refusal at 4096 bytes
```

The `SLOWFTP-INJECTED` marker is not decoration. When a test goes red here the
first question is whether the reply that got consumed was this one or an
ordinary one, and a bare `550` cannot answer it. `--unsolicited-code` changes
the code if 550 collides with something real.

**How the test reads it.** Speculative consumption cannot be caught in the act:
the read happens entirely on the client's side of the socket. It is caught by
what comes next. So the assertion is a question with a known answer, asked
afterwards, on a file whose size cannot coincide with anything else in play:

```
--file-size 40961      ->  SIZE must answer exactly "213 40961"
```

Not 0, not 65536, not a round number: if the assertion fails, a strange number
says immediately whether the answer belongs to this question or to a previous
one. A red on a *value* says what happened; a red on a timeout says only that
something did not arrive.

**Assert `unsolicited_sent == 1` as well.** If the injection never fired, a test
checking "the size is still right afterwards" would pass without exercising
anything at all. That is the same green-and-empty failure this fixture keeps
walking into, so the counter is in the status file next to the others.

Verified: injection fires at 4096 bytes of a 40961 byte transfer, the whole file
still arrives on the data channel, and `SIZE` afterwards answers `213 40961`.

## `--pasv-no-accept`: announce a data port and then fail to serve it

Three ways the data connection can fail to happen. They are **not**
interchangeable: the client sees a different failure in each, and the wait sits
at a different depth.

| mode | what the client sees | verified |
|---|---|---|
| `tls-silent` | `connect()` succeeds, then nothing ever comes back | connect in 0.00s, zero data bytes |
| `connect-hang` | `connect()` itself never returns | stalled past 8s |
| `refuse` | `connect()` gets RST at once | refused in 0.00s |

`connect-hang` fills the listening socket's accept queue so the kernel drops the
SYN. Without that the kernel completes the handshake on its own and `connect()`
returns, which is a different case wearing the same name.

`refuse` is not padding: it is the contrast that shows the other two really are
hangs and not just slow.

**`--refuse-before-data`** sends `550 Failed to open file.` on the control
channel *before* the data phase, so the refusal is already queued while the
client waits on a data channel that will never work. That is the shape measured
live: the answer was in our own socket buffer and nobody was looking at it.

### The byte count will not match the live one, and here is why

Live, the control channel held **55** unread bytes. This fixture speaks
plaintext, so the same refusal queues **26**:

```
"550 Failed to open file.\r\n"                     26 bytes
inside a TLS 1.2 AES-GCM record: 5 + 8 + 26 + 16 = 55 bytes
```

The shape is identical and the arithmetic accounts for the difference exactly,
but a test asserting `Recv-Q == 55` against this fixture will fail. Assert the
behaviour, not the byte count, or run the case over a real FTPS server.

### Guards on the fact, not just the effect

`pasv_announced` and `data_accepted` are in the status file. Expect `1` and `0`:
without them the test would also pass against a server that refused the `PASV`
itself, which is a different fault entirely.

## What this fixture CANNOT reproduce: the live hang

Measured, not assumed. Against this fixture in `--refuse-before-data
--pasv-no-accept tls-silent`, our own client fails **correctly in 0.113s**:

```
Error: Download failed: Invalid path: [550] 550 Failed to open file.
```

Against the live FTPS lab, the same binary hangs indefinitely on the same
scenario.

The reason is the transport. This fixture speaks **plaintext**, so there is no
TLS handshake on the data channel: the client goes straight to reading, enters
the loop, `read_watching_control` does its job, sees the `550` and fails. Live it
is FTPS, and the wait lives entirely inside the data channel's TLS handshake,
which happens *before* the loop.

**So this fixture exercises the path that is already fixed, not the broken one.**
A test built here to prove that a deadline on the *opening* surfaces the queued
`550` would be green before that change and green after it: it can never go red,
which is the exact shape this file keeps warning about.

**This is now closed: the fixture speaks FTPS.** Pass `--tls-cert` and
`--tls-key` and it offers `AUTH TLS`, `PBSZ` and `PROT` in `FEAT`, upgrades the
control channel, and under `--pasv-no-accept tls-silent` leaves the data
channel's handshake unanswered, which is the live case.

Measured, same fixture, same options, only the transport differs:

| transport | result |
|---|---|
| plaintext | fails correctly in **0.116s** |
| FTPS | **hangs to the 120s ceiling** |

That contrast is also an independent confirmation of where the wait lives: not
in the read loop, which the plaintext run reaches and survives, but in the data
channel's TLS handshake, which only the FTPS run gets to.

Running it needs a certificate the client will accept, and `--insecure` is
refused by design in unattended use, so trust the certificate rather than
disabling the check. A CA plus a leaf (`CA:FALSE`, `subjectAltName=IP:127.0.0.1`)
works; a bare self-signed `openssl req -x509` does not, rustls rejects it with
`CaUsedAsEndEntity`:

```
AEROFTP_PASSWORD=<pw> SSL_CERT_FILE=<ca.pem> \
  aeroftp-cli --tls explicit get ftp://<user>@127.0.0.1:PORT/...
```

The password goes in the environment (or `--password-stdin`), not in the URL.
The client warns about a URL-embedded password every time it sees one, and a
documented example is the fastest way for that habit to spread out of a fixture
and into a real profile.

Under TLS the `ABOR` case does not work: `MSG_PEEK` cannot see through TLS
framing, so `poll_control` returns early and `abor_read` stays `0`. Declared
rather than silently skipped, so nobody reads that zero as a regression.

**What that limit does NOT mean.** It applies to reading the *content* of a
record, which is what `poll_control` needs: it has to know the command is `ABOR`.
The first byte of a TLS record, the content type, is *not* encrypted: `0x17` is
application data, `0x16` a handshake message, `0x15` an alert. A design that
watches the control channel by looking at the record type rather than the
payload is unaffected by this limit, and the limit is in fact the reason to
prefer it. Spelled out here because two READMEs read in sequence would otherwise
suggest the same wall stops both, and it does not.

This is #655's own warning arriving from the opposite side. There: "on loopback
the same code returns in two seconds, a fixture cannot exercise this". Here: in
plaintext the fix works, so the fixture cannot exercise the gap. Twice the same
limit, and both times the risk was publishing a green obtained on the wrong path.

## Every option has been exercised where its mechanism runs

Not merely switched on. A mode enabled on a path that returns before its
mechanism executes is untested, and looks tested. That is how the `tls-silent`
crash survived review here: it had only been run on the `RETR` path together
with `--refuse-before-data`, which returns before `accept_data()` is ever
called, so the mode was on and its code never ran.

| option | exercised by | evidence |
|---|---|---|
| `--feat` (3 positions) | `ls` on each | correct entries, and the reconnection path taken |
| include_hidden | `ls` and `rm -r` on one server | `LIST` vs `CWD` + `LIST -a` |
| `--list-delay` | slow `ls` | 5 rows at 1s took 5.1s, all 5 arrived |
| `--list-total` | slow `ls` | 10 rows over a 6s budget took 6.1s |
| `--late-final` | raw probe on `LIST` | `226` arrived at 8.0s, server held 8.0s |
| `--retr-stall` | `get -r` | the stale-reply desync |
| `--abor-silent` | raw probe mid-transfer | `abor_read` 1, no reply for 8s |
| `--stor-refuse-after` | `put`, both halves | 300000 bytes drained / 65536 then stopped |
| `--stor-read-rate` | `put` throttled | made the drain case measurable at all |
| `--unsolicited-refusal-after` | raw probe mid-`RETR` | injected at 4096 bytes, `unsolicited_sent` 1 |
| `--pasv-no-accept` (3 modes) | raw probe | silent, hung, refused: three different failures |
| `--tls-cert` | our own client | plaintext 0.116s vs FTPS to the ceiling |

### One of those checks was wrong, and the fixture was right

Measuring `--late-final` first suggested the `226` arrived immediately, which
would have made this file's central claim false. The fault was in the probe:

```python
print("226 at %.1fs" % (time.time() - t0), rd())   # wrong
```

Python evaluates the `%` expression before calling `rd()`, so the timestamp was
taken before the blocking read rather than after it. The reading was resolved by
timestamping the **server** log, which showed the hold running its full 8.0s.
When two ends disagree, the end you are not measuring is the one to believe.

## The plumbing options, and one of them is load-bearing

```
--port N        where to listen (default 2130)
--lines N       how many files the listing contains
--hang N        seconds MLSD stays silent in the mlsd-hang position
--stor-stop-hold N   see below
```

`--stor-stop-hold` does more than its name says, and that is worth knowing
before setting it. It was added for the `stop` half of a refused `STOR`, where
it is how long the server keeps a socket open after it stops reading. It is now
**also** how long the server holds a `LIST`, `RETR` or `STOR` pending when
`--pasv-no-accept tls-silent` leaves no usable data channel, which is the hang
this fixture exists to produce.

So lowering it to shorten a `STOR` test also shortens every deliberate hang, and
a client that then returns looks like a client that recovered. The name is
narrower than the behaviour: it is documented rather than renamed because tests
are already being written against these flags, and a rename would break them for
a cosmetic gain.

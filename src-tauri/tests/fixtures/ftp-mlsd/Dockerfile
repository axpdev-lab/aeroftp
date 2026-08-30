# An FTP server that speaks MLSD and MLST, which the vsftpd fixture next door
# does not.
#
# That gap is not a detail of the lab: `list_inner_opts` tries MLSD first and
# falls back to LIST, and the anti-hang MLST probe only runs when the server
# advertises MLST. With vsftpd as the only fixture, the branch most servers
# take was never executed by any test, and a defect that lived there survived
# two rounds of review for that reason alone. A fixture that cannot reach a
# code path decides what can be found in it.
#
# The Python version is pinned for reproducibility, the same posture as the
# vsftpd fixture next door, which pins `alpine:3.19`. It is NOT pinned for a
# compatibility limit: an earlier version of this comment claimed pyftpdlib
# needed 3.11 because `asynchat` was removed in 3.12, and that was checked and
# is false. pyftpdlib 1.5.10 imports cleanly on 3.12 with `asynchat` absent
# from the standard library, because on any Python 3 it uses its own bundled
# `_asynchat` and never reaches for the removed module.
#
# The claim is recorded here because it was written to stop somebody raising
# the pin without knowing why, and a defensive note resting on an unchecked
# fact is worse than no note: it makes a false thing hard to question.
FROM python:3.12-alpine

RUN pip install --no-cache-dir "pyftpdlib==1.5.10" "pyOpenSSL==24.2.1" \
    && adduser -D -h /workdir testuser \
    && mkdir -p /workdir && chown testuser:testuser /workdir

# A throwaway certificate, generated at build time: the fixture is loopback
# only and the tests connect with verification disabled.
RUN apk add --no-cache openssl \
    && openssl req -x509 -newkey rsa:2048 -keyout /k.pem -out /c.pem -days 3650 \
       -nodes -subj "/CN=localhost" 2>/dev/null \
    && cat /k.pem /c.pem > /cert.pem && rm /k.pem /c.pem

COPY server.py /server.py
EXPOSE 21 30100-30109
ENTRYPOINT ["python", "/server.py"]

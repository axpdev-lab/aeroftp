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
# Python 3.11 on purpose: pyftpdlib still uses `asynchat`, removed in 3.12.
FROM python:3.11-alpine

RUN pip install --no-cache-dir "pyftpdlib==1.5.10" \
    && adduser -D -h /workdir testuser \
    && mkdir -p /workdir && chown testuser:testuser /workdir

COPY server.py /server.py
EXPOSE 21 30100-30109
ENTRYPOINT ["python", "/server.py"]

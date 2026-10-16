# ProFTPD mod_sftp (an SFTP server of its own, not OpenSSH's sftp-server).
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends proftpd-core proftpd-mod-crypto openssh-client \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/sh testuser \
    && usermod -p '*' testuser \
    && mkdir -p /etc/proftpd/authorized_keys /home/testuser/data
COPY sftp.conf /etc/proftpd/conf.d/sftp.conf
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 755 /usr/local/bin/entrypoint.sh
EXPOSE 2222
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]

# OpenSSH sftp-server / internal-sftp on a base chosen at build time: Debian
# trixie (OpenSSH 10, limits@openssh.com) or Ubuntu 20.04 (OpenSSH 8.2, no
# limits extension). The entrypoint applies the variant (MaxSessions, chroot).
ARG BASE=debian:trixie-slim
FROM ${BASE}
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
    && apt-get install -y --no-install-recommends openssh-server procps iproute2 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -s /bin/sh testuser \
    && usermod -p '*' testuser \
    && mkdir -p /run/sshd /srv/chroot
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 755 /usr/local/bin/entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]

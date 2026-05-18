# Minimal vsftpd fixture for the PD-FTP-1 intra-file parallelism test.
# Alpine-based to keep the image small, pinned for reproducibility, mirroring
# the sftp-rsync fixture posture. Password auth (testuser:testpass), a single
# chrooted writable home, an explicit passive port range so the data
# connections survive Docker's NAT.

FROM alpine:3.19

RUN apk add --no-cache vsftpd \
    && adduser -D -h /workdir -s /sbin/nologin testuser \
    && echo "testuser:testpass" | chpasswd \
    && mkdir -p /workdir \
    && chown testuser:testuser /workdir

# Standalone vsftpd, local user only, chrooted to its home (= /workdir).
# `allow_writeable_chroot` is required because the chroot root is writable.
# `seccomp_sandbox=NO` and an explicit `pasv_address`/port range are the
# standard container-friendly settings; `pasv_addr_resolve=NO` keeps the
# advertised PASV address literal so the loopback test reaches the mapped
# ports.
RUN printf '%s\n' \
    'listen=YES' \
    'listen_ipv6=NO' \
    'anonymous_enable=NO' \
    'local_enable=YES' \
    'write_enable=YES' \
    'local_umask=022' \
    'chroot_local_user=YES' \
    'allow_writeable_chroot=YES' \
    'secure_chroot_dir=/var/run/vsftpd/empty' \
    'pam_service_name=vsftpd' \
    'seccomp_sandbox=NO' \
    'pasv_enable=YES' \
    'pasv_address=127.0.0.1' \
    'pasv_addr_resolve=NO' \
    'pasv_min_port=30000' \
    'pasv_max_port=30009' \
    'xferlog_enable=NO' \
    'background=NO' \
    > /etc/vsftpd/vsftpd.conf \
    && mkdir -p /var/run/vsftpd/empty

EXPOSE 21 30000-30009

ENTRYPOINT ["/usr/sbin/vsftpd", "/etc/vsftpd/vsftpd.conf"]

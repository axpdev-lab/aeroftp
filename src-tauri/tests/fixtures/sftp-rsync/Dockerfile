# Minimal SSH + rsync fixture for delta-sync integration tests.
# Alpine-based to keep image small (~25 MB).

FROM alpine:3.19

RUN apk add --no-cache openssh rsync bash \
    && ssh-keygen -A \
    && adduser -D -s /bin/sh testuser \
    && passwd -u testuser \
    && mkdir -p /home/testuser/.ssh /workdir \
    && chown -R testuser:testuser /home/testuser /workdir \
    && chmod 700 /home/testuser/.ssh

# Keep only key-based auth: Fase 1 scope.
# StrictModes no because authorized_keys is bind-mounted from the host
# (ownership and perms don't map cleanly). Safe for an isolated test fixture.
RUN sed -i 's/^#\?PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config \
    && sed -i 's/^#\?PubkeyAuthentication.*/PubkeyAuthentication yes/' /etc/ssh/sshd_config \
    && sed -i 's/^#\?StrictModes.*/StrictModes no/' /etc/ssh/sshd_config \
    && echo "AuthorizedKeysFile .ssh/authorized_keys" >> /etc/ssh/sshd_config

# Authorized key is mounted at runtime via the compose file (read-only bind).
EXPOSE 22

# -D foreground, -e logs to stderr for docker logs visibility.
CMD ["/usr/sbin/sshd", "-D", "-e"]

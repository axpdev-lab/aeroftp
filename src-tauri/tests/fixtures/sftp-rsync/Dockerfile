# Minimal SSH + rsync fixture for delta-sync integration tests.
# Alpine-based to keep image small (~25 MB).

FROM alpine:3.19

RUN apk add --no-cache openssh rsync bash \
    && ssh-keygen -A \
    && adduser -D -s /bin/sh testuser \
    && passwd -u testuser \
    && mkdir -p /home/testuser/.ssh /workdir /mnt \
    && chown -R testuser:testuser /home/testuser /workdir \
    && chmod 700 /home/testuser/.ssh

# Key-based auth only (Fase 1 scope).
# StrictModes stays at its secure default (yes); the entrypoint script
# normalizes ownership + mode on the bind-mounted authorized_keys so the
# default check passes. This keeps the fixture posture honest while
# avoiding a "StrictModes no" hack that could be copied elsewhere.
RUN sed -i 's/^#\?PasswordAuthentication.*/PasswordAuthentication no/' /etc/ssh/sshd_config \
    && sed -i 's/^#\?PubkeyAuthentication.*/PubkeyAuthentication yes/' /etc/ssh/sshd_config \
    && echo "AuthorizedKeysFile .ssh/authorized_keys" >> /etc/ssh/sshd_config

COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 755 /usr/local/bin/entrypoint.sh

EXPOSE 22

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]

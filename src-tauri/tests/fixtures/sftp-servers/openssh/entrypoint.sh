#!/bin/sh
# Key-only sshd. SFTP_SUBSYSTEM replaces the Subsystem line (internal-sftp for
# the chroot variant); SSHD_EXTRA is appended last, so a Match block fits.
set -e
install -d -m 700 -o testuser -g testuser /home/testuser/.ssh
install -m 600 -o testuser -g testuser /mnt/authorized_keys /home/testuser/.ssh/authorized_keys
# Fixed host keys from setup.sh, so the key a client trusted survives a rebuild.
for k in /mnt/hostkeys/ssh_host_*_key; do
    [ -f "$k" ] && install -m 600 "$k" /etc/ssh/ && install -m 644 "$k.pub" /etc/ssh/
done
ssh-keygen -A >/dev/null
if [ -n "${SFTP_SUBSYSTEM:-}" ]; then
    sed -i "s|^Subsystem.*|Subsystem sftp ${SFTP_SUBSYSTEM}|" /etc/ssh/sshd_config
fi
{
    echo "PasswordAuthentication no"
    echo "PubkeyAuthentication yes"
    # VERBOSE logs one "Starting session: subsystem 'sftp'" per SFTP channel,
    # which is how run-matrix.sh counts the channels a client opens.
    echo "LogLevel VERBOSE"
    printf '%b\n' "${SSHD_EXTRA:-}"
} >> /etc/ssh/sshd_config
/usr/sbin/sshd -t
exec /usr/sbin/sshd -D -e

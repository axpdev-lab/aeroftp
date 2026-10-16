#!/bin/sh
set -e
install -d -m 700 -o testuser -g testuser /home/testuser/.ssh
install -m 600 -o testuser -g testuser /mnt/authorized_keys /home/testuser/.ssh/authorized_keys
# The fixed host key from setup.sh, converted to Dropbear's format.
dropbearconvert openssh dropbear /mnt/hostkeys/ed25519 /etc/dropbear/dropbear_ed25519_host_key
# -s refuses passwords, -E logs to stderr.
exec dropbear -F -E -s -r /etc/dropbear/dropbear_ed25519_host_key -p 22

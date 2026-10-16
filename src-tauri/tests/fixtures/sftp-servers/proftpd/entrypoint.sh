#!/bin/sh
# mod_sftp reads user keys in RFC 4716 form (setup.sh converts the test key)
# and PEM host keys (fixed ones from setup.sh, mounted read-only).
set -e
install -m 644 /mnt/authorized_keys.rfc4716 /etc/proftpd/authorized_keys/testuser
proftpd --configtest
exec proftpd --nodaemon

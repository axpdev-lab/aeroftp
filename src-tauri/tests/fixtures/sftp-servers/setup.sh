#!/bin/sh
# One-time setup of the SFTP server matrix (see docker-compose.yml): the test
# client key, fixed host keys (so a rebuilt container keeps the key the client
# trusted on first use), SFTPGo's initial user, and the payload every server
# serves read-only at /data. Re-running keeps what exists.
set -e
cd "$(dirname "$0")"
umask 077
[ -f ssh_key ] || ssh-keygen -q -t ed25519 -N '' -C aeroftp-sftp-servers -f ssh_key
ssh-keygen -e -f ssh_key.pub > ssh_key.rfc4716
mkdir -p generated/hostkeys/openssh generated/hostkeys/proftpd generated/hostkeys/dropbear
for t in ed25519 ecdsa rsa; do
    k=generated/hostkeys/openssh/ssh_host_${t}_key
    [ -f "$k" ] || ssh-keygen -q -t "$t" -N '' -f "$k"
done
[ -f generated/hostkeys/proftpd/rsa ] || ssh-keygen -q -t rsa -b 3072 -m PEM -N '' -f generated/hostkeys/proftpd/rsa
[ -f generated/hostkeys/proftpd/ecdsa ] || ssh-keygen -q -t ecdsa -b 256 -m PEM -N '' -f generated/hostkeys/proftpd/ecdsa
[ -f generated/hostkeys/dropbear/ed25519 ] || ssh-keygen -q -t ed25519 -N '' -f generated/hostkeys/dropbear/ed25519
cat > generated/sftpgo-initial.json <<JSON
{
  "users": [
    {
      "id": 1,
      "status": 1,
      "username": "testuser",
      "home_dir": "/srv/sftpgo/data/testuser",
      "uid": 0,
      "gid": 0,
      "permissions": { "/": ["*"] },
      "public_keys": ["$(cat ssh_key.pub)"],
      "filesystem": { "provider": 0 }
    }
  ],
  "folders": [],
  "admins": []
}
JSON
# SFTPGo runs as its own user: it holds only the public key.
chmod 644 generated/sftpgo-initial.json
umask 022
mkdir -p payload/many payload/small payload/edges
[ -f payload/big.bin ] || head -c $((128 * 1024 * 1024)) /dev/urandom > payload/big.bin
[ -f payload/mid.bin ] || head -c $((8 * 1024 * 1024)) /dev/urandom > payload/mid.bin
# 600 files of 320 KiB: two chunks each, so every one goes through the
# read-ahead, one after another on the connections of one job.
if [ ! -f payload/many/f599.bin ]; then
    i=0; while [ $i -lt 600 ]; do head -c 327680 /dev/urandom > payload/many/f$i.bin; i=$((i + 1)); done
fi
if [ ! -f payload/small/s99.bin ]; then
    i=0; while [ $i -lt 100 ]; do head -c 1024 /dev/urandom > payload/small/s$i.bin; i=$((i + 1)); done
fi
# Sizes on the edges of a chunk (256 KiB) and of OpenSSH's read length
# (261120 bytes), plus an empty file.
for n in 0 1 261119 261120 261121 262143 262144 262145 522240 522241 524288 524289; do
    [ -f "payload/edges/e$n.bin" ] || head -c "$n" /dev/urandom > "payload/edges/e$n.bin"
done
# Names a path layer can get wrong, and a symlink to a file.
mkdir -p payload/special
[ -f "payload/special/naïve café 文件 #1 %20.bin" ] || head -c 300000 /dev/urandom > "payload/special/naïve café 文件 #1 %20.bin"
[ -f "payload/special/ leading and trailing .bin " ] || head -c 300000 /dev/urandom > "payload/special/ leading and trailing .bin "
[ -L payload/special/link-to-mid.bin ] || ln -s ../mid.bin payload/special/link-to-mid.bin
chmod -R a+rX payload
(cd payload && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum) > generated/payload.sha256
# A file the server cannot read: a download must fail cleanly. Made after the
# checksums, which the host could not take of it either.
mkdir -p payload/denied
[ -f payload/denied/unreadable.bin ] || head -c 300000 /dev/urandom > payload/denied/unreadable.bin
chmod 000 payload/denied/unreadable.bin
# Over 4 GiB, sparse here: offsets past 2^32 on the wire.
[ -f payload/huge-sparse.bin ] || truncate -s $((4 * 1024 * 1024 * 1024 + 300 * 1024 * 1024)) payload/huge-sparse.bin
echo "ready: $(wc -l < generated/payload.sha256) payload files"

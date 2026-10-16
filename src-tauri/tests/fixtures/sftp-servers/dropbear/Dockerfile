# Dropbear SSH with OpenSSH's sftp-server as its subsystem (the usual pairing
# on embedded systems and small appliances).
FROM alpine:3.19
RUN apk add --no-cache dropbear dropbear-convert openssh-sftp-server \
    && adduser -D -s /bin/sh testuser \
    && sed -i 's/^testuser:!/testuser:*/' /etc/shadow \
    && mkdir -p /etc/dropbear
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 755 /usr/local/bin/entrypoint.sh
EXPOSE 22
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]

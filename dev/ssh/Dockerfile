# SSH bastion for Zephix integration tests ONLY (password + locally generated test key, forwarding on).
# Settings live in a drop-in: it is included before the stock settings, and sshd uses the
# first value it sees for each keyword (the stock file says "AllowTcpForwarding no").
FROM alpine:3.20
RUN apk add --no-cache openssh \
 && ssh-keygen -A \
 && adduser -D -s /bin/sh tunnel \
 && echo 'tunnel:rdbs_tunnel_pw' | chpasswd \
 && mkdir -p /home/tunnel/.ssh /etc/ssh/sshd_config.d \
 && printf '%s\n' 'AllowTcpForwarding yes' 'PasswordAuthentication yes' 'PermitRootLogin no' \
      > /etc/ssh/sshd_config.d/10-rdbs-tests.conf
COPY test_key.pub /home/tunnel/.ssh/authorized_keys
RUN chown -R tunnel:tunnel /home/tunnel/.ssh \
 && chmod 700 /home/tunnel/.ssh \
 && chmod 600 /home/tunnel/.ssh/authorized_keys
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]

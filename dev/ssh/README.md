# Test SSH bastion

Test-only SSH bastion for the `RDBS_IT=1` tunnel tests. It accepts the password `rdbs_tunnel_pw`
and a client key that every developer generates locally; the key is not in the repository.

Create the key once, before building the container (OpenSSH ships with Windows 10/11):

```bash
ssh-keygen -t ed25519 -N "" -C rdbs-it -f dev/ssh/test_key
```

In Windows PowerShell 5.1 write the empty passphrase as `-N '""'`.

Then build and start the servers:

```bash
docker compose -f dev/docker-compose.yml up -d --build
```

After creating a new key, rebuild the bastion (`up -d --build ssh`). The key grants access to this
local container only; never use it anywhere else.

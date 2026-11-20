# Rookery

Rookery runs a team's machines, their desktops, and the tools built on them. See [PLAN.md](PLAN.md).


## Run a local server

Set `ROOKERY_DATABASE_URL` to a dedicated PostgreSQL 17 database with pgvector.
Set `ROOKERY_PUBLIC_URL` to the address users will open. For an HTTP-only local
deployment, set `ROOKERY_COOKIE_SECURE=false`. Use HTTPS and the default secure
cookies for a deployment that serves other machines.

Run `rookery serve`, then run `rookery server setup-key` on the server to create
the first admin's sign-in link. The CLI uses the Unix socket on the server and
can sign in remotely with `rookery login URL`.

## Configure a tailnet

Feed the control server API key from a file readable only by your user.

```sh
rookery tailnet configure --control https://controlplane.tailscale.com \
  --tag tag:rookery --api-key-stdin < "$ROOKERY_TAILNET_API_KEY_FILE"
rookery tailnet status
rookery tailnet devices
```

Use the Headscale HTTPS origin for a self-hosted control server. Set
`ROOKERY_TAILNET_HEADSCALE_USER` when the control server has several users, and
`ROOKERY_TAILNET_LISTEN` to expose the edge on the leader's tailnet node.

## Rotate the deployment sealing key

Keep the previous key file and provide its path with
`ROOKERY_SECRET_PREVIOUS_KEY_FILE`. Start the leader with the new active
`ROOKERY_SECRET_KEY_FILE` and run `rookery server secrets rekey` on its socket.
After the command succeeds, remove the previous-key setting and restart.

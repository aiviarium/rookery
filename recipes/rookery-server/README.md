# A Rookery server

This recipe turns a small Ubuntu machine into a Rookery server. The server
runs in a Docker container, and your tailnet reaches it over HTTPS. A rented
virtual machine with 2 GB of memory is enough, as the server only looks after
hosts that run elsewhere.

It was run on Ubuntu 26.04 and needs 24.04 or later.

## What you get

| Part | Set-up |
| --- | --- |
| Docker | `docker.io` from Ubuntu's packages |
| Container | `rookery`, from the image `ghcr.io/aiviarium/rookery`, restarted with the machine |
| Data | `/srv/rookery` with the folders `config`, `data` and `ssh` |
| Port | 7777 on this machine only |
| Tailscale | Installed and joined when you pass an enrollment key |
| Tailnet | `tailscale serve` puts the server at `https://THIS-MACHINE.YOUR-TAILNET.ts.net` with TLS |
| Firewall | `ufw`: incoming traffic is refused except SSH and the tailnet |

## Run it

On the machine itself:

```bash
curl -fsSL https://github.com/aiviarium/rookery/archive/refs/heads/main.tar.gz \
  | tar xz --strip-components=1 --wildcards '*/recipes' \
    '*/provision/ansible/roles/rookery/files/rookery-server-upgrade.py'
cd recipes/rookery-server
sudo ./install.sh -e tailscale_auth_key=tskey-auth-EXAMPLE
```

`install.sh` installs `ansible-core` from Ubuntu's packages and applies
`site.yml` to the machine it runs on. Its arguments go to `ansible-playbook`.

From another machine that has Ansible:

```bash
ansible-playbook -i 192.0.2.10, -u root site.yml -e tailscale_auth_key=tskey-auth-EXAMPLE
```

A second run changes nothing that is already in place. A container named
`rookery` keeps its image, data and options. A stopped one is started. It is made again only when it lacks one of the recipe's
options, such as the port or a folder in `/srv/rookery`.

## Use it

The first run makes a setup key and prints it at the end, with the server's
address. The key lets the first user collect the server's token. It lasts 24
hours and ends once `rookery login` has collected the token. Whoever runs the recipe can hand on
the key and never sees the token.

On your own machine, give Rookery the key when it asks:

```bash
rookery login https://THIS-MACHINE.YOUR-TAILNET.ts.net
```

Or open Rookery's screens with the key in `ROOKERY_SETUP_KEY`, on Linux, macOS
or Windows:

```bash
read -rs ROOKERY_SETUP_KEY && export ROOKERY_SETUP_KEY
rookery --server https://THIS-MACHINE.YOUR-TAILNET.ts.net
```

The screens sign in and show the token. `tailscale serve status` on
the machine says the address. Make a new key on the machine at any time:

```bash
sudo docker exec rookery rookery server setup-key
```

The token itself gives full control of every host. Without a key, pass it to
`rookery login` on standard input, on the machine itself:

```bash
sudo cat /srv/rookery/data/rookery/serve.token | rookery login https://THIS-MACHINE.YOUR-TAILNET.ts.net --token-stdin
```

## The server on a node

A large server can run the Rookery server and host the machines too. Install
Ubuntu, run the recipe with `rookery_server_node`, and the server can apply its
own machine as a [node](../../docs/content/docs/%28guide%29/use/nodes.mdx):

```bash
sudo ./install.sh -e rookery_server_node=true -e tailscale_auth_key=tskey-auth-EXAMPLE
```

| Part | Set-up |
| --- | --- |
| Login | The server's own key logs in as `root` here |
| Name | The server finds this machine at the name `node` |
| Docker | The container gets the socket of this machine's engine, and joins its group |

Then, on a Rookery connected to the server, add the machine as a node, list
its features and apply it. This example calls it `big`:

```bash
rookery host add big node --user root --type node \
  --feature docker/engine --feature incus/server
rookery apply big
```

The apply installs Incus and Docker, and the server registers their
provisioners, which reach them on this machine. Docker's socket and the root
login give root on this machine, so whoever has the server's token controls
the whole machine. A container from an earlier run keeps its data and its
own options and gets these: the run makes it again with both.

Once the machine is a node, its action `upgrade-server` updates the server
in place. See [Update it](#update-it).

## Settings

The settings are in [`vars.yml`](vars.yml), each with a comment. Change them
there, or keep your own file and pass it with `-e @my-vars.yml`.

| Setting | Default | Meaning |
| --- | --- | --- |
| `rookery_server_image` | `ghcr.io/aiviarium/rookery` | The image, with a tag such as `:edge` if you want one |
| `rookery_server_dir` | `/srv/rookery` | Where the server keeps its settings, data and SSH keys |
| `rookery_server_port` | `7777` | The port on this machine only |
| `rookery_server_tailscale_serve` | `true` | Put the server on the tailnet with TLS |
| `rookery_server_update` | `false` | Pull the image and run the server from it when it is newer. See [Update it](#update-it) |
| `rookery_server_node` | `false` | Let the server apply this machine as a node. See [The server on a node](#the-server-on-a-node) |
| `tailscale_auth_key` | empty | An enrollment key. Without one Tailscale is not installed |
| `tailscale_login_server` | empty | The address of a Headscale server |
| `ssh_from` | `any` | Who reaches SSH from outside the tailnet: `any`, or an address or network |

Pass the enrollment key on the command line. Do not save it in a file you
commit.

This machine takes a key you make, in Tailscale's admin console or with
`headscale preauthkeys create`, because the recipe runs before Rookery does.
Make it one-use and short-lived. The hosts Rookery sets up later get their keys
from Rookery, which makes them through the tailnet's API with the secret
`tailnet_api_key`.

## Update it

Run the recipe again with `rookery_server_update`. It pulls the image and,
when it is newer, makes the container again from it:

```bash
sudo ./install.sh -e rookery_server_update=true
```

The new container keeps the data in `/srv/rookery` and every option of the old
one: its mounts, ports, extra host names, groups, environment, logging and
restart policy. An option the recipe sets, such as the port, takes the value
of this run. The old container is stopped and kept until the new one runs,
and starts again if the new one does not come up. A container with an option
the helper cannot carry over, such as `--privileged`, is left as it is, and
the run says which option it was.

On a machine that runs Incus, the container keeps the name `incus-host`, at
which the provisioners of Rookery 0.5 reach it. A run without
`rookery_server_update` leaves the image as it is.

Pin a version with its tag:

```bash
sudo ./install.sh -e rookery_server_update=true -e rookery_server_image=ghcr.io/aiviarium/rookery:0.6.0
```

A server never goes back to an older version this way. Download the recipe
of the new version first, with the command in [Run it](#run-it), as each
version's recipe knows its own server.

Once the machine is a node, its action `upgrade-server` does the same from a
Rookery of the new version:

```bash
rookery run big upgrade-server version=0.6.0
```

### From your own Rookery

A Rookery for Linux can run the server from itself, without a release. The
machine builds the image `rookery:VERSION` from the Rookery that sends it, and
the container runs from it with its data and options. On a node:

```bash
rookery run big upgrade-server source=here
```

On any machine with Docker that your own `ssh` reaches:

```bash
rookery server deploy root@192.0.2.10
```

| Command | Where it builds |
| --- | --- |
| `rookery run NODE upgrade-server source=here` | On the node, by its controller |
| `rookery server deploy [USER@]HOST` | On `HOST`, over `ssh`. It needs `python3` there for the helper |
| `rookery server image` | On this machine, with no container changed |

`deploy` takes `--container NAME` when the container is not `rookery`. With
no container of that name it makes one, at `127.0.0.1:7777` or the
`--port` given, with its data in two volumes. It then says how to get a
setup key.

Only a static Linux program for the machine's CPU goes into the image, as
a release is. Rookery says which release to use when it is not one.

Back to the published image:

```bash
rookery run big upgrade-server version=0.7.0
rookery server deploy root@192.0.2.10 --release 0.7.0
```

A server that runs a self-built image goes back to
`ghcr.io/aiviarium/rookery`. A version older than the running one is still
refused.

Do not remove the container by hand to update it. A new one from the recipe
has only the recipe's options, and loses those passed by hand.

## Good to know

- The tailnet address needs HTTPS certificates switched on for your tailnet.
  Set `rookery_server_tailscale_serve` to `false` to leave the server on this
  machine only.
- A private image needs `sudo docker login ghcr.io` before the run.
- The server connects to its hosts over SSH with the keys in
  `/srv/rookery/ssh`. That folder belongs to the user 1000, as the server in
  the container is that user.
- Back up `/srv/rookery`. It holds the profiles, the secrets and the token.
- With `ssh_from` set to one address, SSH from anywhere else is refused.
  Your provider's console is the way back in if the address was wrong.

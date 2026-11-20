# The desktop role

One shared virtual desktop for a workstation's account: XFCE on TigerVNC's
`Xvnc`, run as the account's systemd user units with lingering on, so that it is
there whether or not anyone is logged in.

| What | Where |
| --- | --- |
| The session | `rookery-desktop.service`, which runs `Xvnc :1` and then `xfce4-session` |
| The provider | `rookery-box-desktop.service`, which runs `box desktop serve` |
| The VNC socket | `/run/user/<uid>/rookery/vnc.sock`, mode 0600, and no TCP port (`-rfbport -1`) |
| The environment | `/etc/rookery/desktop.env`, which `box exec` and an exec with the desktop's environment read |
| The screen | `desktop_geometry`, and the GPU's render node when the account has it |

Nothing listens on a TCP port. Rookery reaches the desktop through rookeryd: the
socket through `PortService`, and screenshots, input and the clipboard through
`DesktopService`, which relays to `box desktop serve` as a host service.

The screen never locks and never blanks, the account's default keyring is
unlocked (it has no password), and Chromium is installed.

## Variables

| Variable | Default | Meaning |
| --- | --- | --- |
| `desktop_user` | `dev` | The account whose desktop this is; the base role makes it |
| `desktop_geometry` | `1920x1080` | The screen's size |
| `desktop_depth` | `24` | Its pixel depth |
| `desktop_gpu` | `false` | Whether the account gets the `render` and `video` groups |
| `desktop_chromium_package` | `chromium` | Which package to install for Chromium |
| `desktop_box` | `/usr/local/bin/box` | Where the base role put box |

## Checking it

```bash
ansible-playbook --syntax-check -i localhost, ansible/roles-check.yml
```

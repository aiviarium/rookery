# Backups

This machine takes a snapshot every night with restic. The owner runs and
restores them from Rookery. They happen without anything from you.

## What is in a snapshot

- Your home, `{{ rookery_home }}`, and everything under `/srv/agent`.
- The machine's own description: `/etc/rookery` and `/var/lib/rookery`.

Left out, because it can be made again: caches, downloaded dependencies such as
`node_modules`, build output such as `target/debug` and `target/release`, and
installed runtimes. The full list is `/etc/rookery-backup.exclude`, plus what the
files in `/etc/rookery-backup.exclude.d` add for the tools on this machine.

So keep work you want to survive under your home or `/srv/agent`, and do not
keep the only copy of anything in a folder with one of those names.

## Looking at it

    cat /var/lib/rookery-backup/last        # when the last snapshot was taken

Taking, listing and restoring snapshots needs root: `/usr/local/sbin/rookery-backup`.
A restore writes over files in place. Leave it to the owner unless you were asked.

The repository password is `~/.config/rookery-backup/password` and the rclone
configuration `~/.config/rclone/rclone.conf`. Both are secrets. Never print them, copy them or commit them. Do not change them either, or the snapshots become unreadable.

#!/usr/bin/env bash
# Snapshots of the agent's data with restic, over rclone. Installed as
# /usr/local/sbin/rookery-backup; runs as root. Settings: /etc/rookery-backup.conf.
#   rookery-backup snapshot | list | ids | restore [ID] | last
set -euo pipefail
. /etc/rookery-backup.conf
state=/var/lib/rookery-backup
password=$AGENT_HOME/.config/rookery-backup/password
export RESTIC_REPOSITORY=$REPOSITORY RESTIC_PASSWORD_FILE=$password
export RCLONE_CONFIG=$AGENT_HOME/.config/rclone/rclone.conf RESTIC_CACHE_DIR=/var/cache/rookery-backup
host=$(hostname)

ready() {
  [[ -n $REPOSITORY ]] || { echo 'No repository: set rookery_backup_repository in the profile and apply it.' >&2; exit 1; }
  [[ -s $password ]] || { echo "No repository password: send the secret that installs ${password/#$AGENT_HOME/\~} (Secrets and registry logins)." >&2; exit 1; }
}

case ${1:-} in
  snapshot)
    ready
    restic cat config >/dev/null 2>&1 || restic init
    excludes=(--exclude-file /etc/rookery-backup.exclude)
    for file in /etc/rookery-backup.exclude.d/*; do
      [[ -f $file ]] && excludes+=(--exclude-file "$file")
    done
    restic backup --tag rookery --host "$host" --exclude-caches "${excludes[@]}" "${PATHS[@]}"
    restic forget --tag rookery --host "$host" --prune \
      --keep-daily "$KEEP_DAILY" --keep-weekly "$KEEP_WEEKLY" --keep-monthly "$KEEP_MONTHLY"
    mkdir -p "$state"
    restic snapshots --tag rookery --host "$host" --latest 1 --json \
      | jq -r '.[-1] | "\(.time[0:19] | sub("T"; " ")) (\(.short_id))"' >"$state/last" ;;
  list) ready; restic snapshots --tag rookery ;;
  ids) ready; restic snapshots --tag rookery --json | jq -r 'reverse | .[].short_id' ;;
  restore)
    ready
    id=${2:-${ROOKERY_ARG_SNAPSHOT:-}}
    [[ $id =~ ^([0-9a-f]{8,64}|latest)$ ]] || { echo 'Name a snapshot: an id from the list, or latest.' >&2; exit 2; }
    # Files come back to the paths they were taken from; what is not in the
    # snapshot is left alone.
    restic restore "$id" --target /
    echo "Restored $id. Programs that were running may need a restart." ;;
  last)
    echo
    echo '── Backup'
    if [[ -z $REPOSITORY ]]; then echo 'No repository set.'; exit 0; fi
    echo "Last snapshot  $(cat "$state/last" 2>/dev/null || echo none yet)"
    echo "Schedule       $(systemctl is-active rookery-backup.timer 2>/dev/null || true), next $(systemctl show -p NextElapseUSecRealtime --value rookery-backup.timer 2>/dev/null || true)"
    systemctl is-failed -q rookery-backup.service 2>/dev/null && echo 'The last scheduled run failed: journalctl -u rookery-backup' || true ;;
  *) echo 'Usage: rookery-backup snapshot | list | ids | restore [ID] | last' >&2; exit 2 ;;
esac

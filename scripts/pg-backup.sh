#!/usr/bin/env bash
# Weekly backup of openclaw Postgres to g731 via SSH
# Overwrites a single file each run — systemd timer runs weekly

set -euo pipefail

BACKUP_DIR="/home/shawaz/.openclaw/backups"
REMOTE_HOST="shawaz@192.168.50.120"
REMOTE_DIR="/home/shawaz/.openclaw/backups"
SSH_KEY="/home/shawaz/.ssh/id_ed25519_aitan"
DUMP_FILE="openclaw-latest.sql.gz"

mkdir -p "$BACKUP_DIR"

# Dump and compress (overwrites previous)
docker exec openclaw-postgres pg_dump -U openclaw -d openclaw | gzip > "${BACKUP_DIR}/${DUMP_FILE}"

# Ship to g731 (overwrites previous)
scp -i "$SSH_KEY" -q "${BACKUP_DIR}/${DUMP_FILE}" "${REMOTE_HOST}:${REMOTE_DIR}/${DUMP_FILE}"

echo "[$(date)] Backup shipped to g731 ($(du -h "${BACKUP_DIR}/${DUMP_FILE}" | cut -f1))"

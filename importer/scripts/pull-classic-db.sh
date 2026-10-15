#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "$0")/../.." || exit 1

LOCKFILE="importer/scripts/classic-db.lock"
# shellcheck source=importer/scripts/classic-db.lock
source "$LOCKFILE"

CLASSIC_DB_REPO="${CLASSIC_DB_REPO:-https://github.com/cmangos/classic-db.git}"
CLASSIC_DB_REF="${CLASSIC_DB_REF:-$CLASSIC_DB_SHA}"
CLONE_DIR=".import/classic-db"
OUT_FILE=".import/classic-db-full.sql"
# The pinned classic-db commit stores its compressed dump under Full_DB/.
ASSEMBLE_GLOB="Full_DB/*.sql*"

verify_sha256() {
  local file="$1" expected="$2" actual
  if [ ! -f "$file" ]; then
    echo "[pull-classic-db] verify_sha256: no such file: $file" >&2
    return 1
  fi
  actual="$(sha256sum "$file" | awk '{print $1}')"
  if [ "$actual" != "$expected" ]; then
    echo "[pull-classic-db] sha256 MISMATCH for $file" >&2
    echo "  expected: $expected" >&2
    echo "  actual:   $actual" >&2
    return 1
  fi
  echo "[pull-classic-db] sha256 OK: $actual"
  return 0
}

clone_or_fetch() {
  if [ -d "$CLONE_DIR/.git" ]; then
    git -C "$CLONE_DIR" fetch --all --tags
  else
    mkdir -p .import
    git clone "$CLASSIC_DB_REPO" "$CLONE_DIR"
  fi
  git -C "$CLONE_DIR" checkout "$CLASSIC_DB_REF"
}

# Preserve the decompressed bytes exactly for source-profile checksum verification.
assemble() {
  : > "$OUT_FILE"
  shopt -s nullglob
  # shellcheck disable=SC2206 # ASSEMBLE_GLOB requires glob expansion.
  local sql_files=("$CLONE_DIR"/$ASSEMBLE_GLOB)
  shopt -u nullglob
  if [ "${#sql_files[@]}" -eq 0 ]; then
    echo "[pull-classic-db] FATAL: nothing matched '$ASSEMBLE_GLOB' under $CLONE_DIR (upstream layout moved? adjust ASSEMBLE_GLOB)" >&2
    return 1
  fi
  local f
  for f in "${sql_files[@]}"; do
    case "$f" in
      *.gz) gzip -dc "$f" >> "$OUT_FILE" ;;
      *) cat "$f" >> "$OUT_FILE" ;;
    esac
  done
  echo "[pull-classic-db] assembled $OUT_FILE from ${#sql_files[@]} file(s)"
}

main() {
  local skip_verify=0
  if [ "${1:-}" = "--skip-verify" ]; then
    skip_verify=1
  fi

  echo "[pull-classic-db] repo=$CLASSIC_DB_REPO ref=$CLASSIC_DB_REF"
  clone_or_fetch
  local resolved_sha
  resolved_sha="$(git -C "$CLONE_DIR" rev-parse HEAD)"
  echo "[pull-classic-db] resolved commit: $resolved_sha"

  assemble || exit 1

  if [ "$skip_verify" = 1 ]; then
    echo "[pull-classic-db] --skip-verify given (expected when CLASSIC_DB_REF floats past the lock) — sha256 not checked"
  elif [ "$CLASSIC_DB_REF" != "$CLASSIC_DB_SHA" ]; then
    echo "[pull-classic-db] CLASSIC_DB_REF ($CLASSIC_DB_REF) differs from the locked SHA — pass --skip-verify to acknowledge (the lock's checksum only means something for the locked SHA)" >&2
    exit 1
  else
    verify_sha256 "$OUT_FILE" "$CLASSIC_DB_SHA256" || {
      echo "[pull-classic-db] ABORT: the assembled dump does not match the locked checksum — the classic-db repo's" >&2
      echo "  output changed shape at the SAME pinned commit. Do not import this file; investigate before trusting" >&2
      echo "  it (this is the exact drift guard the licensing firewall's 'never silently import a changed shape'" >&2
      echo "  promise requires — see docs/data-ingestion.md)." >&2
      exit 1
    }
  fi

  echo "[pull-classic-db] done: $OUT_FILE ready for importer/scripts/import-world.sh (its default \$DUMP path)"
  echo "[pull-classic-db] resolved SHA for provenance: $resolved_sha (pass to \`importer --source-sha $resolved_sha\`)"
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi

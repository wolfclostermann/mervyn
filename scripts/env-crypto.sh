#!/usr/bin/env bash
# Encrypt or decrypt .env for committing a ciphertext blob (e.g. .env.enc) to GitHub.
# Usage:
#   ./scripts/env-crypto.sh encrypt   # reads .env, writes .env.enc
#   ./scripts/env-crypto.sh decrypt   # reads .env.enc, writes .env
#
# Optional: ENV_FILE=my.env ./scripts/env-crypto.sh encrypt  → writes my.env.enc

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ENV_FILE="${ENV_FILE:-.env}"
ENC_FILE="${ENV_FILE}.enc"

# OpenSSL-compatible defaults; adjust ITER only if you change it here and in decrypt.
CIPHER="-aes-256-cbc"
PBKDF2_ITER=600000

usage() {
  echo "Usage: $0 {encrypt|decrypt}" >&2
  echo "  Uses passphrase from the terminal (not stored in the repo)." >&2
  echo "  ENV_FILE=my.env overrides the plaintext path (ciphertext is \\\${ENV_FILE}.enc)." >&2
  exit 1
}

encrypt() {
  if [[ ! -f "$ENV_FILE" ]]; then
    echo "error: $ENV_FILE not found (create it or set ENV_FILE=...)" >&2
    exit 1
  fi
  read -rsp "Passphrase: " pass1
  echo
  read -rsp "Passphrase (again): " pass2
  echo
  if [[ "$pass1" != "$pass2" ]]; then
    echo "error: passphrases do not match" >&2
    exit 1
  fi
  printf '%s' "$pass1" | openssl enc "$CIPHER" -salt -pbkdf2 -iter "$PBKDF2_ITER" \
    -in "$ENV_FILE" -out "$ENC_FILE" -pass stdin
  echo "Wrote $ENC_FILE — you can commit that file. Never commit the passphrase."
}

decrypt() {
  if [[ ! -f "$ENC_FILE" ]]; then
    echo "error: $ENC_FILE not found" >&2
    exit 1
  fi
  read -rsp "Passphrase: " pass
  echo
  printf '%s' "$pass" | openssl enc -d "$CIPHER" -pbkdf2 -iter "$PBKDF2_ITER" \
    -in "$ENC_FILE" -out "$ENV_FILE" -pass stdin
  chmod 600 "$ENV_FILE" 2>/dev/null || true
  echo "Wrote $ENV_FILE (chmod 600 if supported)."
}

case "${1:-}" in
  encrypt) encrypt ;;
  decrypt) decrypt ;;
  *) usage ;;
esac

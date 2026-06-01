#!/usr/bin/env bash
#
# Interactive deployment of seal-server to DigitalOcean App Platform.
#
# Requirements:
#   - doctl installed and authenticated (`doctl auth init`)
#   - The repo pushed to GitHub (App Platform pulls from a GitHub repo)
#   - A Dockerfile in the repo root (shipped alongside this script)
#
# Usage:
#   ./scripts/deploy-digitalocean.sh
#
# The script will:
#   1. Verify prerequisites
#   2. Prompt for app name, region, size, branch, env vars (incl. JWT_SECRET)
#   3. Generate an App Platform spec
#   4. Create the app (or update it in place if the name already exists)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------- output helpers ----------
info() { printf '\n\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[0;32m[ok]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[warn]\033[0m %s\n' "$*"; }
fail() { printf '\033[0;31m[err]\033[0m %s\n' "$*" >&2; exit 1; }

# ---------- input helpers ----------
ask() {
    local var="$1" msg="$2" default="${3:-}"
    local input=""
    if [[ -n "$default" ]]; then
        read -r -p "$msg [$default]: " input
        input="${input:-$default}"
    else
        while [[ -z "$input" ]]; do
            read -r -p "$msg: " input
        done
    fi
    printf -v "$var" '%s' "$input"
}

ask_secret() {
    local var="$1" msg="$2"
    local input=""
    while [[ -z "$input" ]]; do
        read -r -s -p "$msg: " input
        printf '\n'
    done
    printf -v "$var" '%s' "$input"
}

confirm() {
    local msg="$1" default="${2:-Y}" prompt response
    if [[ "$default" == "Y" ]]; then prompt="[Y/n]"; else prompt="[y/N]"; fi
    read -r -p "$msg $prompt " response
    response="${response:-$default}"
    [[ "$response" =~ ^[Yy]$ ]]
}

# ---------- preflight ----------
[[ -t 0 ]] || fail "Run this script in an interactive terminal."

command -v doctl >/dev/null 2>&1 \
    || fail "doctl not found. Install: https://docs.digitalocean.com/reference/doctl/how-to/install/"
command -v git >/dev/null 2>&1 || fail "git not found."

if ! doctl account get >/dev/null 2>&1; then
    fail "doctl is not authenticated. Run: doctl auth init"
fi

DO_EMAIL="$(doctl account get --format Email --no-header 2>/dev/null || echo authenticated)"
ok "doctl ready ($DO_EMAIL)"

[[ -f "$REPO_ROOT/Dockerfile" ]] \
    || fail "Dockerfile not found at $REPO_ROOT/Dockerfile. App Platform needs one to build the Rust binary."

# ---------- defaults from git ----------
default_branch="$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)"
default_repo=""
origin_url="$(git -C "$REPO_ROOT" remote get-url origin 2>/dev/null || true)"
if [[ "$origin_url" =~ github\.com[:/]([^/]+)/([^/.]+)(\.git)?$ ]]; then
    default_repo="${BASH_REMATCH[1]}/${BASH_REMATCH[2]}"
fi

# ---------- gather config ----------
info "Configure DigitalOcean App"
echo "  Press Enter to accept defaults shown in [brackets]."
echo

ask APP_NAME    "App name"                                                  "seal"
ask GH_REPO     "GitHub repo (owner/name)"                                  "$default_repo"
ask BRANCH      "Branch to deploy"                                          "$default_branch"
ask REGION      "Region (nyc, ams, fra, sfo, sgp, lon, tor, blr, syd)"      "nyc"
ask SIZE_SLUG   "Instance size slug (basic-xxs / basic-xs / basic-s / ...)" "basic-xxs"
ask INSTANCE_N  "Instance count"                                            "1"
ask APP_TITLE   "APP_TITLE (header brand)"                                  "Seal"
ask DB_PATH     "DATABASE_PATH inside the container"                        "/app/data/chat.lance"

ask_secret JWT_SECRET "JWT_SECRET (stored as an encrypted env var)"

# ---------- generate spec ----------
SPEC_FILE="$(mktemp -t seal-do-spec.XXXXXX)"
trap 'rm -f "$SPEC_FILE"' EXIT

# YAML single-quote escape: replace ' with ''
yaml_sq() { printf "%s" "${1//\'/\'\'}"; }

JWT_SQ="$(yaml_sq "$JWT_SECRET")"
APP_TITLE_SQ="$(yaml_sq "$APP_TITLE")"
DB_PATH_SQ="$(yaml_sq "$DB_PATH")"

cat > "$SPEC_FILE" <<SPEC
name: ${APP_NAME}
region: ${REGION}
services:
  - name: web
    github:
      repo: ${GH_REPO}
      branch: ${BRANCH}
      deploy_on_push: true
    dockerfile_path: Dockerfile
    instance_count: ${INSTANCE_N}
    instance_size_slug: ${SIZE_SLUG}
    http_port: 8080
    routes:
      - path: /
    health_check:
      http_path: /
    envs:
      - key: APP_HOST
        value: "0.0.0.0"
        scope: RUN_TIME
      - key: APP_PORT
        value: "8080"
        scope: RUN_TIME
      - key: APP_TITLE
        value: '${APP_TITLE_SQ}'
        scope: RUN_TIME
      - key: DATABASE_PATH
        value: '${DB_PATH_SQ}'
        scope: RUN_TIME
      - key: JWT_SECRET
        value: '${JWT_SQ}'
        type: SECRET
        scope: RUN_TIME
SPEC

info "Generated app spec (secrets redacted in preview)"
awk '
    /key: JWT_SECRET/ { in_secret=1 }
    in_secret && /value:/ { sub(/value:.*/, "value: \047***REDACTED***\047"); in_secret=0 }
    { print "    " $0 }
' "$SPEC_FILE"

confirm "Proceed with this spec?" Y || { info "Aborted."; exit 0; }

# ---------- create or update ----------
EXISTING_ID="$(
    doctl apps list --no-header --format ID,Spec.Name 2>/dev/null \
        | awk -v n="$APP_NAME" '$2==n {print $1; exit}'
)"

if [[ -n "$EXISTING_ID" ]]; then
    info "App '$APP_NAME' already exists ($EXISTING_ID). Updating in place..."
    doctl apps update "$EXISTING_ID" --spec "$SPEC_FILE"
    APP_ID="$EXISTING_ID"
else
    info "Creating new app '$APP_NAME'..."
    APP_ID="$(doctl apps create --spec "$SPEC_FILE" --format ID --no-header --wait)"
fi

ok "Deployment kicked off (app id: $APP_ID)"

# ---------- show result ----------
info "App status"
doctl apps get "$APP_ID"

LIVE_URL="$(doctl apps get "$APP_ID" --format LiveURL --no-header 2>/dev/null || true)"
if [[ -n "$LIVE_URL" && "$LIVE_URL" != "<nil>" ]]; then
    ok "Live URL: $LIVE_URL"
else
    warn "Live URL not assigned yet. Re-run: doctl apps get $APP_ID"
fi

echo
warn "Persistence: by default App Platform containers have an ephemeral filesystem."
warn "The LanceDB directory at '$DB_PATH' will reset on each deploy/restart."
warn "Attach a persistent volume via:  Apps > $APP_NAME > Settings > Resources > Add Storage"
warn "(or extend the app spec with a 'storage_volumes' block) for durable data."

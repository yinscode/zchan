#!/usr/bin/env bash
# Uploads the Linux binaries, the landing page and the service files to a server and
# installs them. Build first with deploy/build-linux.sh, or pass BUILD=server to upload
# the committed source and compile it on the server instead.
#
#   HOST=root@203.0.113.5 DOMAIN=zchan.example.com deploy/push.sh              # update code and site
#   ALIASES='www.zchan.example.com' …                                          # other names, redirected to DOMAIN
#   SITE_ONLY=1 HOST=root@203.0.113.5 deploy/push.sh                           # the site only, no restart
#   MOVE_DATA=1 HOST=root@203.0.113.5 DOMAIN=zchan.example.com deploy/push.sh  # first time: also move
#                                     the testnet wallets and channels from .demo/testnet
# NODE_URL sets the node's JSON-RPC URL as seen from the server (first install only).
# SSH_KEY picks the private key for the server (default: ssh's own choice).
# BUILD=server needs about 5 GB of disk and 3 GB of memory plus swap on the server.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${HOST:?set HOST, e.g. root@203.0.113.5}"

# The public site: landing page, playground page, docs (the viewer and its markdown),
# status page, shared stylesheet and social preview.
DOCS=(quickstart agents use-cases design x402-zchannel mpp-zchannel security privacy demo deploy testnet)
stage_site() {
  mkdir -p "$1/docs" "$1/status"
  cp site/index.html site/base.css site/og.png "$1/"
  cp crates/zchan/src/playground.html "$1/play.html"
  cp site/docs/index.html "$1/docs/"
  cp site/status/index.html "$1/status/"
  for d in "${DOCS[@]}"; do cp "docs/$d.md" "$1/docs/"; done
}

if [ "${SITE_ONLY:-0}" = 1 ]; then
  S=$(mktemp -d)
  trap 'rm -rf "$S"' EXIT
  stage_site "$S"
  COPYFILE_DISABLE=1 tar --no-xattrs -C "$S" -czf - . | ssh ${SSH_KEY:+-i "$SSH_KEY"} "$HOST" 'tar -C /var/www/zchan -xzf - && chmod -R a+rX /var/www/zchan'
  echo "site updated: landing, playground, docs, status"
  exit 0
fi
: "${DOMAIN:?set DOMAIN, e.g. zchan.example.com}"
B=$(mktemp -d)
trap 'rm -rf "$B"' EXIT
mkdir -p "$B/bin"
if [ "${BUILD:-}" = server ]; then
  git diff --quiet HEAD -- crates Cargo.toml Cargo.lock rust-toolchain.toml || echo "note: building the last commit; uncommitted changes in crates/ are not included" >&2
  git archive --format=tar.gz -o "$B/src.tar.gz" HEAD Cargo.toml Cargo.lock rust-toolchain.toml crates
else
  [ -x deploy/dist/zchan-playground ] || { echo "no Linux binaries: run deploy/build-linux.sh first, or pass BUILD=server" >&2; exit 1; }
  cp deploy/dist/zchan-* "$B/bin/"
fi
stage_site "$B/site"
cp deploy/run.sh deploy/install.sh 'deploy/zchan@.service' deploy/Caddyfile scripts/upstream.py "$B/"
if [ "${MOVE_DATA:-0}" = 1 ]; then
  if pgrep -f 'zchan-(solver|merchant|playground) --data .*\.demo/testnet' >/dev/null; then
    echo "the local testnet services are still running; stop them first so the wallets never run in two places" >&2
    exit 1
  fi
  mkdir -p "$B/data"
  cp -a .demo/testnet/solver .demo/testnet/merchant-a .demo/testnet/merchant-b .demo/testnet/merchant-d .demo/testnet/playground "$B/data/"
  cp .demo/testnet/admin-token "$B/admin-token"
fi

COPYFILE_DISABLE=1 tar --no-xattrs -C "$B" -czf - . | ssh ${SSH_KEY:+-i "$SSH_KEY"} "$HOST" "rm -rf /tmp/zchan-deploy && mkdir -m 700 /tmp/zchan-deploy && tar -C /tmp/zchan-deploy -xzf - \
  && DOMAIN='$DOMAIN' ALIASES='${ALIASES:-}' NODE_URL='${NODE_URL:-}' /tmp/zchan-deploy/install.sh; rc=\$?; rm -rf /tmp/zchan-deploy; exit \$rc"

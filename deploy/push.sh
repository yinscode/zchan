#!/usr/bin/env bash
# Uploads the Linux binaries, the landing page and the service files to a server and
# installs them. Build first with deploy/build-linux.sh, or pass BUILD=server to upload
# the committed source and compile it on the server instead.
#
#   HOST=root@203.0.113.5 DOMAIN=zchan.example.com deploy/push.sh              # update code and site
#   SITE_ONLY=1 HOST=root@203.0.113.5 deploy/push.sh                           # the two pages only, no restart
#   MOVE_DATA=1 HOST=root@203.0.113.5 DOMAIN=zchan.example.com deploy/push.sh  # first time: also move
#                                     the testnet wallets and channels from .demo/testnet
# NODE_URL sets the node's JSON-RPC URL as seen from the server (first install only).
# SSH_KEY picks the private key for the server (default: ssh's own choice).
# BUILD=server needs about 5 GB of disk and 3 GB of memory plus swap on the server.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${HOST:?set HOST, e.g. root@203.0.113.5}"
if [ "${SITE_ONLY:-0}" = 1 ]; then
  for f in site/index.html:index.html crates/zchan/src/playground.html:play.html; do
    ssh ${SSH_KEY:+-i "$SSH_KEY"} "$HOST" "cat > /var/www/zchan/${f#*:}.new && mv /var/www/zchan/${f#*:}.new /var/www/zchan/${f#*:}" < "${f%%:*}"
  done
  echo "landing page and playground page updated"
  exit 0
fi
: "${DOMAIN:?set DOMAIN, e.g. zchan.example.com}"
B=$(mktemp -d)
trap 'rm -rf "$B"' EXIT
mkdir -p "$B/bin" "$B/site"
if [ "${BUILD:-}" = server ]; then
  git diff --quiet HEAD -- crates Cargo.toml Cargo.lock rust-toolchain.toml || echo "note: building the last commit; uncommitted changes in crates/ are not included" >&2
  git archive --format=tar.gz -o "$B/src.tar.gz" HEAD Cargo.toml Cargo.lock rust-toolchain.toml crates
else
  [ -x deploy/dist/zchan-playground ] || { echo "no Linux binaries: run deploy/build-linux.sh first, or pass BUILD=server" >&2; exit 1; }
  cp deploy/dist/zchan-* "$B/bin/"
fi
cp site/index.html "$B/site/"
cp crates/zchan/src/playground.html "$B/site/play.html"
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
  && DOMAIN='$DOMAIN' NODE_URL='${NODE_URL:-}' /tmp/zchan-deploy/install.sh; rc=\$?; rm -rf /tmp/zchan-deploy; exit \$rc"

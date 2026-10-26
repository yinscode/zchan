#!/usr/bin/env bash
# Installs or updates zchan on this server, from the bundle deploy/push.sh uploads.
# Run as root: DOMAIN=zchan.example.com ./install.sh
# Wallets and channels in ./data are moved in only on a first install; an existing
# /var/lib/zchan is never overwritten.
set -euo pipefail
cd "$(dirname "$0")"
export DEBIAN_FRONTEND=noninteractive
: "${DOMAIN:?set DOMAIN to the name this server answers to}"
SERVICES=(upstream solver merchant-a merchant-b merchant-d playground)

id zchan >/dev/null 2>&1 || useradd --system --home-dir /var/lib/zchan --shell /usr/sbin/nologin zchan
install -d -o zchan -g zchan -m 700 /var/lib/zchan
install -d -m 755 /opt/zchan/bin /var/www/zchan
install -d -m 750 /etc/zchan

# Wallets and channels from the old host, first install only.
if [ -d data ]; then
  if [ -e /var/lib/zchan/solver ]; then
    echo "/var/lib/zchan already holds wallets; not overwriting them with the uploaded data" >&2
  else
    cp -a data/. /var/lib/zchan/
    chown -R zchan:zchan /var/lib/zchan
    find /var/lib/zchan -name seed -exec chmod 600 {} +
    echo "moved wallets and channels into /var/lib/zchan"
  fi
  rm -rf data
fi

# Settings: the node URL and the admin token, kept on the server.
if [ ! -f /etc/zchan/env ]; then
  {
    echo "ZCHAN_NODE_URL=${NODE_URL:-http://127.0.0.1:18232/}"
    echo "ZCHAN_ADMIN_TOKEN=$(cat admin-token 2>/dev/null || openssl rand -hex 12)"
    echo "# EXPLORER=https://explorer.example/tx/"
    echo "# ZCHAN_SQLITE_SYNC=normal  # faster payments; the last commits can be lost if the machine goes down"
  } > /etc/zchan/env
  chmod 640 /etc/zchan/env
  chgrp zchan /etc/zchan/env
fi
rm -f admin-token

# BUILD=server: compile the uploaded source here, at low priority so the node keeps up.
if [ -f src.tar.gz ]; then
  apt-get install -y -qq build-essential curl >/dev/null 2>&1
  [ -x /root/.cargo/bin/rustup ] || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain none
  rm -rf /opt/zchan/src && install -d /opt/zchan/src && tar -C /opt/zchan/src -xzf src.tar.gz
  echo "building (the first build takes a while)…"
  (cd /opt/zchan/src && CARGO_BUILD_JOBS=${BUILD_JOBS:-2} CARGO_TARGET_DIR=/opt/zchan/target nice -n 19 /root/.cargo/bin/cargo build -q --release --locked \
    --bin zchan-solver --bin zchan-merchant --bin zchan-agent --bin zchan-playground)
  cp /opt/zchan/target/release/zchan-{solver,merchant,agent,playground} bin/
fi
install -m 755 bin/* /opt/zchan/bin/
install -m 755 run.sh /opt/zchan/run.sh
install -m 644 upstream.py /opt/zchan/upstream.py
install -m 644 site/index.html site/play.html /var/www/zchan/
install -m 644 'zchan@.service' /etc/systemd/system/

# Caddy serves HTTPS and gets its own certificate.
if ! command -v caddy >/dev/null; then
  echo "installing Caddy…"
  apt-get update -qq
  apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https curl gnupg >/dev/null 2>&1
  curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/gpg.key | gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt > /etc/apt/sources.list.d/caddy-stable.list
  apt-get update -qq
  apt-get install -y -qq caddy >/dev/null 2>&1
fi
sed "s/{\$ZCHAN_DOMAIN}/$DOMAIN/" Caddyfile > /etc/caddy/Caddyfile
caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile >/dev/null 2>&1 || caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile

# Open the web ports if this server runs ufw (SSH stays allowed).
if command -v ufw >/dev/null && ufw status | grep -q "Status: active"; then
  ufw allow OpenSSH >/dev/null; ufw allow 80/tcp >/dev/null; ufw allow 443/tcp >/dev/null
fi

systemctl daemon-reload
for s in "${SERVICES[@]}"; do systemctl enable --quiet "zchan@$s"; systemctl restart "zchan@$s"; done
systemctl enable --quiet caddy
systemctl reload caddy 2>/dev/null || systemctl restart caddy

for _ in $(seq 1 60); do curl -sf http://127.0.0.1:8088/api/info >/dev/null && break; sleep 1; done
if curl -sf http://127.0.0.1:8088/api/info >/dev/null; then
  echo "zchan is running: https://$DOMAIN/ (landing) and https://$DOMAIN/play (playground)"
else
  echo "the playground did not answer; see: journalctl -u zchan@playground -n 50" >&2
  exit 1
fi

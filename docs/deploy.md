# Hosting the landing page and the playground

One server runs everything:
- **The landing page** at `https://<domain>/`.
- **The judge playground** at `https://<domain>/play`.
- **Behind them, on localhost:** the solver, the three merchants and the demo upstream API.

Caddy serves HTTPS and gets its certificate on its own. Only ports 80 and 443 are public.

The kit is in `deploy/`:

| File | What it does |
|---|---|
| `build-linux.sh` | Cross-compiles x86_64 Linux binaries into `deploy/dist/` (zig, or Docker). |
| `push.sh` | Uploads binaries, site and service files to the server, then runs `install.sh` there. |
| `install.sh` | Creates the `zchan` user, installs the services and Caddy, starts everything. |
| `run.sh` | Each service's flags (same parties, ports and prices as the testnet playground). |
| `zchan@.service` | One systemd unit per service: `zchan@solver`, `zchan@playground`, … |
| `Caddyfile` | The site as files (`/` landing, `/play`, `/docs/`, `/status/`); `/api/*` to the playground service. |

## What the server needs

- **Where:** the droplet that runs the testnet node is the best place. The services reach the node on localhost, so no tunnel is needed. A separate droplet works too; point `NODE_URL` at the node.
- **Memory:** about 1 GB free beyond the node. The services idle at under 50 MB each, but building a proof briefly takes a few hundred MB. On a separate droplet, 2 GB of RAM and 1 vCPU is plenty.
- **Disk and system:** a few MB of disk, and x86_64 Debian 12+ or Ubuntu 23.04+ with systemd. Caddy is installed from its official apt repository.
- **A name:** either a domain with an A record pointing at the droplet's IP, or the IP itself through sslip.io (`203-0-113-5.sslip.io` resolves to 203.0.113.5). Caddy gets a certificate for either.

Check the droplet first:

```bash
ssh root@<droplet-ip> 'free -h; df -h /; nproc; . /etc/os-release; echo $PRETTY_NAME'
```

## First deploy

1. **Build,** either locally or on the server.
   - **Locally:** needs a few GB free on the Mac. It uses `cargo-zigbuild` if installed (`brew install zig && cargo install cargo-zigbuild`), and otherwise a Docker container. Both cross-compile natively, with no emulation. The binaries need glibc 2.36 or newer on the server (Debian 12+, Ubuntu 23.04+).

     ```bash
     deploy/build-linux.sh
     ```

   - **On the server:** add `BUILD=server` to the `push.sh` command in step 3, and skip this step. It uploads the committed source (about 120 KB) and compiles it there at low priority, with 2 jobs (`BUILD_JOBS=` to change). The server needs about 5 GB of disk, and about 3 GB of memory plus swap beyond the node.

2. **Stop the local testnet services,** so the wallets never run in two places at once:

   ```bash
   pkill -f 'zchan-(solver|merchant|playground) --data .*\.demo/testnet'; pkill -f 'scripts/upstream.py'
   ```

3. **Deploy, moving the wallets and channels** (solver, merchants, the house wallet and every judge session) from `.demo/testnet`:

   ```bash
   MOVE_DATA=1 HOST=root@<droplet-ip> DOMAIN=<domain> deploy/push.sh
   ```

   The first install writes `/etc/zchan/env` with the node URL (`http://127.0.0.1:18232/` by default; `NODE_URL=` to change it) and the existing admin token. It moves the data only into an empty `/var/lib/zchan`, and never overwrites a server that already has wallets.

4. **Open the firewall** if the droplet sits behind a DigitalOcean cloud firewall: allow inbound TCP 80 and 443. If `ufw` is active, `install.sh` has already opened them.

5. **Check:**
   - Open `https://<domain>/` and `https://<domain>/play`, and run one session.
   - The local `.demo/testnet` copy is now a stale backup; don't start it again.

## Updating

| What changed | Command | Restarts |
|---|---|---|
| Any page: landing (`site/zchan-landing.html`, copied into `site/index.html`), playground (`crates/zchan/src/playground.html`), docs (`site/docs/` and the markdown in `docs/`), status (`site/status/`) | `SITE_ONLY=1 HOST=root@<droplet-ip> deploy/push.sh` | nothing; live in seconds |
| Code | `deploy/build-linux.sh && HOST=root@<droplet-ip> DOMAIN=<domain> deploy/push.sh` | every service, for a few seconds |
| The same, built on the server | `BUILD=server HOST=root@<droplet-ip> DOMAIN=<domain> deploy/push.sh` | every service |

Wallets and settings on the server stay as they are. Before a full push, check that no session is in the middle of a step: a restart cuts the wait short, and the judge has to refresh. Add `SSH_KEY=~/.ssh/<key>` to any of these to pick the SSH key.

`site/index.html` is the landing page with a short head added; after editing `site/zchan-landing.html`, refresh it with:

```bash
{ head -4 site/index.html; cat site/zchan-landing.html; } > site/index.new && mv site/index.new site/index.html
```

## Operating

| Task | Command (on the server) |
|---|---|
| Status | `systemctl status 'zchan@*'` |
| Logs | `journalctl -u 'zchan@*' -f` |
| Restart one service | `systemctl restart zchan@playground` |
| Settings | `/etc/zchan/env` (node URL, admin token, optional `EXPLORER` and `ZCHAN_SQLITE_SYNC`); `systemctl restart 'zchan@*'` after a change |
| House balance | `curl -s localhost:8088/api/info` |

**Database sync.** By default every commit is synced to disk before it returns, so a signed state survives a power loss. `ZCHAN_SQLITE_SYNC=normal` syncs only at checkpoints. A crashed or restarted service still loses nothing, but if the machine itself goes down, the last few commits can be lost. On the 1-vCPU droplet (Oct 27, 15 payments each), payments took a median of 91 ms with the default and 17 ms with `normal`. The hosted playground runs with `normal`.

**Topping up the house wallet:**

```bash
sudo -u zchan /opt/zchan/bin/zchan-agent send-shielded \
  "$(sudo -u zchan /opt/zchan/bin/zchan-agent shielded-address --data /var/lib/zchan/playground/house)" 2000000 \
  --data /var/lib/zchan/solver --node http://127.0.0.1:18232/
```

# ClawFS — Shared Filesystem for the Agent Fleet

ClawFS is a JuiceFS-backed shared filesystem that lets all agents in the fleet
read and write shared files. It uses MinIO (S3-compatible) for data storage and
Redis for metadata, both running on rocky (do-host1).

## Architecture

```
                         rocky (do-host1)
                    ┌─────────────────────┐
                    │  Redis :6379 (meta)  │
                    │  MinIO :9000 (data)  │
                    │  JuiceFS gateway     │
                    │    :9100 (S3 API)    │
                    │  FUSE /mnt/clawfs    │
                    └────────┬────────────┘
                             │
              ┌──────────────┼──────────────┐
              │              │              │
         Tailscale IP   Tailscale IP    Public IP
        100.89.199.14  100.89.199.14  146.190.134.110
              │              │              │
           sparky         puck         Sweden
         (Natasha)    (Bullwinkle)     (boris)
```

### Access Methods

| Node Type | Access Method | Endpoint |
|-----------|--------------|----------|
| **Rocky** (do-host1) | FUSE mount | `/mnt/clawfs` (local, symlinked `~/clawfs`) |
| **Tailscale hosts** (sparky, puck) | mc (MinIO client) → S3 gateway | `http://100.89.199.14:9100` |
| **K8s containers** (Sweden fleet) | mc → S3 gateway | `http://146.190.134.110:9100` |

**Key rule:** Only rocky uses FUSE (`/mnt/clawfs`). ALL other nodes — sparky,
puck, boris, anything in Sweden — use `mc` (MinIO client) via the S3 gateway
on port 9100. No remote node should ever connect to Redis directly (it only
listens on 127.0.0.1). No remote node needs JuiceFS installed.

### Why Not FUSE Everywhere?

- **K8s pods** lack CAP_SYS_ADMIN — FUSE is impossible
- **macOS** — MacFUSE requires kernel extension signing and reboots
- **aarch64 hosts** (sparky) — works in theory but the S3 gateway is simpler
  and avoids a direct Redis dependency

The S3 gateway gives uniform access for all node types. The tradeoff is no
POSIX semantics (no symlinks, no random writes) for remote nodes, but that's
fine for agent file sharing.

## Services on Rocky

### redis-server.service (system)

Standard Ubuntu redis-server package. Listens on 127.0.0.1:6379 only.
JuiceFS uses DB 1 (`redis://127.0.0.1:6379/1`).

Must be running before clawfs.service starts. The clawfs.service has a
pre-start check that waits up to 60 seconds for Redis.

### minio.service (system)

MinIO object storage at 127.0.0.1:9000. Bucket: `clawfs`.
Credentials in `~/.ccc/.env` (MINIO_ACCESS_KEY / MINIO_SECRET_KEY).

### clawfs.service (system)

JuiceFS S3 gateway on 0.0.0.0:9100. This is what remote nodes connect to.
Canonical source: `deploy/clawfs.service` in this repo.

### FUSE mount (manual/boot)

`juicefs mount redis://127.0.0.1:6379/1 /mnt/clawfs -d`
Provides local POSIX access on rocky. Symlinked: `~/clawfs -> /mnt/clawfs`.

## Setting Up a Remote Node

### 1. Install mc (MinIO Client)

```bash
# aarch64 (sparky, etc)
curl -sSL -o ~/bin/mc https://dl.min.io/client/mc/release/linux-arm64/mc
chmod +x ~/bin/mc

# x86_64
curl -sSL -o ~/bin/mc https://dl.min.io/client/mc/release/linux-amd64/mc
chmod +x ~/bin/mc

# macOS (puck)
brew install minio-mc
```

Make sure `~/bin` is in PATH (add to `~/.bashrc` if needed):
```bash
export PATH="$HOME/bin:$PATH"
```

### 2. Configure mc alias

For Tailscale-connected hosts (sparky, puck):
```bash
mc alias set clawfs http://100.89.199.14:9100 <ACCESS_KEY> <SECRET_KEY>
```

For K8s containers (no Tailscale):
```bash
mc alias set clawfs http://146.190.134.110:9100 <ACCESS_KEY> <SECRET_KEY>
```

Credentials: same MinIO creds from `~/.ccc/.env` on rocky
(MINIO_ACCESS_KEY / MINIO_SECRET_KEY).

### 3. Verify

```bash
mc ls clawfs/clawfs/            # list root
mc ls clawfs/clawfs/repos/      # should show CCC/
mc cat clawfs/clawfs/repos/CCC/README.md   # read a file
```

### 4. Remove any stale FUSE service

If the node had an old JuiceFS FUSE mount service, remove it:

```bash
# User-level (sparky had this)
systemctl --user stop clawfs.service
systemctl --user disable clawfs.service
rm ~/.config/systemd/user/clawfs.service

# System-level (if applicable)
sudo systemctl stop clawfs.service
sudo systemctl disable clawfs.service
```

Remote nodes should NOT have a clawfs.service — they use mc, not FUSE.

## Usage (mc commands)

```bash
mc ls clawfs/clawfs/                       # list files
mc cat clawfs/clawfs/path/to/file.txt      # read a file
echo "data" | mc pipe clawfs/clawfs/file   # write from stdin
mc cp local.txt clawfs/clawfs/             # upload
mc cp clawfs/clawfs/remote.txt ./          # download
mc rm clawfs/clawfs/unwanted.txt           # delete
mc mirror ./dir clawfs/clawfs/dir/         # sync directory up
mc mirror clawfs/clawfs/dir/ ./dir         # sync directory down
```

The bucket is always `clawfs/clawfs/` (bucket name "clawfs", then JuiceFS
volume name "clawfs").

## Shared CCC Repo

The CCC repo is seeded on ClawFS at `/mnt/clawfs/repos/CCC` (accessible via
`mc` as `clawfs/clawfs/repos/CCC/`).

- Rocky is the designated pusher (CCC_REPO_PUSHER=true)
- A 30-min sync timer (`ccc-repo-sync.timer`) auto-commits and pushes changes
- Other agents can read the repo via mc but should NOT do git operations on it
- Rocky's workspace symlink: `~/.ccc/workspace -> ~/clawfs/repos/CCC`

## Troubleshooting

### "Connection refused" to Redis from remote node

Remote nodes should NOT connect to Redis. They connect to the S3 gateway on
port 9100. If a node has a JuiceFS config pointing at `redis://100.89.199.14:6379`,
it's using the old (wrong) FUSE approach. Fix: remove the FUSE service, use mc.

### Gateway returns errors / won't start

Check on rocky:
```bash
# Is Redis running?
redis-cli ping                        # should return PONG

# Is the metadata intact?
redis-cli -n 1 GET setting            # should return JSON

# Is the gateway running?
ss -tlnp | grep 9100                  # should show juicefs
sudo systemctl status clawfs.service  # check logs
```

If metadata is gone ("database is not formatted"), see Recovery below.

### Full Recovery (metadata lost)

When Redis DB 1 is empty but MinIO has stale data:

```bash
# On rocky:
sudo systemctl stop clawfs.service
sudo umount /mnt/clawfs 2>/dev/null
mc rm --recursive --force local/clawfs/clawfs/
mc ls --recursive local/clawfs/clawfs/   # verify empty

juicefs format \
  --storage minio \
  --bucket http://127.0.0.1:9000/clawfs \
  --access-key <KEY> \
  --secret-key <SECRET> \
  redis://127.0.0.1:6379/1 \
  clawfs

redis-cli -n 1 GET setting             # verify metadata
sudo systemctl start clawfs.service     # restart gateway
juicefs mount redis://127.0.0.1:6379/1 /mnt/clawfs -d  # remount FUSE

# Reseed shared repo
mkdir -p /mnt/clawfs/repos
git clone ~/Src/CCC /mnt/clawfs/repos/CCC
cd /mnt/clawfs/repos/CCC
git remote set-url origin "$(cd ~/Src/CCC && git remote get-url origin)"
ln -sf ~/clawfs/repos/CCC ~/.ccc/workspace
```

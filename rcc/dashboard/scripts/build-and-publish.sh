#!/usr/bin/env bash
# rcc/dashboard/scripts/build-and-publish.sh
#
# Builds the WASM dashboard on sparky (aarch64) and publishes the dist/
# artifact to MinIO at agents/natasha/wasm-dist-latest.tar.gz.
#
# Usage:
#   ./build-and-publish.sh              # build + publish
#   ./build-and-publish.sh --build-only  # build only, skip MinIO upload
#   ./build-and-publish.sh --test-only   # cargo test only, no build
#
# Env vars:
#   MINIO_ENDPOINT   default: http://100.89.199.14:9000
#   MINIO_KEY        default: rocky2197fb96dde4618aa17f
#   MINIO_SECRET     default: (see script)
#   MINIO_BUCKET     default: agents
#   RELEASE          set to 1 for --release build (default: dev)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DASHBOARD_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WORKSPACE_DIR="$(cd "$DASHBOARD_DIR/../.." && pwd)"

MINIO_ENDPOINT="${MINIO_ENDPOINT:-http://100.89.199.14:9000}"
MINIO_KEY="${MINIO_KEY:-rocky2197fb96dde4618aa17f}"
MINIO_SECRET="${MINIO_SECRET:-e47696ac5fcd998be6f342bbc47d13bf5f2fcaebae0ba3e1}"
MINIO_BUCKET="${MINIO_BUCKET:-agents}"
RELEASE="${RELEASE:-0}"

MODE="${1:-}"

# ── Tool check ────────────────────────────────────────────────────────────────
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
source "$HOME/.cargo/env" 2>/dev/null || true

for tool in cargo trunk; do
  command -v "$tool" >/dev/null 2>&1 || { echo "❌ $tool not found"; exit 1; }
done

echo "✓ cargo $(cargo --version)"
echo "✓ trunk $(trunk --version)"
command -v sccache >/dev/null 2>&1 && echo "✓ sccache $(sccache --version)" || echo "~ sccache not found (builds will be slower)"

# ── Tests ─────────────────────────────────────────────────────────────────────
echo ""
echo "=== Running dashboard-server unit tests ==="
cd "$DASHBOARD_DIR"
cargo test --package dashboard-server 2>&1 | tail -5
echo ""

if [[ "$MODE" == "--test-only" ]]; then
  echo "✓ Test-only mode, done."
  exit 0
fi

# ── Build ─────────────────────────────────────────────────────────────────────
echo "=== Building WASM dashboard ==="
cd "$DASHBOARD_DIR/dashboard-ui"

BUILD_START=$(date +%s)
if [[ "$RELEASE" == "1" ]]; then
  RUSTC_WRAPPER=sccache trunk build --release 2>&1
else
  RUSTC_WRAPPER=sccache trunk build 2>&1
fi
BUILD_END=$(date +%s)
ELAPSED=$((BUILD_END - BUILD_START))

DIST_SIZE=$(du -sh "$DASHBOARD_DIR/dist" | cut -f1)
echo ""
echo "✓ Build complete in ${ELAPSED}s — dist/ size: ${DIST_SIZE}"

if [[ "$MODE" == "--build-only" ]]; then
  echo "✓ Build-only mode, done."
  exit 0
fi

# ── Package ───────────────────────────────────────────────────────────────────
GIT_SHA=$(git -C "$WORKSPACE_DIR" rev-parse --short HEAD 2>/dev/null || echo "unknown")
TARBALL="/tmp/wasm-dist-${GIT_SHA}.tar.gz"

echo ""
echo "=== Packaging dist/ → ${TARBALL} ==="
tar -czf "$TARBALL" -C "$DASHBOARD_DIR/dist" .
TARBALL_SIZE=$(du -sh "$TARBALL" | cut -f1)
echo "✓ Tarball: ${TARBALL_SIZE}"

# ── Publish to MinIO ──────────────────────────────────────────────────────────
echo ""
echo "=== Publishing to MinIO ==="
python3 << PYEOF
import urllib.request, hmac, hashlib, datetime, sys, os

payload = open("${TARBALL}", "rb").read()
endpoint = "${MINIO_ENDPOINT}"
access_key = "${MINIO_KEY}"
secret_key = "${MINIO_SECRET}"
bucket = "${MINIO_BUCKET}"
git_sha = "${GIT_SHA}"

def sign(k, msg):
    return hmac.new(k, msg.encode(), hashlib.sha256).digest()
def signing_key(secret, date, region, service):
    return sign(sign(sign(sign(("AWS4"+secret).encode(), date), region), service), "aws4_request")
def put_object(key, data):
    now = datetime.datetime.utcnow()
    amz_date = now.strftime("%Y%m%dT%H%M%SZ")
    date_stamp = now.strftime("%Y%m%d")
    region, service = "us-east-1", "s3"
    host = endpoint.split("//")[1]
    ct = "application/gzip"
    payload_hash = hashlib.sha256(data).hexdigest()
    canon_hdrs = f"content-type:{ct}\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n"
    signed_hdrs = "content-type;host;x-amz-content-sha256;x-amz-date"
    canon_req = f"PUT\n/{bucket}/{key}\n\n{canon_hdrs}\n{signed_hdrs}\n{payload_hash}"
    cred_scope = f"{date_stamp}/{region}/{service}/aws4_request"
    sts = f"AWS4-HMAC-SHA256\n{amz_date}\n{cred_scope}\n{hashlib.sha256(canon_req.encode()).hexdigest()}"
    sig = hmac.new(signing_key(secret_key, date_stamp, region, service), sts.encode(), hashlib.sha256).hexdigest()
    auth = f"AWS4-HMAC-SHA256 Credential={access_key}/{cred_scope}, SignedHeaders={signed_hdrs}, Signature={sig}"
    req = urllib.request.Request(f"{endpoint}/{bucket}/{key}", data=data, method="PUT")
    req.add_header("Content-Type", ct)
    req.add_header("x-amz-date", amz_date)
    req.add_header("x-amz-content-sha256", payload_hash)
    req.add_header("Authorization", auth)
    try:
        urllib.request.urlopen(req)
        print(f"  ✓ published {bucket}/{key} ({len(data):,} bytes)")
    except Exception as e:
        print(f"  ✗ failed {key}: {e}", file=sys.stderr)
        sys.exit(1)

put_object("natasha/wasm-dist-latest.tar.gz", payload)
put_object(f"natasha/wasm-dist-{git_sha}.tar.gz", payload)
print(f"  ✓ immutable copy: natasha/wasm-dist-{git_sha}.tar.gz")
PYEOF

echo ""
echo "✓ Done. artifact=natasha/wasm-dist-latest.tar.gz sha=${GIT_SHA} build_time=${ELAPSED}s"

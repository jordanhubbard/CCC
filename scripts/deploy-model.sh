#!/bin/bash
# deploy-model.sh — Generic vLLM model deployment for Sweden fleet containers
#
# Usage: deploy-model.sh <model-dir-name> [served-name] [minio-path]
#
# Arguments:
#   model-dir-name  Directory name under /home/horde/models/ (also the MinIO path basename)
#                   e.g. "gemma-4-31B-it-FP8_BLOCK"
#   served-name     Value for --served-model-name (default: lowercase basename sans version noise)
#                   e.g. "gemma"
#   minio-path      Full MinIO object path (default: agents/models/<model-dir-name>)
#
# Pre-requisites: model must already be uploaded to MinIO at minio-path
# Run ON the container (via HORDE SSH from sparky/puck, or RCC exec if listener is alive)
#
# Examples:
#   deploy-model.sh gemma-4-31B-it-FP8_BLOCK gemma
#   deploy-model.sh Qwen3-32B-FP8 qwen3
#   deploy-model.sh Llama-4-Scout-17B-16E-Instruct llama4 agents/models/Llama-4-Scout-17B-16E-Instruct

set -euo pipefail

# ── Args ──────────────────────────────────────────────────────
MODEL_DIR_NAME="${1:?Usage: deploy-model.sh <model-dir-name> [served-name] [minio-path]}"
SERVED_NAME="${2:-$(echo "$MODEL_DIR_NAME" | tr '[:upper:]' '[:lower:]' | sed 's/-fp8.*//;s/-it$//;s/-instruct$//')}"
MINIO_PATH="${3:-agents/models/${MODEL_DIR_NAME}}"

# ── Config (same across all Sweden containers) ────────────────
MODEL_DIR="/home/horde/models/${MODEL_DIR_NAME}"
MINIO_ENDPOINT="https://minio.yourmom.photos"
MINIO_ACCESS="${MINIO_ROOT_USER:-}"
MINIO_SECRET="${MINIO_ROOT_PASSWORD:-}"
VLLM_BIN="/home/horde/.vllm-venv/bin/vllm"
VLLM_PORT=8080  # Must match tunnel forward port (ssh -R 1808x:localhost:8080)
SUPERVISOR_CONF="/etc/supervisor/conf.d/vllm.conf"
TP_SIZE=$(nvidia-smi --list-gpus 2>/dev/null | wc -l || echo 4)

echo "=== Model Deployment ==="
echo "Host:        $(hostname)"
echo "Model dir:   ${MODEL_DIR}"
echo "Served name: ${SERVED_NAME}"
echo "MinIO path:  ${MINIO_PATH}"
echo "Port:        ${VLLM_PORT}"
echo "TP size:     ${TP_SIZE}"
echo "Started:     $(date)"
echo ""

# ── 1. Install mc if missing ──────────────────────────────────
if ! which mc &>/dev/null; then
  echo "Installing mc..."
  curl -fsSL https://dl.min.io/client/mc/release/linux-amd64/mc -o /tmp/mc
  chmod +x /tmp/mc
  sudo mv /tmp/mc /usr/local/bin/mc
fi

# ── 2. Configure mc ───────────────────────────────────────────
mc alias set clawfs "${MINIO_ENDPOINT}" "${MINIO_ACCESS}" "${MINIO_SECRET}" --no-color 2>/dev/null

# ── 3. Download model ─────────────────────────────────────────
mkdir -p "${MODEL_DIR}"
echo "Syncing from ClawFS MinIO -> ${MODEL_DIR}"
mc mirror --overwrite "clawfs/${MINIO_PATH}" "${MODEL_DIR}" --no-color
echo "Download complete. Size: $(du -sh ${MODEL_DIR} | cut -f1)"
echo ""

# ── 4. Update supervisord vllm config ─────────────────────────
echo "Updating vLLM supervisord config..."
cat > /tmp/vllm-deploy.conf << CONF
[program:vllm]
command=${VLLM_BIN} serve ${MODEL_DIR} --host 0.0.0.0 --port ${VLLM_PORT} --tensor-parallel-size ${TP_SIZE} --gpu-memory-utilization 0.92 --max-model-len 32768 --served-model-name ${SERVED_NAME}
directory=/home/horde
user=horde
autostart=true
autorestart=true
stderr_logfile=/var/log/supervisor/vllm.err.log
stdout_logfile=/var/log/supervisor/vllm.out.log
startretries=3
stopasgroup=true
killasgroup=true
CONF
sudo cp /tmp/vllm-deploy.conf "${SUPERVISOR_CONF}"
sudo supervisorctl reread
sudo supervisorctl update
echo "Restarting vLLM..."
sudo supervisorctl restart vllm

# ── 5. Wait for healthy ───────────────────────────────────────
echo "Waiting for vLLM to be healthy..."
for i in $(seq 1 120); do
  if curl -s "http://localhost:${VLLM_PORT}/health" 2>/dev/null | grep -q "{}"; then
    echo "vLLM healthy after $((i*5))s"
    break
  fi
  if [ "$i" -eq 120 ]; then
    echo "Timeout after 600s"
    sudo supervisorctl status vllm
    exit 1
  fi
  sleep 5
done

# ── 6. Confirm ────────────────────────────────────────────────
echo ""
echo "=== Done at $(date) ==="
curl -s "http://localhost:${VLLM_PORT}/v1/models" | python3 -c "import json,sys; [print('Serving:', m['id']) for m in json.load(sys.stdin).get('data',[])]" 2>/dev/null

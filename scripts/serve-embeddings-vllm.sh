#!/usr/bin/env bash
# Qwen3-Embedding-4B on vLLM, FP8 (RTX 5090 / sm_120).
# Reachable from Windows at http://127.0.0.1:8000/v1 via WSL2 localhost forwarding.
#
# Launched by serve-embeddings-vllm.ps1, which runs this file in place off
# /mnt/c so the repo copy is the only copy — the flags used to live only in
# ~/lmt/vllm-embed/serve.sh, outside version control, where a retune left no
# diff behind.
#
# The venv and HF cache still live in the WSL home; override with
# VLLM_EMBED_HOME if that tree moves.
set -euo pipefail
VLLM_EMBED_HOME="${VLLM_EMBED_HOME:-$HOME/lmt/vllm-embed}"
cd "$VLLM_EMBED_HOME"
# The logon task runs this hidden, so without a log a failed start is
# silent and `lore status` just says UNREACHABLE with no reason.
LOG="${VLLM_EMBED_LOG:-$VLLM_EMBED_HOME/serve.log}"
exec > >(tee -a "$LOG") 2>&1
echo "=== start $(date -Is) gpu-mem-util 0.20 ==="
exec ./.venv/bin/vllm serve Qwen/Qwen3-Embedding-4B \
  --runner pooling --convert embed \
  --quantization fp8 \
  --host 0.0.0.0 --port 8000 \
  --max-model-len 8192 \
  --gpu-memory-utilization 0.20 \
  "$@"

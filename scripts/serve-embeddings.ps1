# Dogfood embedding server — the D-0014 reference stack.
#
# Starts a standalone llama.cpp llama-server (CUDA) serving Qwen3-Embedding-4B
# on a fixed port for the live lore daemon. The daemon never manages this
# process (D-0007); run this at logon or whenever the daemon reports the
# endpoint unreachable. Reuses the bench's downloaded binary and GGUF
# (bench/embed/setup.ps1 fetches both).
#
# Flag rationale (measured 2026-08-16, see D-0014):
#   --kv-unified      REQUIRED for causal embedders: per-slot KV streams make
#                     llama.cpp split pooled variable-length batches into
#                     ~one-sequence decodes (throughput flat vs slot count).
#   --no-cache-prompt embedding inputs never repeat; the prompt cache is pure
#                     bookkeeping overhead (~+10% off when disabled).
#   -b/-ub 2048       beats 8192 by ~17-19% on qwen3 0.6b/4b.
#   -c 16384          unified cache only holds in-flight sequences; bigger
#                     bought nothing. Keeps max_embed_bytes 3584 token-safe.
# Resident cost: ~8.2 GB VRAM while running.
param(
    [int]$Port = 8090,
    [string]$Gguf = (Join-Path $PSScriptRoot '..\bench\embed\models\qwen3-4b.gguf'),
    [string]$LlamaExe = (Join-Path $PSScriptRoot '..\bench\embed\tools\llama\llama-server.exe')
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path $LlamaExe)) { throw "llama-server.exe not found at $LlamaExe — run bench\embed\setup.ps1" }
if (-not (Test-Path $Gguf)) { throw "$Gguf not found — run bench\embed\setup.ps1" }

& $LlamaExe -m $Gguf --embedding --pooling last `
    -c 16384 -b 2048 -ub 2048 --parallel 16 `
    --kv-unified --no-cache-prompt `
    -ngl 999 --metrics --host 127.0.0.1 --port $Port

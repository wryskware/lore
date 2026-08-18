# Dogfood embedding server — vLLM in WSL2, replacing the D-0014 llama.cpp stack.
#
# Starts vLLM serving Qwen3-Embedding-4B at FP8 inside the Ubuntu WSL2 distro,
# on a fixed port for the live lore daemon. The daemon never manages this
# process (D-0007); run this at logon or whenever the daemon reports the
# endpoint unreachable.
#
# Same model as D-0014 (Qwen3-Embedding-4B, 2560 dims, last pooling), different
# runtime and precision: FP8 W8A8 via CUTLASS on sm_120 instead of Q8_0 GGUF.
#
# Measured 2026-08-17, 256 real repo chunks / 126,957 tokens, batch 8 x conc 16:
#   vLLM FP8         20,471 tok/s   (1.19x over the same model at bf16)
#   vLLM bf16        17,274 tok/s
#   llama.cpp Q8_0    9,794 tok/s   <- the stack this replaces
# Cosine similarities track bf16 within ~0.007 on a 3-sentence probe.
#
# Resident cost: ~6.9 GB VRAM (gpu-memory-utilization 0.20 of a 32 GB card).
# Weights are ~4 GB at FP8; the rest is KV cache vLLM reserves up front.
#
# Retuned 2026-08-18 from 0.60 (~20.1 GB measured) to 0.20. This is a pooling
# embed runner with no decode loop, so the large reservation bought nothing:
# on the same 256-chunk / 210,650-token probe, 39,177 tok/s at 0.60 vs 42,106
# and 41,820 tok/s at 0.20. Cold start to /health 200 is ~40 s either way, so
# an idle stop/start supervisor would trade 13 GB for a 40 s stall on the first
# search after any idle gap; shrinking the reservation was the cheaper trade.
#
# Rollback: serve-embeddings.ps1 (llama.cpp Q8_0) plus the matching
# config.toml.bak-d0014-llamacpp in %LOCALAPPDATA%\lore.
param(
    [string]$Distro = 'Ubuntu',
    [string]$ServeScript = '~/lmt/vllm-embed/serve.sh'
)
$ErrorActionPreference = 'Stop'

# -u: run as the distro's default user, not root — the venv and HF cache live
# under that user's home.
& wsl.exe -d $Distro -e bash -lc "exec $ServeScript"

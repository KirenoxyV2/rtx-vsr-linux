#!/usr/bin/env bash
# Source this to make nvidia-vfx find its runtime deps (all pip-shipped, no system CUDA needed):
#   source spike/env.sh
# Then run:  .venv/bin/python spike/bench_vsr.py [gpu]
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SITE="$HERE/.venv/lib/python3.11/site-packages"
export CUDA_PATH="$SITE/nvidia/cuda_runtime"
export LD_LIBRARY_PATH="$SITE/nvidia/curand/lib:${LD_LIBRARY_PATH:-}"

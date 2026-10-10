#!/bin/bash
SP=/home/kirenoxy/vsr-venv/lib/python3.12/site-packages/vapoursynth
objdump -p "$SP/libvsscript.so" | grep -E "NEEDED|SONAME"
echo "=== size ==="
ls -la "$SP/libvsscript.so"
echo "=== undefined python syms ==="
objdump -T "$SP/libvsscript.so" 2>/dev/null | grep -E "UND.*(Py_|python)" | head
echo "=== vapoursynth.abi3.so NEEDED ==="
objdump -p "$SP/vapoursynth.abi3.so" | grep NEEDED | head -15
echo "=== abi3 python syms ==="
objdump -T "$SP/vapoursynth.abi3.so" 2>/dev/null | grep -cE "UND.*Py_"

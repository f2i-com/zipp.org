param([string]$LlvmDirectory = "$env:ProgramFiles\LLVM\bin")
$ErrorActionPreference = 'Stop'
$labRoot = Split-Path $PSScriptRoot -Parent
$objectPath = Join-Path ([IO.Path]::GetTempPath()) ("zipp-gpu-" + [guid]::NewGuid().ToString('N') + '.o')
try {
    # Separate compilation/linking avoids Clang invoking a Windows-incompatible
    # npm wasm-opt shim as its optional post-link optimizer.
    & (Join-Path $LlvmDirectory 'clang.exe') --target=wasm32 -O3 -msimd128 -fno-builtin -ffp-contract=off -c (Join-Path $labRoot 'wasm/kernels.c') -o $objectPath
    if ($LASTEXITCODE -ne 0) { throw "Kernel compilation failed: $LASTEXITCODE" }
    & (Join-Path $LlvmDirectory 'wasm-ld.exe') --no-entry --export-memory --export=__heap_base --initial-memory=131072 --max-memory=134217728 -o (Join-Path $labRoot 'wasm/kernels.wasm') $objectPath
    if ($LASTEXITCODE -ne 0) { throw "Kernel linking failed: $LASTEXITCODE" }
} finally {
    if (Test-Path -LiteralPath $objectPath) { Remove-Item -LiteralPath $objectPath }
}

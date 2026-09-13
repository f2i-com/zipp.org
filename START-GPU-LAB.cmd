@echo off
cd /d "%~dp0"
echo Open the Zipp playground URL printed below, then choose the GPU compute sample.
echo Keep this terminal open while experiments run. Ctrl+C stops the server.
node crates\zipp-wasm\playground\serve.cjs
pause

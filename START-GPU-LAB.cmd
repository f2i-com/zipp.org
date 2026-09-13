@echo off
cd /d "%~dp0"
echo Open the native GPU lab URL printed below.
echo Keep this terminal open while experiments run. Ctrl+C stops the server.
node crates\zipp-wasm\playground\serve.cjs
pause

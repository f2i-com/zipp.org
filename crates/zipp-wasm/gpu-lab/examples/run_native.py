"""Working Python -> host transport -> compiled WASM demo. Python itself is native here.

    python examples/run_native.py
    python examples/run_native.py cpu-js

The browser demo executes the same graph through WebGPU or WebGL2.
"""
import asyncio
import json
from pathlib import Path
import shutil
import sys
ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT.parents[1]/"zipp-vm"/"src"/"frontend"/"python"/"lib"))
from zipp_gpu import Graph

async def main():
    node=shutil.which("node")
    if not node:
        raise RuntimeError("Install Node.js 18+ for this native development harness")
    backend=sys.argv[1] if len(sys.argv)>1 else "wasm"
    async def transport(program):
        process=await asyncio.create_subprocess_exec(
            node,str(ROOT/"scripts"/"run_graph.mjs"),backend,
            stdin=asyncio.subprocess.PIPE,stdout=asyncio.subprocess.PIPE,stderr=asyncio.subprocess.PIPE)
        try:
            stdout,stderr=await asyncio.wait_for(process.communicate(json.dumps(program,allow_nan=False).encode()),30)
        except BaseException:
            if process.returncode is None:process.kill()
            await process.wait()
            raise
        if process.returncode:
            raise RuntimeError(stderr.decode("utf-8",errors="replace"))
        return json.loads(stdout)
    gpu=Graph()
    a=gpu.tensor([1,2,3,4]);b=gpu.tensor([10,20,30,40])
    c=(a*b+4).relu()
    result=await gpu.run(transport,result=c,total=c.sum())
    print("Backend:",result["backend"])
    print("Result: ",result["outputs"]["result"]["data"])
    print("Total:  ",result["outputs"]["total"]["data"][0])
    assert result["outputs"]["result"]["data"]==[14,44,94,164]
    assert result["outputs"]["total"]["data"]==[316]

if __name__=="__main__":
    try:asyncio.run(main())
    except (RuntimeError,TimeoutError) as exc:
        print("Error:",exc,file=sys.stderr);sys.exit(1)

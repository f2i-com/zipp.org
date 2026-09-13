#!/usr/bin/env node
/** One bounded JSON graph on stdin, one JSON result on stdout. No dependencies. */
import {readFile} from 'node:fs/promises';
import {createRuntime} from '../src/runtime.mjs';
const backend=process.argv[2]||'wasm';
let runtime;
try {
  if(!['wasm','cpu-js'].includes(backend))throw Error('This Node CLI accepts wasm or cpu-js. Use the browser demo for GPU backends.');
  const chunks=[];let bytes=0;
  for await(const chunk of process.stdin){bytes+=chunk.length;if(bytes>24*1024*1024)throw Error('Input exceeds 24 MiB');chunks.push(chunk);}
  const program=JSON.parse(Buffer.concat(chunks).toString('utf8'));
  const wasmBytes=backend==='wasm'?await readFile(new URL('../wasm/kernels.wasm',import.meta.url)):undefined;
  runtime=await createRuntime({backend,wasmBytes});
  process.stdout.write(JSON.stringify(await runtime.execute(program))+'\n');
}catch(error){process.stderr.write(`${error.code||'ERROR'}: ${error.message}\n`);process.exitCode=1;}
finally{runtime?.dispose();}

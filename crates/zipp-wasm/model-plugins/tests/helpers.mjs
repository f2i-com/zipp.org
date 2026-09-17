import {readFile, readdir} from 'node:fs/promises';
import {FileMapSource} from '../src/sources.mjs';
export async function directoryEntries(relative) {
  const folder=new URL(relative,import.meta.url), entries=new Map();
  async function walk(url,prefix='') {
    for(const e of await readdir(url,{withFileTypes:true})) {
      if(e.name==='__pycache__')continue;
      if(e.isDirectory())await walk(new URL(`${e.name}/`,url),`${prefix}${e.name}/`);
      else entries.set(prefix+e.name,new Uint8Array(await readFile(new URL(e.name,url))));
    }
  }
  await walk(folder); return entries;
}
export async function sourceDirectory(relative) {return new FileMapSource(await directoryEntries(relative));}

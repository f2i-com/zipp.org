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

/** A folder read on demand, the way a browser reads a chosen File: a checkpoint
 * is tens of megabytes and nothing should pull it all into memory to look at a
 * header. */
export async function lazyDirectory(target) {
  const {stat, open} = await import('node:fs/promises');
  const folder = target instanceof URL ? target : new URL(target, import.meta.url);
  const sizes = new Map();
  for (const entry of await readdir(folder, {withFileTypes: true})) {
    if (entry.isFile()) sizes.set(entry.name, (await stat(new URL(entry.name, folder))).size);
  }
  return {
    size(path) {
      if (!sizes.has(path)) { const error = new Error(`Asset not supplied: ${path}`); error.code = 'MISSING'; throw error; }
      return sizes.get(path);
    },
    async read(path, offset, length) {
      this.size(path);
      const handle = await open(new URL(path, folder));
      try {
        const out = new Uint8Array(length);
        const {bytesRead} = await handle.read(out, 0, length, offset);
        if (bytesRead !== length) throw new Error(`Short read of ${path}`);
        return out;
      } finally { await handle.close(); }
    },
  };
}

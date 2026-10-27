# Running Node programs

`zipp app.js`, `zipp app.mjs`, `zipp app.ts` and `zipp app.tsx` run a
program the way `node` runs it: with Node's globals, npm packages from `node_modules`,
CommonJS and the core modules most packages reach for. The layer lives in
`crates/zipp-vm/src/lang/js/node/`: JavaScript (as Python's runtime is) over
a few host functions, each part compiled the first time a program uses it,
so a program that uses none of it pays about half a millisecond at startup.

It is the command's. An embedder, the WebAssembly engines, the sandboxed
runner and a run with a harness prelude (the test262 runner) get the plain
ECMAScript global object. `ZIPP_NO_NODE=1` turns it off for a run.

## Globals

`process` (`argv`, `env`, `platform`, `arch`, `cwd()`, `exit()`,
`exitCode`, `nextTick`, `hrtime`, `stdout.write`, `stderr.write`, `on('exit')`,
`versions`), `global`, `Buffer`, `atob`, `btoa`, `structuredClone`, the
WHATWG `URL` and `URLSearchParams` (`url_whatwg.js`: the URL Standard's
parser, serializer and setters, passing every case of the web-platform
tests' `urltestdata.json` and `setters_tests.json`), `queueMicrotask`, and
timers with ids, arguments and cancellation:
`setTimeout`/`clearTimeout`, `setInterval`/`clearInterval`,
`setImmediate`/`clearImmediate`. `process.nextTick` callbacks run before
promise jobs, as Node orders them. A script also has CommonJS's `require`,
`module`, `exports`, `__filename` and `__dirname`; an ES module does not.

## Packages

A bare specifier (`zod`, `@scope/pkg/sub`) resolves as Node resolves it:
the nearest `node_modules/<name>` above the importing file, then its
`package.json`. `exports` decides when the package has one: subpaths, `*`
patterns, and conditions in order (`node`, then `import` for an `import` or
`require` for a `require`, then `default`; `types` is never one). Otherwise
`main` or `index`, with CommonJS's extension and folder probing.

A file's format is Node's: `.mjs` and `.cjs` by name, `.js` by its
package's `"type"`, and with no `"type"`, by whether its source has
`import`/`export` statements. TypeScript files are modules.

## CommonJS

`require` loads a file inside Node's wrapper function, caches it by its
real path, and runs it once, so a cycle sees the other module's `exports` as
far as it had got. `require` of JSON parses it; `require` of an ES module
returns its namespace; `require.resolve`, `require.cache` and `require.main`
are there. An ES module importing a CommonJS module gets `module.exports`
as its default import (as Node, whatever `__esModule` says) and its
properties as named imports. Node finds those names by reading the source;
zipp takes the module's own enumerable properties after it ran, which can
be more names than Node's, never fewer of the ones a program can use.

## Core modules

Implemented, as `require('x')`, `require('node:x')` and `import`:
`path` (POSIX and Windows), `events`, `util` (`format`, `inspect`,
`inherits`, `promisify`, `callbackify`, `deprecate`, `debuglog`,
`isDeepStrictEqual`, `types`), `fs` (the synchronous API, and its callback
and promise forms: read, write, append, stat, readdir, mkdir, rm, rename,
copy, realpath), `os`, `tty`, `url` (file URLs, legacy `parse`/`format`),
`module` (`createRequire`), `assert` and `assert/strict`, `buffer`,
`crypto` (`randomUUID`, `randomBytes`, `getRandomValues`, and `createHash`
and `createHmac` for md5, sha1 and sha256; the randomness is
`Math.random`'s, not a cryptographic source), `string_decoder`, `timers`,
`process`.

The other core modules (`http`, `net`, `child_process`, `stream`, `zlib`,
`worker_threads`, …) can be required, as many packages do at their top, and
their members read and their classes extended (`react-dom/server` extends
`stream.Readable` as it loads); calling or constructing one throws
`ERR_ZIPP_UNSUPPORTED` naming it.

Not there yet: `fetch`, streams, and `fs` streams and watchers.

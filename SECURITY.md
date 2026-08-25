# Security policy

Security reports are welcome. zipp parses and executes attacker-controlled
JavaScript, so parser, VM, JIT, regular-expression, host-bridge, module-loader,
and resource-accounting defects can all have security impact even when they
would be ordinary correctness bugs in another project.

## Reporting a vulnerability

Please do not open a public issue containing a vulnerability, exploit, or
unpatched denial-of-service technique.

1. Use GitHub's **Report a vulnerability** flow to open a private repository
   security advisory:
   <https://github.com/f2i-com/zipp.org/security/advisories/new>.
2. If private reporting is unavailable, open a public issue containing only a
   request for a private maintainer contact. Do not include technical details
   in that issue.

Include the affected commit or release, platform and architecture, invocation
and sandbox options, a minimal reproducer, expected versus actual behavior, and
your assessment of impact. Remove credentials and third-party data. Please say
whether the issue is already public or has been reported elsewhere.

We aim to acknowledge a complete report within seven days and to provide a
status update at least every fourteen days while it remains open. Timelines for
a fix and disclosure depend on severity and release coordination. We will
credit reporters who want attribution. Please test only on systems and data you
are authorized to use.

Security fixes target the latest published release and `main`. Older revisions
are not supported unless a release announcement says otherwise.

## Threat model

For an untrusted-code deployment, assume the guest controls its complete script,
all runtime-generated source (`eval`, `Function`, and related APIs), regular
expressions, values crossing guest entry points, and every module below an
explicitly enabled import root. The guest may deliberately trigger worst-case
CPU, memory, output, recursion, parsing, garbage-collection, and host-call
behavior. Guest-visible names, paths, IDs, JSON, and queued host requests must
all be treated as hostile.

The executable and its build inputs, browser/runtime, configured import root,
and host bridge implementations are trusted. A writable import root, a bridge
object supplied by a guest, or a host that dispatches guest strings to arbitrary
properties, URLs, commands, database collections, or tenant resources violates
this model.

### Native `zipp sandbox`

`zipp sandbox` is defense in depth, not an OS isolation boundary. It supervises
a child process, clears its environment and stdin, restricts module resolution,
disables native VM and regex JIT execution for the run, and applies time,
instruction, heap, source, and output limits. These controls reduce attack
surface and contain many language-level failures.

The child still runs under the invoking user's OS identity in a native process.
It does not independently deny filesystem or network system calls, and it is not
a memory-safety boundary. A bug in native code, unsafe dependency code, the
parser, VM, garbage collector, or a native builtin can invalidate the
language-level assumptions. Resource meters are also not exact RSS accounting,
and a single expensive native operation may run between instruction polls.

Do not use native `zipp sandbox` as the sole boundary for arbitrary hostile code.
For the strongest isolation, add a dedicated unprivileged account plus platform
controls, or run it in an appropriately hardened OS sandbox, container, or
microVM with explicit filesystem, network, process, memory, and CPU policy.

### WebAssembly embedding

For deployments that cannot use an OS VM, the recommended boundary is
`zipp-wasm`, built from its separately isolated Cargo workspace. It selects
zipp-vm's `safe-sandbox` profile, excludes native JITs and shared-memory guest
APIs, and makes synchronous host capabilities default-deny per `Engine`.

Run each untrusted tenant in a dedicated Web Worker with its own WASM instance.
Enforce a deadline from a different, responsive context and terminate the Worker
when it expires. Dispose of and recreate the Worker/WASM instance between
tenants; do not rely on reusing an `Engine` as a confidentiality boundary.

The host must grant only the exact operations required and must enforce tenant,
collection, row/ID, key-prefix, room, origin, and network authorization itself.
Treat every asynchronous `host.call` item drained from the guest queue as data,
not as an authorized command. Bridge implementations must be trusted,
synchronous where the API requires it, bounded, and must not synchronously
re-enter the same `Engine`. Render guest console/output strings as text, never
as HTML or terminal/control markup.

WebAssembly limits memory corruption to the runtime's linear-memory boundary,
but it does not make ambient browser authority safe. The Worker, message
handler, imported functions, origin policy, browser, and host bridge remain part
of the trusted computing base. Use an OS sandbox or microVM as an additional
layer when the risk warrants it.

## Deployment checklist

- Use `zipp-wasm` in a dedicated Worker for hostile code, or add an external OS
  isolation layer around the native runner.
- Build with committed lockfiles and the documented safe profile; do not combine
  `safe-sandbox` with native JIT features through Cargo feature unification.
- Start with no imports and no host capabilities, then grant the minimum needed.
- Keep import trees host-controlled and read-only for the complete run.
- Scope database, storage, clipboard, sync, and network bridges to one tenant.
- Bound source, messages, results, logs, memory, CPU, and wall time outside the
  guest as well as inside it.
- Cap concurrent Workers and aggregate origin/process memory; the linked WASM
  memory maximum applies independently to every live instance.
- Recreate the process or Worker between mutually untrusted tenants.
- Keep Rust, browser/runtime, dependencies, and the host application patched.
- Run the security workflow and dependency audit before release.

## Particularly useful reports

High-value findings include native memory unsafety; escape from WASM linear
memory or the Worker boundary; execution of a denied host operation; import-root
escape through path, link, race, or module-kind confusion; JIT execution in a
safe profile; bypass of terminal resource exhaustion; cross-tenant state reuse;
host exception or secret disclosure; and small inputs that cause uncontrolled
host CPU or memory growth.

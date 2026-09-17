# SoftN integration: implemented boundary, remaining host work

**No SoftN source was changed and no `.softn` application was executed here.**
The implemented `sourceFromBundle(entries, prefix)` adapts a host-supplied Map of
already validated package entries. It is not a ZIP extractor, a replacement SoftN
loader, a new SoftN manifest schema, or a claim that current releases recognize
these paths/permissions automatically.

The inspected SoftN source uses a ZIP bundle with `manifest.json`, UI/logic and
assets. A suitable proposed asset layout is:

```text
manifest.json                         # existing SoftN manifest
ui/...                                # app UI
logic/...                             # app logic
assets/ai/plugins/tiny-causal/plugin.json
assets/ai/plugins/tiny-causal/architecture.py
assets/ai/plugins/tiny-causal/graph.py
assets/ai/plugins/tiny-causal/tokenizer.py
assets/ai/models/helper/model.json
assets/ai/models/helper/weights.safetensors
```

The model manifest's weights path remains `weights.safetensors`, relative to its
scoped model root. Neither the model nor plugin receives the entire application
archive, another app's files, or an unrestricted filesystem/network URL.

The host wiring, once its existing package loader has safely produced the entry
Map and user approval, is:

```javascript
import {sourceFromBundle, PluginRegistry, ModelSession} from './model-plugins/src/index.mjs';

// archiveEntries: Map<string, Blob | Uint8Array>, from the HOST'S validated loader.
const pluginSource = sourceFromBundle(archiveEntries, 'assets/ai/plugins/tiny-causal');
const modelSource = sourceFromBundle(archiveEntries, 'assets/ai/models/helper');
const registry = new PluginRegistry(hostModelLimits);
const plugin = await registry.install(pluginSource, {approve: hostPluginApproval});
const session = await ModelSession.open({
  source: modelSource,
  plugin,
  engineFactory: () => new Engine(),
  runtime,
  limits: hostModelLimits,
});
```

`archiveEntries`, `hostModelLimits`, `hostPluginApproval`, `Engine` and `runtime`
are explicit host integration points, not functions this SDK invents inside
SoftN. `sourceFromBundle` accepts canonical ASCII entry names only; other package
names may need a scoped host adapter. It currently checks all incoming entry paths.

Required SoftN work before shipping:

1. Register the plugin and model files as binary/text assets using the actual
   SoftN manifest/assets mechanism. Review package file-type and size limits.
   Enforce archive entry count, compressed/uncompressed byte limits, duplicate
   names, traversal and expansion limits before extraction. A post-extraction
   Map limit is too late to prevent an oversized archive allocation.
2. Keep weights out of `.logic`, JSON/base64 source maps and Python VFS loading.
   Route them only into model asset sources. Scope each app/model separately.
3. Add user-visible permission/consent for executing bundled Python plugins and
   consuming local model memory/compute. The new SDK's source manifest is not a
   substitute for actual SoftN permission enforcement. Do not silently enable
   Python-to-JavaScript interop, network access or custom shaders.
4. Own model engines in a dedicated Worker lifecycle. Expose an app-level local
   model API with session IDs, generation cancellation, bounded output and app
   shutdown cleanup. The Python `model.open` host-request API is not implemented
   by this ZIPP overlay; do not route it through arbitrary eval.
5. Validate transactional install/update, archive/model/plugin hashes, restart,
   cancellation, model replacement and multiple app isolation. Integrate asset
   version pins into existing package integrity checks, rather than downloading
   a new plugin revision during app startup.

A fully bundled app can then supply the same files as the local-file lab without
resolving Hugging Face or another remote model registry. A user may instead attach
an external compatible model folder, keeping the app small. That is a host UX
choice over the same source contract, not a different inference implementation.
Only include model weights/tokenizer files that the app publisher has permission
to redistribute. No third-party model weights are included in this deliverable.

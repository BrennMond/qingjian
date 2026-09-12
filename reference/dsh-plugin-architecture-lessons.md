# DSH plugin architecture: findings and what transfers to a configuration-driven input-method engine

Source of record: local checkout `/home/brennmond/dshsrc/`. Every claim below was read from a file
listed in the text. Items I could not verify are marked **UNVERIFIED**.

## 1. Mechanism-by-mechanism findings

### 1.1 Plugin/extension model

**Declaration.** A plugin is a Node module with one of two shapes (`packages/AGENTS.md:5`): a service
package default-exports its `Service` class, or a function plugin named-exports `name` / `inject` /
`Config` / `apply` and has *no* default export. Mixing the forms makes the Loader discard `inject`
(the exact production crash in `docs/postmortem/0001-acp-default-export-drops-inject.md`). The
package manifest adds roles under a `dsh` field — `packages/util/package-manifest/src/types.ts:28-37`:

```ts
export interface DshManifest {
  manifestVersion?: 1
  bundle?: DshBundleManifest      // { patch: string }
  profile?: DshProfileManifest    // { bundles?: string[]; patchReload?: 'live'|'startup' }
  client?: DshClientManifest      // { platform: string; inject?: string[]; immediately?: boolean; external?: string[] }
}
```

**Discovery and loading.** There is no registry service. A running product is a *plugin tree composed
at boot from ordered patch layers* (`docs/architecture.md:15-27`). Each layer is a list of rows
(`id`, `name`, `config`, `disabled`, `inject`) applied over an empty list; the Loader
(`vendor/loader/src/index.ts`) then imports each named module and mounts it as a Cordis *fiber*.
Composition order is bundle layers → profile `cordis.patch.yml` → `$DSH_HOME/cordis.patch.yml` →
`--patch` overlays (`apps/cli/src/profile-boot.ts:206-244`).

**Installation.** `dsh plugin` is a thin pnpm forwarder: it runs `pnpm <args>` in
`$DSH_HOME/profiles/<name>`, then reconciles `dsh.profile.bundles` against installed state
(`apps/cli/src/plugin.ts:120-163`, `reconcilePlugins:59-91`). A dependency that resolves to a package
declaring `dsh.bundle` joins the layer stack; a bundle-less dependency only warns. Plugins are
in-process — same Node process, native ESM import, no IPC and no serialization boundary.

**Disabled/absent without breaking the host.** Three distinct mechanisms, and they are *not*
interchangeable:

- `disabled: true` (or a `!!js` expression evaluating truthy) removes a row; ancestors propagate, and
  groups can never be disabled (`vendor/loader/src/config/entry.ts:84-108`). The base bundle itself
  ships `hmr` disabled (`packages/bundle/base/cordis.patch.yml:19-24`).
- A **required** `inject` that is never satisfied leaves the fiber `PENDING` and never applies
  (`vendor/cordis/src/fiber.ts:147-154,314-318`).
- An **optional** service is read with `ctx.get('name')` at use time; absence degrades a feature, not
  the process. Real examples: `packages/fs/tool-fs-search/src/search-core.ts:390-392` warns and skips
  result spill; `packages/compaction/compaction-basic/src/index.ts:279-285` ("Pruning is optional so
  compaction-basic remains independently composable"); `packages/client/modules/src/index.ts:543`
  lazily `ctx.inject(['webServer'], …)` only if the service is absent. A full host
  (`packages/web/web-server/src/index.ts`) is optional to a plugin that may run elsewhere.

**Failure is loud, not silent.** `assertEntriesActivated` (`packages/boot/app-boot/src/index.ts:722-755`)
audits the settled tree and **throws** if any enabled entry is `FAILED` or still `PENDING`, naming the
missing injected services; boot then disposes the partial context. `EntryGroup.update` is
all-or-nothing: one throwing plugin rolls back the whole composed tree (`vendor/loader/src/config/group.ts:59-106`).
So graceful degradation is a property of *explicit* optional reads and `disabled` rows, never of an
accidentally broken plugin.

**Version mismatch.** There is **no per-plugin API version and no compatibility enforcement.**
`engines.dsh` is documentary: `packages/util/package-manifest/src/types.ts:21` says "DSH compatibility
is declarative until a reader enforces it", and no reader enforces it. The only surfaces with real
version negotiation are content-addressed client bundles (`packages/client/modules/src/index.ts:163-164`:
"Versioned code is immutable; mismatched revisions are rejected instead of serving newer bytes" — a
stale `?rev=` gets a 404) and `packages/typert/protocol/src/index.ts:254-257`, which throws on
`version !== 1`. Durable session data instead uses version-named adjacent migrations and refuses a
future header (`packages/session/session-persistence-jsonl/src/index.ts:530-537`).

### 1.2 Capability/provider declarations

A capability is a **capability seam** with three roles — Service Definition (an abstract `Service`
owning a `ctx.<key>`), one or more providers, and consumers (`docs/glossary.md:9`). Resolution is
**both by typed interface and by string key**: a provider calls a typed method
(`ctx.llm.registerAdapter(providers, adapter)`), the registry binds it under string route names.

Conflict handling is explicit and mostly **reject-the-second**:

- `LlmRuntime.registerAdapter` throws `LlmError('… already registered', 'DUPLICATE_ADAPTER')` and is
  all-or-nothing; `replace()` validates the whole candidate set first so no request sees a gap
  (`packages/llm/llm/src/index.ts:294-326,379-399`).
- Tool names are unique per scope: `packages/core/tools/src/index.ts:719-721` throws
  ``tool "${name}" is already registered`` and points the author at `agent.ctx` for a per-agent variant.
- One scope's presentation mode is "one cell rather than an entry table: two answers to 'which form
  does the model see' is a contradiction, not a merge" (`packages/core/tools/src/index.ts:711-716`).

There is no priority field and no numeric rank. **Ordering** is expressed three ways:

1. **Service dependency**: `inject` means load order is derived from availability, so row order in the
   patch file carries no semantics — stated verbatim in `packages/bundle/base/cordis.patch.yml:11-12`
   ("Row order carries no load semantics (activation is service-availability driven)").
2. **Event dispatch mode and registration order**: `emit`/`waterfall`/`serial`/`bail` run listeners in
   registration order, with `prepend: true` as the only escape (`vendor/cordis/src/events.ts:94-114,143`).
3. **Allocated named positions** for prompt sections, a fixed central table
   (`packages/core/system-prompt/src/index.ts:121-154`) rather than plugin-chosen numbers.

Two safety properties are worth naming: `ctx.tools.guard()` is a *monotonic* final denial that later
listeners cannot undo, and the pre-execute waterfall is "the reorderable policy layer"
(`docs/cookbook/extension-cookbook.md:24-33`). And scoped registration shadows global per agent, with
nearest-definition-wins (`packages/core/scope/README.md`).

### 1.3 Configuration/schema system

Every plugin may declare `Config`, a `schemastery` `Schema` (`vendor/schemastery/src/index.ts`), applied
by the Cordis fiber — `vendor/cordis/src/fiber.ts:50-60`:

```ts
export function resolveConfig(runtime: Plugin.Runtime, config: any) {
  if (!runtime.Config) return config
  const result = runtime.Config['~standard'].validate(config)
  if ('then' in result) throw new TypeError('Async config validation is not supported')
  if (result.issues) { throw new ValidationError(result.issues) } else { return result.value }
}
```

Errors are **loud by default**: a `ValidationError` with `$`-rooted paths propagates through
`Entry._start` → `EntryGroup.update` rollback → `boot()`'s "plugin tree failed to load". Silent
fallback happens only for a node explicitly marked `.loose()` (`vendor/schemastery/src/index.ts:491-494`),
and `autofix` — the other silent path — has **no caller** in `packages/` or `vendor/`. Real schemas:
`packages/core/tools/src/index.ts:783-786` (`z.union(['native','ptc','both']).default('native')`),
`packages/host/webserver/src/index.ts:125-131` (`z.const('127.0.0.1')` / `.required()` / `z.natural().max(65535)`).

**Layering — two different mechanisms, and the distinction matters:**

- **Composition patches** (`cordis.patch.yml`) target a row by `id` and replace **whole top-level
  keys**: `config: {...}` replaces the entire config object, no deep merge; unspecified keys survive
  (`vendor/include/src/index.ts:58-128`). A patch matching nothing warns and is skipped; the row is not
  invented. Inserts are indexed so a later layer can patch an earlier layer's inserted row
  (`:96-101`). There is **no** Rime-style auto-paired `*.custom.yaml`; the analogue is an explicit,
  ordered patch list plus `--patch` overlays.
- **Runtime settings** (`packages/settings/settings/src/index.ts`) *is* a per-namespace layered system:
  schema defaults → composition `base` → user document section, with `mergeLayers` deep-merging plain
  objects (arrays replace) (`:740-753,287-295`). Writes touch only the user layer, so `replace({})` is a
  true reset; writes are serialized per namespace and carry `expectedRevision`, rejecting a stale
  writer with `SettingsConflictError`; an invalid stored section keeps the last good value and warns
  (`packages/settings/settings/README.md:66-76`, `settings-file/src/index.ts:211-231`). This is the
  closest thing DSH has to a safe patch overlay.

**One expression language exists, and it is code.** `!!js` scalars become `__jsExpr` nodes
(`vendor/include/src/index.ts:9-23`) evaluated by
`new Function('ctx','expr','with (ctx) { return eval(expr) }')` (`vendor/loader/src/config/utils.ts:5-9`).
It is permitted only on `config` and `disabled`; every other entry field stays literal precisely
because an expression there "remains truthy data and silently changes composition"
(`scripts/verify-cordis-config.ts:1-9`).

### 1.4 The "everything is a plugin" boundary

The slogan is real but qualified. `docs/architecture.md:11` states it; the generated taxonomy then
names the exceptions (`docs/config-catalog.md:3550`, `## Library packages (no plugin entry)`: "a
`cordis.yml` cannot load them"). `scripts/gen-config-catalog.ts:617-643` classifies a package as
`'library'` when it has neither a default export nor an `apply`; the catalog counts ~52 library
packages and ~16 seam packages against ~200 loadable plugins. `docs/architecture.md:66` itself labels
`core/scope` "library, no key".

The visible reasoning for drawing the line:

- **No `ctx`, no state, no events ⇒ library.** "It is a library of pure functions, **not** a cordis
  service or plugin: it takes no `ctx`, registers nothing, holds no cross-call state, and emits no
  events" (`.agents/notes/implemented/architecture/2026-07-06-timeout-deadline-library.md:19`). Same
  rule in `packages/util/http-proxy/README.md:28` ("Transport policy has one answer per process:
  nothing to swap") and `packages/sdk/protocol/README.md:12`.
- **Profile-independent ⇒ build-static.** The session-format migration catalog "is build-static and
  profile-independent, so mounting or omitting the producer plugin cannot change whether an old
  artifact migrates" (`.agents/notes/implemented/architecture/2026-08-31-alpha-historical-unknown-event-refusal.md:19`).
- **Needs coordinated change across known packages ⇒ closed union, not an extension point.**
  "`WebFetchBody` is a closed discriminated union because body kinds require coordinated changes to
  the seam, provider, and tool rather than independent plugin extension"
  (`.agents/notes/implemented/architecture/2026-06-24-web-capability-seam.md:231`).
- **The loop is `core`, not a seam.** `packages/core/tools` is a loadable plugin but classified `core`
  (`docs/capability-seams.md:511`); the pipeline skeleton (pre-execute → guards → execute →
  post-execute → result) is core and only the *policy* is a listener
  (`packages/core/tools/src/index.ts:1444-1592`). `AGENTS.md:112`: "**Plugins, not loop changes**".

### 1.5 Cost of the plugin boundary

Indirection is not free and the authors say so: "A waterfall + emit is less direct than
`await ctx.fileContext.edit(...)`. The payoff is removing the tool-to-policy method dependency while
keeping the default policy plugin; the cost is one more event vocabulary to learn"
(`.agents/notes/implemented/architecture/2026-06-26-file-context-as-event-gate.md:169`). Measured
numbers exist for data-path routing (worst p95 regression 3.150% against a 5% budget,
`.agents/notes/implemented/architecture/2026-09-01-v2-embedded-assistant-streams.md:54`) and for
`structuredClone`/freeze of request extensions (500 entries → 0.60 ms warm median,
`.agents/notes/implemented/architecture/2026-08-21-deepseek-llm-api-request-extensions.md:37-45`).
**UNVERIFIED: no benchmark in the repo compares a direct in-process call against event dispatch for
the agent loop or tool pipeline.** `BENCHMARK.md` is three lines pointing at the Python SDK guide.

Keeping the core lean is enforced structurally, not by discipline:

- Hot paths never cross a plugin boundary: "The hot path never blocks on I/O — persistence plugins
  buffer asynchronously" (`packages/core/session/src/index.ts:674-676`), and event dispatch to a
  listener is contained so "one bad subscriber never breaks core lifecycle"
  (`docs/defensive-patterns.md:29-33`).
- **Bundle purity** is a build error: `packages/client/tsdown.client.ts:482-499` rejects any
  cross-plugin `@deepseek-ai` value import ("a cross-plugin value import either inlines a duplicate
  runtime instance or requires a specifier the module table cannot answer").
- **Static gates** in `scripts/` (55 `verify-*`/`.mjs` files) pin the boundary: package dependency
  faces, client domain layering, runtime closure, optional-dependency imports, application entrypoints.
- **Scale is bounded and deliberate.** One profile mounts ~152 distinct rows (~126 active after
  disables): base is 84 rows with 2 disabled, and the web bundle adds 94 rows with 24 disabled
  (`packages/bundle/base/cordis.patch.yml`, `packages/bundle/web-app/cordis.patch.yml`). `sdk-minimal`
  deliberately does *not* apply base. **UNVERIFIED: no note quantifies total boot wall-time as a
  function of those rows.**

### 1.6 Extensibility vs security

The line is explicit and unambiguous: **plugins are trusted code, full stop.**

- `SAFETY.md:9` — "The project can execute model-generated code and commands, load third-party
  plugins… untrusted plugins may damage the host computer". `SAFETY.md:23` — "Review plugins,
  configuration, and proposed commands before allowing them to run."
- The one dynamic-plugin system says so in its own design note:
  "Neither restricts the authority of exposed services: a temporary Plugin can call `ctx.shell` with
  the host executor's privileges… This is an opt-in development tool with **bash-equivalent trust, not
  a security boundary or product default**"
  (`.agents/notes/implemented/feature/2026-07-08-self-referential-cordis-toolset.md:17`), and a
  capability-restricted sandbox was rejected: "A real one (separate process, permission prompts) was
  out of scope… and would fight the entire point" (`:80`).
- There is **no plugin permission model, no manifest capability list, no signature check**. The `vm`
  realm there isolates accidental global pollution only.
- What *is* defended is narrower and still instructive: spawn scrubbing of credential-shaped env
  (`docs/defensive-patterns.md:39-45`), and refusing config files that decide how the process launches or
  where the network connects (`packages/boot/app-boot/src/index.ts:92-117`, `BOOTSTRAP_NAMES`/`BOOTSTRAP_PREFIXES`)
  — "the project's code running under the agent's policy is the deal; the project rewriting that policy
  is not" (`.agents/notes/implemented/architecture/2026-08-04-configuration-source-ownership.md`).
- The trust asymmetry is used deliberately: `AGENTS.md:119` — "Trust TypeScript at typed same-process
  boundaries… validate at parser/config, queued, model/tool JSON, durable/file, worker, process, and
  wire boundaries."

## 2. What is genuinely transferable to a configuration-driven input-method engine

1. **Separate "no `ctx`" libraries from loadable schemes.** The mechanical test is transferable even
   though the runtime is not: if a component has no key, registers nothing, holds no cross-call state
   and emits no events, it is a library, not a scheme. This is how a Rust engine should keep
   segmentation, table lookup, and codecs *out* of the scheme trait, so a scheme cannot be used as a
   smuggling route into the hot path.
2. **A declarative manifest with roles, plus a schema per component.** `DshManifest`'s shape
   (identity + role fields, one role per consumer) maps directly to a scheme manifest: identity
   (`id`, `version`), kind (`pinyin` / `cangjie` / `wubi`), a declared engine hook, and the schema its
   parameters validate against.
3. **Schema-validated config with loud failure and defaults in the schema.** `resolveConfig`
   (`vendor/cordis/src/fiber.ts:50-60`) is a small, portable rule: no schema → accept; schema → validate
   or fail. Combine with `docs/config-catalog.md`, generated from the schemas, to get an exhaustive
   human-readable field list for free. "Misconfiguration fails loud" (`AGENTS.md:117`) is exactly the
   right posture for a scheme file a user edits.
4. **Service-availability-driven dependency ordering instead of a numeric priority.** `inject` +
   fiber `PENDING` is the cleanest idea here: a scheme that needs a table loader never races it, and
   there is no arbitrated priority number to mis-set. Rust analogue: typed capability requirements
   resolved at load, with an unsatisfied requirement leaving the scheme *pending*, not half-applied.
5. **Two-layer override with an explicit reset.** The settings seam is the transferable patch design,
   not the `cordis.patch.yml` one: deep-merge a user layer over a composition base, keep the layers
   detachable, gate writes with a revision, and make `replace({})` re-inherit defaults. Rime's
   `*.custom.yaml` pain — no way to un-patch, no provenance of which layer won — is precisely what
   this separates.
6. **Explicit optional capability reads that degrade a feature, not the engine.** `ctx.get()` +
   warn-and-skip is the pattern for anything optional (a fuzzy matcher, a cloud ranker), and it is
   strictly better than a build feature flag because absence is handled at one call site.
7. **Monotonic guards for invariants.** `ctx.tools.guard()` — a denial no later listener can undo — is
   the right shape for IME invariants (never emit above a size bound, never leave composition state
   unbalanced). Reorderable policy stays reorderable; invariants are unreorderable.
8. **Content-addressed integrity for any shipped artifact.** The client-module rule — "versioned code
   is immutable; mismatched revisions are rejected instead of serving newer bytes" — transfers to
   compiled scheme tables: hash them, address them by hash, refuse a mismatch loudly.
9. **Static verification gates as the real enforcement.** The insight is not the JS tooling but the
   stance: the boundary is kept honest by `scripts/verify-*` gates in CI, not by documentation. A Rust
   engine should have `cargo` gates asserting "no scheme crate depends on another scheme crate" and
   "no runtime `eval`/`dlopen` in the scheme path".

## 3. What does NOT transfer, and why

1. **The plugin *runtime* itself.** Cordis gives reversible effects, fiber lifecycle, hot reload and
   unload-to-quiescence — a large dynamic object-graph machine. A Rust IME with a hard memory budget
   should not carry a lifecycled context tree per scheme. What transfers is the *declaration* (manifest
   + schema + capability requirements), not the fiber machinery.
2. **In-process untrusted plugin code.** DSH's model is trusted plugins with bash-equivalent authority;
   its only dynamic-plugin system explicitly refuses to be a security boundary. The stated constraint
   here is the opposite: never execute untrusted scripting. So `apply(ctx)` as arbitrary code, `!!js`
   config expressions, and `new Function`/`eval` config interpolation
   (`vendor/loader/src/config/utils.ts:5-9`) are all disqualified. A scheme must be *data plus a
   statically linked engine hook* — declarative config and compiled code only.
3. **The `cordis.yml`-style whole-key config replacement.** The base bundle's own header warns that "a
   patch replaces the targeted row's whole `config` rather than merging into it"
   (`packages/bundle/base/cordis.patch.yml:5-7`) and responds by splitting rows across bundles so each
   row has one owner. For an IME that is the wrong trade: users expect to override two keys of a table
   without restating the rest, which is why the deep-merging settings seam transfers and the
   patch-list does not.
4. **Failure semantics that abort the boot.** `assertEntriesActivated` refuses to start on any broken
   enabled plugin (`packages/boot/app-boot/src/index.ts:722-755`). Correct for a developer harness a
   human is debugging; wrong for an IME where one bad user-added scheme must not make typing
   impossible. Here the DSH *exception* is the rule: treat every scheme as optional, keep the last-good
   compiled table, and surface a diagnostic.
5. **Event-waterfall dispatch on the keystroke path.** DSH tolerates a waterfall + emit per tool call
   and admits the cost. A keypress path cannot: it should be a static match on a resolved scheme
   descriptor, with interception reserved for coarse, non-per-keystroke points.
6. **String-keyed runtime resolution of everything.** `ctx.<key>` lookup by name is what makes DSH
   replaceable, but it also defers errors to runtime and requires `ctx.get()` discipline plus the
   postmortem's rules about required-vs-optional reads
   (`packages/AGENTS.md:6`, `docs/postmortem/0001`). With no dynamic loading, a Rust engine should
   resolve providers at compile time and keep name lookup only for the serialized scheme *file*.
7. **The row-count scale.** ~126 active plugins is affordable when each is an npm package in a desktop
   app. An IME has a fixed, small scheme set; a 100-row composition language would be pure overhead.
   The transferable part is compositional *declaration*, not compositional *volume*.

## Files read (representative)

`docs/architecture.md`, `docs/cordis-primer.md`, `docs/glossary.md`, `docs/defensive-patterns.md`,
`docs/capability-seams.md`, `docs/config-catalog.md`, `docs/cookbook/extension-cookbook.md`,
`docs/subsystems/filesystem.md`, `docs/api-gateway.md`, `docs/postmortem/0001-acp-default-export-drops-inject.md`,
`AGENTS.md`, `packages/AGENTS.md`, `SAFETY.md`, `BENCHMARK.md`, `benchmarks/AGENTS.md`;
`packages/util/package-manifest/src/types.ts`, `packages/boot/app-boot/src/index.ts`,
`packages/boot/app-boot/src/profile.ts`, `apps/cli/src/plugin.ts`, `apps/cli/src/profile-boot.ts`,
`packages/bundle/base/cordis.patch.yml`, `packages/bundle/web-app/cordis.patch.yml`,
`packages/core/tools/src/index.ts`, `packages/core/scope/README.md`,
`packages/core/system-prompt/src/index.ts`, `packages/llm/llm/src/index.ts`,
`packages/settings/settings/README.md`, `packages/settings/settings/src/index.ts`,
`packages/settings/settings-file/src/index.ts`, `packages/client/modules/src/index.ts`,
`packages/typert/protocol/src/index.ts`, `packages/host/plugin-inventory/src/index.ts`,
`packages/fs/tool-fs-search/src/search-core.ts`, `packages/compaction/compaction-basic/src/index.ts`,
`packages/session/session-persistence-jsonl/src/index.ts`;
`vendor/loader/src/index.ts`, `vendor/loader/src/config/{entry,group,tree,utils}.ts`,
`vendor/include/src/index.ts`, `vendor/cordis/src/{fiber,events,service}.ts`,
`vendor/schemastery/src/index.ts`; `scripts/{verify-cordis-config,verify-optional-dependency-imports,verify-runtime-closure,verify-no-bare-dispatcher}.ts`,
`scripts/package-dependency-policy.ts`;
design notes under `.agents/notes/implemented/architecture/` (configuration-source-ownership,
web-capability-seam, timeout-deadline-library, file-context-as-event-gate, capability-seams),
`.agents/notes/implemented/feature/2026-07-08-self-referential-cordis-toolset.md`.

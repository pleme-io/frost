# The Shikumi Four — tear, mado, frost, frostmourne

Operator-facing strategy. Why these four are one ecosystem, what
that means in practice, and what's done vs. pending.

## Frame

Each of these four tools is something an operator interacts with
every day. They overlap on every axis that matters:

| Axis | tear | mado | frost | frostmourne |
|---|---|---|---|---|
| Role | terminal multiplexer | GPU terminal emulator | shell | curated frost preset |
| Persistent process | daemon (long-lived) | window (per-session) | shell (per-tab) | shell (per-tab) |
| Config file | `~/.config/tear/tear.yaml` | `~/.config/mado/mado.yaml` | `~/.config/frost/frost.yaml` | (renders into frost.yaml) |
| Hot-reload | yes (own LiveConfig) | yes (shikumi) | yes (shikumi) | follows frost |
| Env override | partial | yes (shikumi) | yes (shikumi) | follows frost |
| Composes with the others | mado renders tear's panes; frost runs in tear's panes | frost runs in mado's panes; mado attaches to tear | shell for both | preset for frost |

The compounding insight: **if all four use shikumi, an operator
learns one config grammar and gets the same dynamic-reload UX
across the whole stack**. Adding a fifth tool to this family
means "implement Shikumi, get the operator experience for free."

## Status (2026-05-19)

| Tool | shikumi adoption | typed Nix module trio |
|---|---|---|
| **mado** | ✓ `shikumi::ConfigStore<MadoConfig>` | ✓ HM + Darwin |
| **tear** | ✗ hand-rolled `LiveConfig` duplicating shikumi's `ArcSwap`+`notify` | partial HM only |
| **frost** | ✓ `shikumi::ConfigStore<FrostConfig>` (this round) | ✓ HM + NixOS + Darwin (this round) |
| **frostmourne** | partial — tatara-lisp authoring, lifted via `frost-lisp` bridge | ✓ HM |

## What this round shipped

* `crates/frost-config` — typed `FrostConfig` schema (options,
  aliases, env, prompt, history, keybindings, completion,
  frostmourne_preset, reload_debounce_ms) loaded via
  `shikumi::ConfigStore`. 16 unit tests covering defaults,
  round-trip, unknown-field rejection, missing-file fallback,
  malformed-YAML error path.
* `module/home-manager` — full options surface mirroring the
  Rust schema, generates the YAML, supports
  `setAsInteractiveShell`, `manageConfig` opt-out for
  hand-managed configs, `configPath` override.
* `module/nixos` + `module/darwin` — system-side wrappers for
  installing the binary + opt-in `/etc/shells` registration.
* `homeManagerModules.default` / `nixosModules.default` /
  `darwinModules.default` exported from the flake.

## Pending compounding moves

### M1 — Migrate tear-config to shikumi

`tear-config/src/lib.rs` defines its own `LiveConfig` doing
exactly what `shikumi::ConfigStore` does (`ArcSwap` + `notify`).
This drift is the highest-leverage thing to fix because:

* Tear's tests already cover the `LiveConfig` API surface
  → migration risk is bounded.
* Eliminates the only shikumi-shaped duplication in the four.
* Sets up tear as shikumi consumer #3 (after mado + frost).

Concrete steps:
1. Add `shikumi` to `tear-config`'s deps.
2. Replace `LiveConfig` internals with
   `Arc<shikumi::ConfigStore<TearConfig>>`.
3. Keep the `LiveConfig` public API as a thin shim so consumers
   (mado, tear-daemon, the daemon's `SubscribeConfigChange` push
   path) don't need to change.
4. Run the existing `tear-config` test suite + the daemon's
   `SetConfig` / `SubscribeConfigChange` integration tests.
5. Bump tear in nix.

### M2 — Reshape frostmourne to render shikumi YAML

Today `frostmourne` ships tatara-lisp files that the
`frost-lisp` bridge interprets directly into runtime state. The
unified-ecosystem move is for `frost-lisp` to also (or instead)
EMIT a `FrostConfig` YAML — operators get the same lisp
authoring ergonomics, but the runtime path is single (load
YAML → shikumi store → live-reload).

This lets:
* Operators inspect what frostmourne actually configures via
  `cat ~/.config/frost/frost.yaml` (instead of grepping lisp).
* Frostmourne-authored configs and operator-hand-edited
  configs merge cleanly via shikumi's provider chain (lisp →
  YAML → env-override).
* The tatara-lisp catalog stays the authoring surface, becomes
  the macro substrate for any of the four tools (today only
  frostmourne uses it; tomorrow tear/mado could opt in).

### M3 — Lift cross-tool composition primitives into shikumi

Four config files today share three obvious cross-references:

* **Color palette**: tear's status_bar.colors,
  mado's theme, frost's prompt template (when ANSI escapes
  reference a palette).
* **Font ref**: mado's font, frost's prompt template
  (decoration).
* **Hostname / user identity**: every prompt template wants
  these.

Today each config repeats the values. Compounding move: lift a
typed `pleme-shared` (working name) crate with
`PaletteRef::name(&str)`, `FontRef::family(&str)`, etc. The
four configs can then declare `palette_ref: nord-frost` and
shikumi resolves to one source. Theme changes propagate fleet-
wide via one edit.

### M4 — Cross-tool typed messaging via shikumi config schemas

Once schemas are shared, the next level is shared TYPED
MESSAGES. Tear's daemon exposes a `SetConfig(yaml)` RPC; mado
has a `ConfigChange` push. Frost can join the same pattern via
its existing notify hot-reload + an MCP `set_config` tool.
Operators in mado MCP could broadcast a config tweak that lands
on tear + frost simultaneously.

## Why this matters strategically

Three reasons the user articulated as "build together":

1. **Operator UX surface area** — one config grammar across
   four tools means muscle memory transfers. Today an operator
   learns mado's YAML, then re-learns tear's slightly-different
   conventions, then learns frost has none at all. The cost is
   real and recurring.
2. **Test infrastructure compounds** — shikumi's
   `ConfigStore::load_and_watch` is tested once; every consumer
   inherits the verification. Same shape will apply to the L1/L2
   verification ladder we built for mado — if frost (etc.) adopt
   shikumi's testing primitives, regression coverage scales for
   free.
3. **Cross-tool composition becomes possible** — operators
   today re-author the same color palette in three configs.
   With M3, one declaration flows everywhere; the four tools
   become a unified UX rather than a coincidental bundle.

The path is mechanical: shikumi already exists, mado already
adopts it cleanly, this round adds frost, M1-M4 finish the
job over the next two-to-three sessions.

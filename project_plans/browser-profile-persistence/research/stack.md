# Research: STACK — persisting a Chrome user-data-dir across daemon restarts

item_id: 10f6836c-71fc-4db2-8bc8-117d65da6a2d

## 1. chromiumoxide's/CDP's API surface

`chromiumoxide = "0.9"` in `crates/native/Cargo.toml:9`, resolved to **0.9.1** in
`Cargo.lock`.

There is no CDP-level or per-navigation mechanism for this — it's a plain Chrome
launch flag. `BrowserConfigBuilder::user_data_dir(impl AsRef<Path>)`
(`~/.cargo/registry/.../chromiumoxide-0.9.1/src/browser/config.rs:233-236`) just
stores a `PathBuf`. At launch time (`config.rs:393-403`) it's rendered straight
into `--user-data-dir=<path.display()>` via `ArgsBuilder`:

```rust
if let Some(ref user_data) = self.user_data_dir {
    builder.arg(Arg::value("user-data-dir", user_data.display()));
} else {
    // chromiumoxide's own default: $TMPDIR/chromiumoxide-runner (shared,
    // unsuffixed — this repo already overrides it per-process, see below)
    builder.arg(Arg::value("user-data-dir", std::env::temp_dir().join("chromiumoxide-runner").display()));
}
```

No chromiumoxide API exists to change it after `Browser::launch()` — confirms the
requirements doc's conclusion that this is a **launch-time-only** setting; a new
`user_data_dir` param on `stapler_browser_navigate` cannot work without relaunching
the whole Chrome process.

Gotchas:
- **Chrome's `SingletonLock`**: pointing two Chrome launches at the same
  `user-data-dir` concurrently is exactly what `crates/native/src/browser.rs:339-346`'s
  existing comment says chromiumoxide's own default causes ("the second one to
  start finds it locked ... and fails to launch"). A durable, named profile dir
  reused across restarts reintroduces this risk if two daemon instances ever
  launch against the same path (e.g., a stale process didn't exit, or two
  `--daemon` invocations race) — Chrome's `SingletonLock` will make the second
  launch fail or silently steal the profile. Worth a name-collision comment,
  same as the existing per-pid temp dir has.
- **Unclean shutdown**: `SingletonLock` (and CDP debug port lockfiles) aren't
  guaranteed removed on a crash/kill; a stale lock in a durable dir can wedge
  the *next* legitimate launch. Ephemeral per-pid dirs never hit this because
  each dir is used exactly once.
- **Path rendering**: `Arg::value` uses `Path::display()`, i.e. a lossy
  UTF-8 conversion of the OS string — a non-issue for normal paths, but if the
  profile dir is user-suppliable, don't accept arbitrary bytes.
- No chromiumoxide version-specific behavior change around this flag between
  minor 0.9.x releases was found in the vendored source (single code path,
  `config.rs:393-403`); nothing to pin beyond the existing `"0.9"` req.

## 2. Existing config/CLI-flag/env-var plumbing in this repo

There is **no CLI-flag-with-value parser** anywhere in the crate — no `clap`
dependency (confirmed absent from `crates/cli/Cargo.toml` and `Cargo.lock`).
The only CLI argument handling is a boolean presence check:

```rust
// crates/cli/src/main.rs:26
let is_daemon = std::env::args().any(|a| a == "--daemon");
```

Every other piece of opt-in daemon configuration in this codebase is an
**environment variable**, read through `stapler_mcp_core::ports::EnvPort`
(`crates/core/src/ports.rs:105-108`, `var`/`home_dir`), implemented natively by
`NativeEnv` (`crates/native/src/env.rs`, literally `std::env::var(key).ok()`).
Established examples:
- `STAPLER_MCP_HOME` — overrides the whole state-dir root
  (`crates/core/src/paths.rs:7-17`: `base_dir()` checks
  `env.var(ENV_HOME_OVERRIDE)`, falls back to `env.home_dir()` +
  `/.stapler-mcp`).
- `BRAVE_API_KEY` / `BRAVE_API_BASE_URL` — read directly via `std::env::var`
  in `crates/cli/src/main.rs:122-124` (not routed through `EnvPort`, an
  inconsistency worth flagging if this matters for testability, but not this
  feature's problem to fix).

`paths.rs` already derives several subdirectories off `base_dir()`
(`socket_path`, `lock_path`, `log_path`, `cache_dir`, `docs_index_dir`,
`embedding_cache_dir` — `crates/core/src/paths.rs:19-41`), all following the
identical `format!("{}/<name>", base_dir(env))` shape and all unit-tested with
a `MockEnv` (`crates/core/src/paths.rs:43-97`). A new
`browser_profile_dir<E: EnvPort>(env: &E) -> String` function slotting into
this same file, in this same style, is the natural, consistent surface — not a
CLI flag. `NativeBrowser::launch()` (`crates/native/src/browser.rs:337-338`,
currently a zero-arg `pub async fn launch() -> Result<Self, PortError>`) would
take an `Option<String>`/`Option<PathBuf>` param threaded from
`run_daemon()` in `crates/cli/src/main.rs:60-94`, exactly how `paths::base_dir(&env)`
and friends are already computed there and passed to `NativeEmbedder::new(...)`
at line 95.

**Recommendation implied by the codebase's own conventions**: an opt-in env var
(e.g. `STAPLER_MCP_BROWSER_PROFILE`, following the `STAPLER_MCP_*` naming
already used by `STAPLER_MCP_HOME`), not a new CLI flag parser and not a
per-call MCP tool parameter (which also isn't architecturally possible per the
requirements doc's ground truth).

## 3. Stable "app config dir" path resolution

The repo does **not** use the `directories` or `dirs` crate for this and has
its own hand-rolled equivalent: `EnvPort::home_dir()` →
`std::env::var("HOME").ok()` in `crates/native/src/env.rs:10-12`, consumed by
`paths::base_dir()`. This is deliberately part of the ports-and-adapters seam
(`EnvPort` is mockable in tests — see `crates/core/src/paths.rs`'s `MockEnv`),
which a `dirs`/`directories` crate call (`dirs::data_dir()` etc., not
injectable) would bypass.

Notably, **`dirs = "6.0.0"` is already present in `Cargo.lock`** — but only as
a *transitive* dependency (of `hf-hub`, itself pulled in by `fastembed`), not a
direct dependency of any crate in this workspace:

```
$ awk '/^\[\[package\]\]/{p=""} /^name = /{p=$0} /dirs/{if(p!="")print p}' Cargo.lock | sort -u
name = "dirs"
name = "dirs-sys"
name = "hf-hub"
```

Conclusion: don't add `dirs`/`directories` as a direct dependency for this
feature — it would duplicate the existing `EnvPort::home_dir()` seam, break
the injectable/testable pattern every other path in `paths.rs` follows, and
isn't needed since `HOME`-relative resolution is already the established
(Linux/macOS-only — `NativeEnv::home_dir()` reads `HOME` only, no `%USERPROFILE%`
fallback, so Windows was never supported by this path-resolution layer
regardless of this feature) convention. Extend `paths.rs` with a
`browser_profile_dir()` function the same way `embedding_cache_dir()` was
added, instead.

## 4. Dependency delta

**None required.** Everything needed is already a workspace dependency:
- `chromiumoxide = "0.9"` (native crate) — already exposes the exact API
  (`BrowserConfigBuilder::user_data_dir`) needed; no version bump.
- `EnvPort`/`NativeEnv` (core + native crates) — already the established seam
  for reading an opt-in env var and resolving `$HOME`.
- `paths.rs` (core crate) — already the established location for deriving
  subdirectories under the daemon's state root; extend, don't replace.
- No `clap`, no `dirs`/`directories`, no new crate needed. The only workspace
  change is application code: a new `paths::browser_profile_dir()` (or
  equivalent), a new `Option<PathBuf>` parameter threaded from `run_daemon()`
  through `NativeBrowser::launch()` to `BrowserConfig::builder().user_data_dir(...)`,
  and — given the `SingletonLock` risk above — probably a `std::fs::create_dir_all`
  call plus a comment analogous to the one already at
  `crates/native/src/browser.rs:339-352` explaining the collision hazard for a
  *named, reused* dir (as opposed to the current per-pid ephemeral one).

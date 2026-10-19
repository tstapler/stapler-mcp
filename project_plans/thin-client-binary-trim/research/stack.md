# Stack research: thin-client binary trim

Environment: cargo 1.98.1, resolver 2, rmcp 2.2.0, tokio 1.52.3. All results below were run in this worktree or in a scratch crate (not in the repo).

## (a) Per-package profile override keys

Scratch crate with `[profile.release.package.dep]` containing one key at a time, `cargo build --release --offline`:

| Key | Result |
|---|---|
| `opt-level`, `codegen-units`, `debug`, `incremental`, `debug-assertions`, `overflow-checks` | accepted |
| `strip = true` | accepted by the parser (VERIFIED: "Finished"), but see caveat |
| `lto = true` | REJECTED: `lto may not be specified in a package profile` |
| `panic = "abort"` | REJECTED: `panic may not be specified in a package profile` |
| `rpath = true` | rejected (parse error) |

Caveats:
- `strip` is accepted per-package but it is a link-time action on the final binary, so a per-package value on a library or on a non-final crate does nothing useful. INFERRED, not tested for binary size. Keep `strip` in `[profile.release]` or the named profile.
- `lto`, `panic` are profile-wide. The repo's root `Cargo.toml` already follows this: per-package `opt-level`/`codegen-units` for `stapler-mcp` plus a `[profile.release-thin]` that sets `lto = "fat"`. That design is correct.
- Per-package override on `stapler-mcp` (the thin bin crate) only affects that crate's own code, not rmcp/tokio/serde. To shrink dependencies use the `release-thin` profile (applies `opt-level = "z"` to everything, including deps) or add a `[profile.release-thin.package."*"]` override. The current `[profile.release.package."stapler-mcp"]` is near-negligible for size; the real gains come from `release-thin`.
- `panic = "abort"` in `release-thin` is allowed (profile-level) but is risky if any code relies on `catch_unwind`/tokio task panic isolation. Needs an owner decision; not tested here.
- Gap: `release-thin` profile binary size not measured in this research.

## (b) Feature unification, resolver 2, shared crates/native

- Resolver 2 does NOT unify features across targets/build-deps/dev-deps, but it DOES unify features for all packages selected in one invocation. `cargo build --workspace` (or `-p a -p b`) selects both thin client and daemon, so `stapler-mcp-native/heavy` (requested by the daemon via default features) is enabled for the single compiled `stapler-mcp-native`, and the thin client links the heavy build.
- Evidence: `cargo tree -e normal -p stapler-mcp -p stapler-mcp-daemon | grep -cE "chromiumoxide|fastembed|reqwest"` = 10 hits; `cargo tree -e normal --workspace | grep -cE "chromiumoxide|fastembed"` = 7. Versus `cargo tree -e normal -p stapler-mcp` alone: zero hits for chromiumoxide/fastembed/reqwest/hyper (the only grep match was `dtoa-short`, a false positive on "ort ").
- Current thin Cargo.toml (`crates/thin-client/Cargo.toml`) already sets `stapler-mcp-native = { default-features = false }`, and `cargo build -p stapler-mcp --offline` compiled successfully.
- Consequence: the thin binary must always be built with `-p stapler-mcp` (alone), never as part of `--workspace`, or the heavy deps are unified in. Note: the repo's `default-members` includes both, so a bare `cargo build` also unifies. Mitigations: (1) document and script `cargo build -p stapler-mcp --profile release-thin` (and dist/CI/install scripts); (2) add a CI check that fails if `cargo tree -e normal -p stapler-mcp` contains heavy crates; (3) optionally set `default-members` to exclude the thin client or the daemon, or use `resolver = "2"` plus `cargo hakari`-style workspace-hack is NOT needed. Also `cargo test --workspace` will unify, which is fine for tests but does not prove the thin-only feature set compiles; compile-check thin alone with `cargo check -p stapler-mcp`.
- Also: `crates/wasm` is a member but not in default-members; if it depends on core only, unaffected.

## (c) tokio / rmcp feature minimization

Thin tree (`cargo tree -e features -p stapler-mcp -i tokio`) enables tokio features: bytes, fs, io-std, io-util, libc, macros, mio, net, process, rt, signal-hook-registry, sync, time, tokio-macros. No `rt-multi-thread` (it appears only in the workspace-wide tree, from the daemon/other deps), which is correct for a current_thread thin client.
- thin-client's own tokio request: rt, macros, time, net, io-util, sync. `fs` and `process` come from `stapler-mcp-native`'s tokio dependency (`crates/native/Cargo.toml` line 13); `io-std` comes from rmcp `transport-io`; `signal-hook-registry`/`libc` come from `process`.
- Opportunity: make `fs` and `process` in native conditional on `heavy` only if non-heavy native code does not use them (UNVERIFIED which native modules use `tokio::fs`/`tokio::process`; check before moving). Expected saving is small (tokio sub-features are mostly thin).
- tokio's own `default` feature is empty, so "tokio feature default" in the tree is harmless.
- rmcp: thin-client sets features `server, macros, transport-io, schemars` but does not set `default-features = false`, so rmcp's defaults (`base64`, `macros`, `server`) are also on (rmcp 2.2.0 Cargo.toml: `default = ["base64","macros","server"]`). `server` already pulls `transport-async-rw`, schemars, pastey. Resulting rmcp features in tree: default, macros, schemars, server, transport-async-rw, transport-io. So adding `default-features = false` would only drop `base64`; it is cheap to try and verify with `cargo tree`. No reqwest/http/auth features of rmcp are active.
- Total unique normal deps of thin client: 207 (`cargo tree -e normal -p stapler-mcp --prefix none | sort -u | wc -l`). Duplicates exist in `phf_generator` (build-deps via dom_smoothie in core); core's `dom_smoothie` is a thin-client dep through `stapler-mcp-core`. If core's HTML-extraction deps are not needed by the thin client, that is the next candidate (not investigated).

## (d) Is `cargo tree -e normal -p stapler-mcp` the right check?

Yes, with adjustments:
- It correctly shows the feature-resolved normal dependency graph for that package alone (VERIFIED: no heavy crates). `-e normal` excludes dev and build deps, which is what ships in the binary.
- Match on exact crate names, not substrings (`ort ` matched `dtoa-short`). Use e.g. `cargo tree -e normal -p stapler-mcp --prefix none | awk '{print $1}' | grep -xE 'chromiumoxide|fastembed|reqwest|hyper|ort|tokio-rustls|rustls'`.
- `-e normal` still shows build-time proc-macro crates (they are in the graph but not linked). Fine for a heavy-dep denylist.
- It does not prove the final binary is clean; add a size/symbol check on the built `release-thin` artifact (e.g. `cargo bloat` or `strings`) as a second gate. UNVERIFIED here.
- It is only valid when invoked with `-p stapler-mcp` alone; running it with `--workspace` reintroduces unified features (see b).

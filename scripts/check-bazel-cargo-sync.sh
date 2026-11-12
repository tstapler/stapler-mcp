#!/usr/bin/env bash
# Fails when the Bazel build and the Cargo manifests have drifted apart. Cargo.toml/Cargo.lock stay the source of
# truth; Bazel derives from them, so every check reads both sides. Each check fails closed (a missing file is a FAIL).
#   check-bazel-cargo-sync.sh [ROOT]   run all checks (ROOT defaults to the current directory)
#   check-bazel-cargo-sync.sh --selftest
# Static checks (python3 + tomllib, no Bazel):
#   toolchain  rust-toolchain.toml channel == RUST_VERSION in MODULE.bazel, and .bazelversion exists
#   lock       packages in Cargo.lock == crates in cargo-bazel-lock.json (name + version)
#   members    every workspace member has a BUILD.bazel and is listed in MODULE.bazel `manifests`
#   features   every [features] name of a member appears in its BUILD.bazel (a new feature must be wired up)
#   profile    [profile.release] lto == the lto of `build:dist` in .bazelrc
# Bazel checks (SM_SYNC_BAZEL=0 skips):
#   lockfile   MODULE.bazel.lock is current (`--lockfile_mode=error`)
#   repin      cargo-bazel-lock.json is not stale: re-pinning in place must leave the file unchanged
#   tests      every tests/*.rs of a member has a //crates/<c>:<stem>_test target
set -euo pipefail

die() { echo "FAIL: $*"; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "required tool '$1' not found on PATH (the check cannot run, so it does not pass)"; }

static_checks() {
  python3 -I - "$1" <<'PY'
import json, re, sys, tomllib
from pathlib import Path

root = Path(sys.argv[1])
bad = []

def fail(msg): bad.append(msg)

def read(p):
    try: return (root / p).read_text()
    except OSError: fail(f"missing file {p}"); return ""

def toml(p):
    try: return tomllib.loads(read(p))
    except tomllib.TOMLDecodeError as e: fail(f"{p}: {e}"); return {}

cargo = toml("Cargo.toml")
module = read("MODULE.bazel")
rc = read(".bazelrc")

# toolchain
chan = toml("rust-toolchain.toml").get("toolchain", {}).get("channel", "")
m = re.search(r'^RUST_VERSION *= *"([^"]+)"', module, re.M)
if not m: fail('MODULE.bazel has no RUST_VERSION = "x.y.z"')
elif chan != m.group(1): fail(f"rust-toolchain.toml channel {chan!r} != MODULE.bazel RUST_VERSION {m.group(1)!r}")
if not re.fullmatch(r"\d+\.\d+\.\d+", chan): fail(f"rust-toolchain.toml channel {chan!r} is not an explicit x.y.z version")
if not (root / ".bazelversion").is_file(): fail(".bazelversion missing")

# lock: Cargo.lock packages == crate_universe lock
want = {f'{p["name"]} {p["version"]}' for p in toml("Cargo.lock").get("package", [])}
try: have = set(json.loads(read("cargo-bazel-lock.json")).get("crates", {}))
except json.JSONDecodeError as e: fail(f"cargo-bazel-lock.json: {e}"); have = set()
if not want: fail("Cargo.lock lists no packages")
for k in sorted(want - have): fail(f"Cargo.lock has {k} but cargo-bazel-lock.json does not (repin: CARGO_BAZEL_REPIN=1 scripts/bazel.sh query @crates//:all)")
for k in sorted(have - want): fail(f"cargo-bazel-lock.json has {k} but Cargo.lock does not (repin: CARGO_BAZEL_REPIN=1 scripts/bazel.sh query @crates//:all)")

# members and features
members = cargo.get("workspace", {}).get("members", [])
if not members: fail("Cargo.toml has no [workspace] members")
for d in members:
    b = read(f"{d}/BUILD.bazel")
    if f'"//{d}:Cargo.toml"' not in module: fail(f'MODULE.bazel crate.from_cargo manifests lacks "//{d}:Cargo.toml"')
    for f in toml(f"{d}/Cargo.toml").get("features", {}):
        if f != "default" and f'"{f}"' not in b: fail(f'{d}: Cargo feature "{f}" is not wired in {d}/BUILD.bazel (crate_features / a variant)')
for p in sorted((root / "crates").glob("*/Cargo.toml")) if (root / "crates").is_dir() else []:
    d = p.parent.relative_to(root).as_posix()
    if d not in members: fail(f"{d} has a Cargo.toml but is not in [workspace] members")

# profile
lto = cargo.get("profile", {}).get("release", {}).get("lto")
rl = re.search(r"^build:dist +--@rules_rust//rust/settings:lto=(\S+)", rc, re.M)
if lto is None or not rl or rl.group(1) != lto: fail(f"[profile.release] lto = {lto!r} vs .bazelrc build:dist lto = {rl.group(1) if rl else None!r}")

for b in bad: print("FAIL:", b)
sys.exit(1 if bad else 0)
PY
}

bazel_checks() {
  local root=$1 bz=${SM_BAZEL:-$1/scripts/bazel.sh} rc=0 targets crate f stem before after
  [ -x "$bz" ] || command -v "$bz" >/dev/null 2>&1 || { echo "FAIL: $bz not found (SM_SYNC_BAZEL=0 skips the Bazel checks)"; return 1; }
  (cd "$root" && "$bz" build --nobuild --lockfile_mode=error //... >/dev/null 2>&1) \
    || { echo "FAIL: MODULE.bazel.lock is out of date (run: scripts/bazel.sh build --nobuild --lockfile_mode=update //...) or bazel failed"; rc=1; }
  # A Cargo.toml dependency change leaves cargo-bazel-lock.json stale and Bazel fails later with "cannot find crate":
  # re-pin in place and fail if the committed lock changes.
  before=$(sha256sum "$root/cargo-bazel-lock.json" 2>/dev/null | cut -d' ' -f1)
  (cd "$root" && CARGO_BAZEL_REPIN=1 "$bz" build --nobuild //crates/core:core >/dev/null 2>&1) || true
  after=$(sha256sum "$root/cargo-bazel-lock.json" 2>/dev/null | cut -d' ' -f1)
  if [ "$before" != "$after" ]; then
    echo "FAIL: cargo-bazel-lock.json was stale: a Cargo.toml dependency changed (it is now re-pinned in the working tree: review and commit it)"; rc=1
  fi
  targets=$(cd "$root" && "$bz" query 'kind("rust_test", //crates/...)' 2>/dev/null) || { echo "FAIL: bazel query failed"; return 1; }
  for f in "$root"/crates/*/tests/*.rs; do
    [ -e "$f" ] || continue
    crate=${f#"$root"/crates/}; crate=${crate%%/*}; stem=${f##*/}; stem=${stem%.rs}
    printf '%s\n' "$targets" | grep -qx "//crates/$crate:${stem}_test" || { echo "FAIL: crates/$crate/tests/$stem.rs has no Bazel target //crates/$crate:${stem}_test"; rc=1; }
  done
  return "$rc"
}

run() {
  local root=${1:-.} rc=0
  need python3
  root=$(cd "$root" && pwd)
  static_checks "$root" || rc=1
  if [ "${SM_SYNC_BAZEL:-1}" != 0 ]; then bazel_checks "$root" || rc=1; fi
  [ "$rc" -eq 0 ] && echo "ok: Cargo and Bazel agree (toolchain, lock, members, features, profile$([ "${SM_SYNC_BAZEL:-1}" != 0 ] && echo ', bazel'))"
  return "$rc"
}

# A minimal synthetic repo that passes, then one mutation per static check that must fail.
make_good() {
  local d=$1
  mkdir -p "$d/crates/a"
  printf '[workspace]\nmembers = ["crates/a"]\n[profile.release]\nlto = "fat"\n' > "$d/Cargo.toml"
  printf '[package]\nname = "a"\nversion = "0.1.0"\n[features]\ndefault = []\nheavy = []\n' > "$d/crates/a/Cargo.toml"
  printf 'RUST_VERSION = "1.98.1"\nmanifests = ["//crates/a:Cargo.toml"]\n' > "$d/MODULE.bazel"
  printf '[toolchain]\nchannel = "1.98.1"\n' > "$d/rust-toolchain.toml"
  echo 9.3.2 > "$d/.bazelversion"
  printf 'build:dist --@rules_rust//rust/settings:lto=fat\n' > "$d/.bazelrc"
  printf '[[package]]\nname = "a"\nversion = "0.1.0"\n[[package]]\nname = "serde"\nversion = "1.0.0"\nsource = "registry+x"\n' > "$d/Cargo.lock"
  echo '{"crates":{"a 0.1.0":{},"serde 1.0.0":{}}}' > "$d/cargo-bazel-lock.json"
  echo 'crate_features = ["heavy"]' > "$d/crates/a/BUILD.bazel"
}

selftest() {
  need python3
  local n=0 bad=0 d out rc
  t=$(mktemp -d)  # global: the EXIT trap runs after this function's locals are gone
  trap 'rm -rf "$t"' EXIT INT TERM
  export SM_SYNC_BAZEL=0
  expect() {  # want(pass|fail) name pattern mutation-command...
    local want=$1 name=$2 pat=$3; shift 3
    d=$t/r$n; make_good "$d"; (cd "$d" && eval "$*")
    rc=0; out=$(run "$d" 2>&1) || rc=$?
    if { [ "$want" = pass ] && [ $rc -eq 0 ]; } || { [ "$want" = fail ] && [ $rc -eq 1 ]; }; then
      if echo "$out" | grep -q -- "$pat"; then echo "ok: $name"; else echo "SELFTEST BAD: $name (output lacks '$pat'): $out"; bad=$((bad+1)); fi
    else echo "SELFTEST BAD: $name (rc=$rc): $out"; bad=$((bad+1)); fi
    n=$((n+1))
  }
  expect pass "a consistent repo passes" "ok: Cargo and Bazel agree" true
  expect fail "toolchain version drift" "rust-toolchain.toml channel" "sed -i 's/1.98.1/1.99.0/' MODULE.bazel"
  expect fail "a floating toolchain channel" "not an explicit x.y.z" "printf '[toolchain]\nchannel = \"stable\"\n' > rust-toolchain.toml; sed -i 's/1.98.1/stable/' MODULE.bazel"
  expect fail "a missing .bazelversion" ".bazelversion missing" "rm .bazelversion"
  expect fail "a package missing from the crate lock" "Cargo.lock has serde 1.0.0" "echo '{\"crates\":{\"a 0.1.0\":{}}}' > cargo-bazel-lock.json"
  expect fail "a stale package in the crate lock" "cargo-bazel-lock.json has old 0.9.0" "echo '{\"crates\":{\"a 0.1.0\":{},\"serde 1.0.0\":{},\"old 0.9.0\":{}}}' > cargo-bazel-lock.json"
  expect fail "a version bump in Cargo.lock alone" "Cargo.lock has serde 1.0.1" "sed -i 's/1.0.0/1.0.1/' Cargo.lock"
  expect fail "a member without BUILD.bazel" "missing file crates/a/BUILD.bazel" "rm crates/a/BUILD.bazel"
  expect fail "a member missing from MODULE.bazel manifests" 'lacks "//crates/a:Cargo.toml"' "echo 'RUST_VERSION = \"1.98.1\"' > MODULE.bazel"
  expect fail "a crate not in the workspace" "crates/b has a Cargo.toml but is not in" "mkdir crates/b; echo '[package]' > crates/b/Cargo.toml"
  expect fail "a feature with no wiring" 'Cargo feature "gate" is not wired' "sed -i 's/heavy = \\[\\]/heavy = []\\ngate = []/' crates/a/Cargo.toml"
  expect fail "release lto drift" "lto = 'fat' vs .bazelrc build:dist lto = None" "sed -i '/lto=fat/d' .bazelrc"
  expect fail "a missing Cargo.lock" "missing file Cargo.lock" "rm Cargo.lock"
  echo "selftest: $((n - bad)) ok, $bad bad"; [ "$bad" -eq 0 ]
}

case "${1:-}" in
  --selftest) selftest ;;
  -h | --help) sed -n '2,17p' "$0" ;;
  *) run "${1:-.}" ;;
esac

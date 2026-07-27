#!/usr/bin/env bash
# Controlled experiment: which pnpm invocations rewrite a project's
# pnpm-lock.yaml, and which setting suppresses it.
#
# Every variant runs in a fresh synthetic project in the same job, so the OS,
# node version, pnpm binary, store state and network are identical across them.
set -uo pipefail

ROOT="${RUNNER_TEMP:-/tmp}/pnpm-repro"
rm -rf "$ROOT"
mkdir -p "$ROOT"

PNPM_BIN="$(command -v pnpm)"
echo "pnpm binary : $PNPM_BIN"
echo "pnpm version: $(pnpm --version 2>/dev/null)"
echo "node version: $(node --version)"
echo "PNPM_HOME   : ${PNPM_HOME:-<unset>}"
echo "uname       : $(uname -sm)"
echo

# $1 dir, $2 pnpm-workspace.yaml contents (empty string = omit the file),
# $3 packageManager value (empty = omit the field)
mkproj() {
  local dir="$1" ws="$2" pm="$3"
  mkdir -p "$dir"
  if [ -n "$pm" ]; then
    printf '{\n  "name": "repro",\n  "private": true,\n  "packageManager": "%s"\n}\n' "$pm" > "$dir/package.json"
  else
    printf '{\n  "name": "repro",\n  "private": true\n}\n' > "$dir/package.json"
  fi
  printf "lockfileVersion: '9.0'\n\nsettings:\n  autoInstallPeers: true\n  excludeLinksFromLockfile: false\n\nimporters:\n\n  .: {}\n" > "$dir/pnpm-lock.yaml"
  [ -n "$ws" ] && printf '%s\n' "$ws" > "$dir/pnpm-workspace.yaml"
  return 0
}

# $1 label, $2 dir, rest: command
probe() {
  local label="$1" dir="$2"; shift 2
  local before after size_before size_after
  before=$(sha1sum "$dir/pnpm-lock.yaml" | cut -d' ' -f1)
  size_before=$(wc -c < "$dir/pnpm-lock.yaml")
  ( cd "$dir" && "$@" ) >"$dir/.out" 2>&1
  local rc=$?
  after=$(sha1sum "$dir/pnpm-lock.yaml" | cut -d' ' -f1)
  size_after=$(wc -c < "$dir/pnpm-lock.yaml")
  if [ "$before" = "$after" ]; then
    printf '%-52s UNCHANGED   (rc=%s, %s bytes)\n' "$label" "$rc" "$size_after"
  else
    printf '%-52s REWRITTEN   (rc=%s, %s -> %s bytes)\n' "$label" "$rc" "$size_before" "$size_after"
  fi
}

echo "================ which command triggers the rewrite ================"
for cmd in "store path --silent" "--version" "root" "config get storeDir" "why pnpm"; do
  d="$ROOT/cmd-$(echo "$cmd" | tr ' /-' '___')"
  mkproj "$d" "packages: []" "pnpm@11.15.1"
  # shellcheck disable=SC2086
  probe "pnpm $cmd" "$d" pnpm $cmd
done

echo
echo "================ settings, all with 'pnpm store path' ================"
mkproj "$ROOT/s-baseline" "packages: []" "pnpm@11.15.1"
probe "baseline (no relevant setting)" "$ROOT/s-baseline" pnpm store path --silent

mkproj "$ROOT/s-mpmv-false" "$(printf 'managePackageManagerVersions: false\npackages: []')" "pnpm@11.15.1"
probe "managePackageManagerVersions: false" "$ROOT/s-mpmv-false" pnpm store path --silent

mkproj "$ROOT/s-mpmv-true" "$(printf 'managePackageManagerVersions: true\npackages: []')" "pnpm@11.15.1"
probe "managePackageManagerVersions: true" "$ROOT/s-mpmv-true" pnpm store path --silent

mkproj "$ROOT/s-pmof-ignore" "$(printf 'pmOnFail: ignore\npackages: []')" "pnpm@11.15.1"
probe "pmOnFail: ignore" "$ROOT/s-pmof-ignore" pnpm store path --silent

mkproj "$ROOT/s-pmof-error" "$(printf 'pmOnFail: error\npackages: []')" "pnpm@11.15.1"
probe "pmOnFail: error" "$ROOT/s-pmof-error" pnpm store path --silent

mkproj "$ROOT/s-pmof-warn" "$(printf 'pmOnFail: warn\npackages: []')" "pnpm@11.15.1"
probe "pmOnFail: warn" "$ROOT/s-pmof-warn" pnpm store path --silent

mkproj "$ROOT/s-bogus" "$(printf 'zzzTotallyBogusSetting: true\npackages: []')" "pnpm@11.15.1"
probe "zzzTotallyBogusSetting: true (control)" "$ROOT/s-bogus" pnpm store path --silent

echo
echo "================ env var / no-workspace / no-packageManager ================"
mkproj "$ROOT/e-env" "packages: []" "pnpm@11.15.1"
probe "env pnpm_config_pm_on_fail=ignore" "$ROOT/e-env" env pnpm_config_pm_on_fail=ignore pnpm store path --silent

mkproj "$ROOT/e-env-legacy" "packages: []" "pnpm@11.15.1"
probe "env pnpm_config_manage_package_manager_versions=false" "$ROOT/e-env-legacy" \
  env pnpm_config_manage_package_manager_versions=false pnpm store path --silent

mkproj "$ROOT/e-no-ws" "" "pnpm@11.15.1"
probe "no pnpm-workspace.yaml at all" "$ROOT/e-no-ws" pnpm store path --silent

mkproj "$ROOT/e-no-pm" "packages: []" ""
probe "no packageManager field" "$ROOT/e-no-pm" pnpm store path --silent

mkproj "$ROOT/e-mismatch" "packages: []" "pnpm@11.15.0"
probe "packageManager mismatch (11.15.0)" "$ROOT/e-mismatch" pnpm store path --silent

echo
echo "================ what exactly gets written (baseline) ================"
d="$ROOT/dump"
mkproj "$d" "packages: []" "pnpm@11.15.1"
cp "$d/pnpm-lock.yaml" "$d/lock.orig"
( cd "$d" && pnpm store path --silent >/dev/null 2>&1 )
if ! cmp -s "$d/lock.orig" "$d/pnpm-lock.yaml"; then
  echo "--- diff (orig -> after) ---"
  diff -u "$d/lock.orig" "$d/pnpm-lock.yaml" | head -60
  echo "--- total lines now: $(wc -l < "$d/pnpm-lock.yaml") ---"
else
  echo "(baseline not rewritten in this environment)"
fi

echo
echo "================ debug log of the triggering command ================"
d="$ROOT/dbg"
mkproj "$d" "packages: []" "pnpm@11.15.1"
( cd "$d" && pnpm store path --loglevel=debug 2>&1 | head -40 )

echo
echo "================ deprecation warning for the legacy key? ================"
d="$ROOT/warn"
mkproj "$d" "$(printf 'managePackageManagerVersions: false\npackages: []')" "pnpm@11.15.1"
( cd "$d" && pnpm store path --loglevel=debug 2>&1 | grep -iE "deprecat|unknown|unrecogni|managePackage|pmOnFail" | head -10 || echo "(no matching output)" )

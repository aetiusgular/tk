#!/bin/bash
# Builds and signs "MonoCode Computer Use.app" (wave 3, lane C, item C1).
#
#   CUA_DRIVER_BIN=/path/to/verified/cua-driver \
#   MONOCODE_HELPER_IDENTITY="MonoCode Dev" \
#   macos/computer-use-helper/bundle.sh <output folder>
#
# - Runs on the Mac only. It downloads nothing: CUA_DRIVER_BIN is a cua-driver
#   you fetched and checked yourself (the release's SHA256SUMS and its
#   Sigstore bundle with cosign; note W, section 3.2).
# - A release build signs with the identity you name, never one it creates
#   (plan, section 3, call 4), and builds in that certificate's SHA-1 as the
#   only caller the helper answers. The same identity must sign MonoCode, and
#   macOS keeps the helper's grants across rebuilds only while it stays the
#   same.
# - MONOCODE_APP_ID is the bundle id of the MonoCode the helper answers:
#   com.monocode.desktop by default, com.monocode.desktop.dev with
#   HELPER_DEBUG=1 (the user's decision 1).
# - HELPER_DEBUG=1 builds a debug helper, bundle id
#   com.monocode.computer-use-helper.dev, signed ad hoc, with no signing
#   requirement built in: it checks its caller by pid only and says so in its
#   log. No identity and no keychain lookup. For a dev build of MonoCode.
#   Never ship it. An ad hoc signature changes with every build, so macOS may
#   ask for its grants again after a rebuild.
# - DRY_RUN=1 prints the steps and touches nothing, on any system. With it,
#   DRY_RUN_IDENTITIES=<file> stands in for the keychain's identity list.
set -euo pipefail

out="${1:-}"
if [ -z "$out" ]; then
  echo "usage: CUA_DRIVER_BIN=... MONOCODE_HELPER_IDENTITY=... $0 <output folder>" >&2
  exit 2
fi
debug=0
[ "${HELPER_DEBUG:-}" = 1 ] && debug=1
identity="${MONOCODE_HELPER_IDENTITY:-}"
driver="${CUA_DRIVER_BIN:-}"
if [ -z "$driver" ]; then
  echo "Set CUA_DRIVER_BIN." >&2
  exit 2
fi
if [ "$debug" = 1 ]; then
  helper_id=com.monocode.computer-use-helper.dev
  app_id="${MONOCODE_APP_ID:-com.monocode.desktop.dev}"
else
  helper_id=com.monocode.computer-use-helper
  app_id="${MONOCODE_APP_ID:-com.monocode.desktop}"
  if [ -z "$identity" ]; then
    echo "Set MONOCODE_HELPER_IDENTITY (or HELPER_DEBUG=1 for a debug helper)." >&2
    exit 2
  fi
fi
# The id goes inside a quoted code requirement: plain reverse-DNS only.
if ! [[ "$app_id" =~ ^[A-Za-z0-9_-]+(\.[A-Za-z0-9_-]+)+$ ]]; then
  echo "MONOCODE_APP_ID must be a plain bundle id." >&2
  exit 2
fi

here="$(cd "$(dirname "$0")" && pwd)"
app="$out/MonoCode Computer Use.app"

run() {
  if [ "${DRY_RUN:-}" = 1 ]; then
    printf 'would run:'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

if [ "${DRY_RUN:-}" != 1 ]; then
  [ "$(uname -s)" = Darwin ] || { echo "Build the helper on the Mac." >&2; exit 2; }
  [ -f "$driver" ] && [ -x "$driver" ] || { echo "No cua-driver at $driver" >&2; exit 2; }
fi

# The signing identity, found by its exact name. Not `security
# find-certificate -c`: it matches any certificate whose common name merely
# contains the text, and takes the first (Apple's SecurityTool,
# keychain_find.c, `CFStringFind` on the common name). `find-identity -v -p
# codesigning` lists each valid code-signing identity as `N) <SHA-1>
# "<name>"` (security(1); identity_find.c, printIdentity), so the name is
# compared whole, and zero or several matches are refused. The certificate is
# then pinned, and signed with, by its SHA-1: anyone can make a certificate
# with your identity's name (security review, finding 1), and codesign takes
# 40 hex digits as a certificate's SHA-1 (codesign(1), SIGNING IDENTITIES).
if [ "$debug" = 1 ]; then
  sign=-
  sha1=
else
  run security find-identity -v -p codesigning
  if [ "${DRY_RUN:-}" = 1 ]; then
    if [ -n "${DRY_RUN_IDENTITIES:-}" ]; then
      listing="$(cat "$DRY_RUN_IDENTITIES")"
    else
      listing="  1) 0000000000000000000000000000000000000000 \"$identity\""
    fi
  else
    listing="$(security find-identity -v -p codesigning)"
  fi
  matches="$(printf '%s\n' "$listing" | awk -v want="$identity" '
    # No regex intervals: older BSD awk on macOS lacks them.
    match($0, /^ *[0-9]+\) [0-9A-Fa-f]+ "/) && length($2) == 40 {
      sha = $2
      name = substr($0, RLENGTH + 1)
      if (substr(name, length(name)) != "\"") next
      name = substr(name, 1, length(name) - 1)
      if (name == want) print toupper(sha)
    }' | sort -u)"
  count="$(printf '%s' "$matches" | grep -c . || true)"
  if [ "$count" -eq 0 ]; then
    echo "No signing identity named exactly \"$identity\" in your keychain." >&2
    exit 2
  elif [ "$count" -ne 1 ]; then
    echo "$count signing identities named \"$identity\". Keep one, or rename the others." >&2
    exit 2
  fi
  sha1="$matches"
  sign="$sha1"
fi

if [ "$debug" = 1 ]; then
  profile=debug
  run env MONOCODE_APP_ID="$app_id" \
    cargo build --manifest-path "$here/Cargo.toml" --target aarch64-apple-darwin
else
  profile=release
  run env MONOCODE_APP_ID="$app_id" MONOCODE_HELPER_CERT_SHA1="$sha1" \
    cargo build --release --manifest-path "$here/Cargo.toml" --target aarch64-apple-darwin
fi
run rm -rf "$app"
run mkdir -p "$app/Contents/MacOS"
run cp "$here/bundle/Info.plist" "$app/Contents/Info.plist"
run /usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $helper_id" "$app/Contents/Info.plist"
run cp "$here/target/aarch64-apple-darwin/$profile/monocode-computer-use-helper" "$app/Contents/MacOS/"
run cp "$driver" "$app/Contents/MacOS/cua-driver"
# Inside out: the driver first, then the bundle, both with the hardened runtime.
run codesign --force --options runtime --timestamp=none --sign "$sign" "$app/Contents/MacOS/cua-driver"
run codesign --force --options runtime --timestamp=none --sign "$sign" \
  --identifier "$helper_id" "$app"
run codesign --verify --strict --verbose=2 "$app"
echo "Built $app ($helper_id, answers $app_id)"
echo "Place it at MonoCode.app/Contents/Helpers/, or set MONOCODE_COMPUTER_USE_HELPER to it for a dev build."

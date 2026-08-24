#!/usr/bin/env bash
#
# Exercises docs/public/install end to end.
#
#   make test-install          # locally
#   .github/scripts/test-install.sh
#
# Run by .github/workflows/install.yml on Ubuntu and macOS. The point is not
# coverage for its own sake: the script hardcodes the release asset naming that
# release.yml chooses, so without this job that pairing can drift and break
# every new user while the Rust test suite stays green. It already caught one
# such mistake before the script shipped.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INSTALLER="$ROOT/docs/public/install"

# Pinned deliberately. Testing against "latest" makes this job fail while a
# release is mid-publish, for reasons unconnected to the script.
#
# :warning: **This must name a release that still has assets attached**, not
# merely a tag. It was 0.18.0 until 2026-08-24, when the repository was replaced
# and only the newest release was carried over; the tag survived, the release
# did not, and four of these tests failed with `no such release: v0.18.0`. If
# old releases are pruned again, move this to the oldest one that remains.
PINNED="0.20.6"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0

ok() {
	printf '  ok    %s\n' "$1"
	pass=$((pass + 1))
}

bad() {
	printf '  FAIL  %s\n' "$1" >&2
	[ $# -gt 1 ] && printf '        %s\n' "$2" >&2
	fail=$((fail + 1))
}

# Runs the installer, capturing output and status without tripping set -e.
run() {
	set +e
	OUT="$(sh "$INSTALLER" "$@" 2>&1)"
	STATUS=$?
	set -e
}

# ------------------------------------------------------------------ happy path

printf '\ninstall\n'

run --version "$PINNED" --to "$WORK/bin"
if [ "$STATUS" -eq 0 ] && [ -x "$WORK/bin/videre" ]; then
	ok "installs a pinned version"
else
	bad "installs a pinned version" "$OUT"
fi

if "$WORK/bin/videre" --version 2>/dev/null | grep -q "$PINNED"; then
	ok "the installed binary reports $PINNED"
else
	bad "the installed binary reports $PINNED" "$("$WORK/bin/videre" --version 2>&1 || true)"
fi

run --version "v$PINNED" --to "$WORK/vpref"
if [ "$STATUS" -eq 0 ] && [ -x "$WORK/vpref/videre" ]; then
	ok "accepts a v-prefixed version"
else
	bad "accepts a v-prefixed version" "$OUT"
fi

run --help
if [ "$STATUS" -eq 0 ] && printf '%s' "$OUT" | grep -q "USAGE"; then
	ok "--help exits 0"
else
	bad "--help exits 0" "$OUT"
fi

run --nonsense
if [ "$STATUS" -ne 0 ]; then
	ok "an unknown option is rejected"
else
	bad "an unknown option is rejected" "$OUT"
fi

# ------------------------------------------------------------------ platforms

printf '\nplatform detection\n'

# A uname shim earlier on PATH is the only way to reach these branches on a
# real runner.
mkdir -p "$WORK/shim"
cat >"$WORK/shim/uname" <<'SHIM'
#!/bin/sh
case "$1" in
-s) printf '%s\n' "${FAKE_OS:-Linux}" ;;
-m) printf '%s\n' "${FAKE_ARCH:-x86_64}" ;;
*) printf '%s\n' "${FAKE_OS:-Linux}" ;;
esac
SHIM
chmod +x "$WORK/shim/uname"

shimmed() {
	set +e
	OUT="$(FAKE_OS="$1" FAKE_ARCH="$2" PATH="$WORK/shim:$PATH" \
		sh "$INSTALLER" --to "$WORK/never" 2>&1)"
	STATUS=$?
	set -e
}

shimmed Darwin x86_64
if [ "$STATUS" -ne 0 ] && printf '%s' "$OUT" | grep -qi "intel"; then
	ok "Intel Mac is refused, and says why"
else
	bad "Intel Mac is refused, and says why" "$OUT"
fi

shimmed Linux riscv64
if [ "$STATUS" -ne 0 ] && printf '%s' "$OUT" | grep -q "riscv64"; then
	ok "an unknown architecture names what it saw"
else
	bad "an unknown architecture names what it saw" "$OUT"
fi

shimmed FreeBSD x86_64
if [ "$STATUS" -ne 0 ] && printf '%s' "$OUT" | grep -q "FreeBSD"; then
	ok "an unsupported OS names what it saw"
else
	bad "an unsupported OS names what it saw" "$OUT"
fi

if [ -e "$WORK/never/videre" ]; then
	bad "no binary is written on a refused platform"
else
	ok "no binary is written on a refused platform"
fi

# ------------------------------------------------------------------ verification

printf '\nchecksum verification\n'

# A real archive whose published checksum is wrong. This is the only test that
# exercises the security property, which is why the base-URL seam exists.
# The archive has to be named for the target this runner will ask for, so work
# it out the same way the installer does. Built by staging a directory and
# taring its contents, because BSD tar has no --transform to rename on the fly.
case "$(uname -s)" in
Darwin) TARGET="aarch64-apple-darwin" ;;
*)
	case "$(uname -m)" in
	aarch64 | arm64) TARGET="aarch64-unknown-linux-gnu" ;;
	*) TARGET="x86_64-unknown-linux-gnu" ;;
	esac
	;;
esac

FAKE="$WORK/fake/v$PINNED"
mkdir -p "$FAKE" "$WORK/payload"
printf '#!/bin/sh\necho nope\n' >"$WORK/payload/videre"
tar czf "$FAKE/videre-v$PINNED-$TARGET.tar.gz" -C "$WORK/payload" videre
printf '%s  %s\n' \
	"0000000000000000000000000000000000000000000000000000000000000000" \
	"videre-v$PINNED-$TARGET.tar.gz" >"$FAKE/videre-v$PINNED-$TARGET.sha256"

set +e
OUT="$(VIDERE_INSTALL_BASE_URL="file://$WORK/fake" \
	sh "$INSTALLER" --version "$PINNED" --to "$WORK/corrupt" 2>&1)"
STATUS=$?
set -e

if [ "$STATUS" -ne 0 ] && printf '%s' "$OUT" | grep -qi "checksum mismatch"; then
	ok "a corrupted archive is rejected"
else
	bad "a corrupted archive is rejected" "$OUT"
fi

if [ -e "$WORK/corrupt/videre" ]; then
	bad "nothing is installed when the checksum fails"
else
	ok "nothing is installed when the checksum fails"
fi

printf '\nmissing release\n'
run --version 99.0.0 --to "$WORK/nope"
if [ "$STATUS" -ne 0 ] && printf '%s' "$OUT" | grep -q "no such release"; then
	ok "a missing release says so, rather than leaking a curl error"
else
	bad "a missing release says so, rather than leaking a curl error" "$OUT"
fi

# ------------------------------------------------------------------ uninstall

printf '\nuninstall\n'

run --uninstall --to "$WORK/bin"
if [ "$STATUS" -eq 0 ] && [ ! -e "$WORK/bin/videre" ]; then
	ok "removes the binary it installed"
else
	bad "removes the binary it installed" "$OUT"
fi

run --uninstall --to "$WORK/bin"
if [ "$STATUS" -ne 0 ]; then
	ok "uninstalling nothing exits non-zero"
else
	bad "uninstalling nothing exits non-zero" "$OUT"
fi

# :warning: The two below are the only tests asserting a file SURVIVES. Every
# other test here asserts something happened, so a change making uninstall
# slightly more eager passes all of them and fails only these.
FH="$WORK/fakehome"
mkdir -p "$FH/.cargo/bin"
printf '#!/bin/sh\necho "videre 0.1.0"\n' >"$FH/.cargo/bin/videre"
chmod +x "$FH/.cargo/bin/videre"

set +e
OUT="$(HOME="$FH" sh "$INSTALLER" --uninstall --to "$FH/.cargo/bin" 2>&1)"
STATUS=$?
set -e

if [ "$STATUS" -ne 0 ] && [ -e "$FH/.cargo/bin/videre" ] &&
	printf '%s' "$OUT" | grep -q "cargo uninstall videre"; then
	ok "refuses a cargo-owned binary, and it survives"
else
	bad "refuses a cargo-owned binary, and it survives" "$OUT"
fi

BP="$WORK/brewprefix"
mkdir -p "$BP/bin" "$WORK/brewshim"
printf '#!/bin/sh\n[ "$1" = "--prefix" ] && printf "%%s\\n" "%s"\n' "$BP" \
	>"$WORK/brewshim/brew"
chmod +x "$WORK/brewshim/brew"
printf '#!/bin/sh\necho "videre 0.1.0"\n' >"$BP/bin/videre"
chmod +x "$BP/bin/videre"

set +e
OUT="$(PATH="$WORK/brewshim:$PATH" sh "$INSTALLER" --uninstall --to "$BP/bin" 2>&1)"
STATUS=$?
set -e

if [ "$STATUS" -ne 0 ] && [ -e "$BP/bin/videre" ] &&
	printf '%s' "$OUT" | grep -q "brew uninstall videre"; then
	ok "refuses a homebrew-owned binary, and it survives"
else
	bad "refuses a homebrew-owned binary, and it survives" "$OUT"
fi

# ------------------------------------------------------------------ result

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]

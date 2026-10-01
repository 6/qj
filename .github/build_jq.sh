#!/bin/sh
# Builds jq from its release tarball (the version mise.toml pins), for a
# platform jq publishes no binary for (musl, FreeBSD, NetBSD): the jq a user
# there would build, with Oniguruma from the tarball and decNumber on, as in
# jq's release builds. The tarball is checked against the release's
# sha256sum.txt.
#
#   sh .github/build_jq.sh OUT_DIR      # OUT_DIR/bin/jq
#
# Needs a C compiler, curl and GNU make (MAKE=gmake on the BSDs).
set -eu
out=$1
make=${MAKE:-make}
version=$(sed -n 's/^jq = "\(.*\)"/\1/p' mise.toml)
url=https://github.com/jqlang/jq/releases/download/jq-$version
work=$(mktemp -d "${TMPDIR:-/tmp}/jq-build.XXXXXX")
cd "$work"
curl -fsSL -o "jq-$version.tar.gz" "$url/jq-$version.tar.gz"
curl -fsSL "$url/sha256sum.txt" | grep " jq-$version.tar.gz\$" > sum.txt
# sha256sum (Linux, BusyBox), else OpenSSL (in the BSDs' base systems).
if command -v sha256sum > /dev/null; then
    digest=$(sha256sum "jq-$version.tar.gz")
else
    digest=$(openssl dgst -sha256 -r "jq-$version.tar.gz")
fi
test "${digest%% *}" = "$(cut -d' ' -f1 sum.txt)"
tar xzf "jq-$version.tar.gz"
cd "jq-$version"
./configure --prefix="$out" --with-oniguruma=builtin --disable-docs --disable-shared > configure.log
"$make" -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)" > make.log
"$make" install > install.log
"$out/bin/jq" --version

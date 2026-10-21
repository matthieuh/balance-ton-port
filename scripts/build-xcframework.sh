#!/usr/bin/env bash
# Builds the Rust core for the macOS app:
#   app/Frameworks/BtpCore.xcframework   universal (arm64 + x86_64) static lib,
#                                        C header and module map
#   app/Sources/Generated/btp_ffi.swift  UniFFI Swift bindings
# Both outputs are generated and git-ignored; rerun this after any change to
# crates/btp-core or crates/btp-ffi.
#
# Requires: macOS with Xcode (xcodebuild, xcrun, lipo, and its libclang for
# the libproc crate's bindgen step), rustup with the aarch64-apple-darwin and
# x86_64-apple-darwin targets (`rustup target add ...`). Works from a clean
# checkout; run from anywhere.
#
# Then build the app (XcodeGen generates the git-ignored .xcodeproj):
#   (cd app && xcodegen generate)
#   xcodebuild -project app/BalanceTonPort.xcodeproj -scheme BalanceTonPort build
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ -d /opt/homebrew/opt/rustup/bin ]; then
    export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi
export PATH="$HOME/.cargo/bin:$PATH"

TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
CRATE=btp-ffi
LIB=libbtp_ffi.a
MODULE=btp_ffiFFI
XCFRAMEWORK="$ROOT/app/Frameworks/BtpCore.xcframework"
SWIFT_OUT="$ROOT/app/Sources/Generated"
WORK="$ROOT/target/xcframework"

# The app targets macOS 14+.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"

# libproc runs bindgen at build time: point clang at the macOS SDK (also for
# the x86_64 cross build) and at Xcode's libclang unless one is already set.
SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
export SDKROOT
export BINDGEN_EXTRA_CLANG_ARGS="-isysroot $SDKROOT ${BINDGEN_EXTRA_CLANG_ARGS:-}"
if [ -z "${LIBCLANG_PATH:-}" ]; then
    XCODE_LIBCLANG="$(xcode-select -p)/Toolchains/XcodeDefault.xctoolchain/usr/lib"
    if [ -f "$XCODE_LIBCLANG/libclang.dylib" ]; then
        export LIBCLANG_PATH="$XCODE_LIBCLANG"
    fi
fi

installed="$(rustup target list --installed)"
for t in "${TARGETS[@]}"; do
    if ! grep -qx "$t" <<<"$installed"; then
        echo "error: missing Rust target $t (run: rustup target add $t)" >&2
        exit 1
    fi
done

echo "==> Building $CRATE (release) for ${TARGETS[*]}"
for t in "${TARGETS[@]}"; do
    cargo build --locked --release -p "$CRATE" --lib --target "$t"
done

rm -rf "$WORK"
mkdir -p "$WORK/lib" "$WORK/headers" "$WORK/bindings"

echo "==> lipo universal $LIB"
lipo -create \
    "target/aarch64-apple-darwin/release/$LIB" \
    "target/x86_64-apple-darwin/release/$LIB" \
    -output "$WORK/lib/$LIB"

echo "==> Generating Swift bindings"
cargo run --locked --release -p "$CRATE" --features bindgen --bin uniffi-bindgen -- \
    generate --library "target/aarch64-apple-darwin/release/$LIB" \
    --language swift --out-dir "$WORK/bindings"

cp "$WORK/bindings/$MODULE.h" "$WORK/headers/"
# Xcode only picks up a module map named module.modulemap.
cp "$WORK/bindings/$MODULE.modulemap" "$WORK/headers/module.modulemap"

echo "==> Assembling $XCFRAMEWORK"
rm -rf "$XCFRAMEWORK"
mkdir -p "$(dirname "$XCFRAMEWORK")"
xcodebuild -create-xcframework \
    -library "$WORK/lib/$LIB" -headers "$WORK/headers" \
    -output "$XCFRAMEWORK" >/dev/null

mkdir -p "$SWIFT_OUT"
cp "$WORK/bindings/btp_ffi.swift" "$SWIFT_OUT/btp_ffi.swift"

echo "==> Done"
echo "    $XCFRAMEWORK"
echo "    $SWIFT_OUT/btp_ffi.swift"

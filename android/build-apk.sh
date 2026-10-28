#!/usr/bin/env bash
# Build the NautiK9 Android app (arm64) without Gradle: aapt2 + javac + d8 + apksigner.
#
# Needs: ANDROID_HOME with platforms;android-35, build-tools;35.0.0 and ndk;27.2.12479018,
#        a JDK 17+, zip, and the Rust target aarch64-linux-android.
# Env:   LLAMA_SERVER  llama.cpp's llama-server built for Android arm64 (android/build-llama.sh)
#        KEYSTORE      signing keystore (default ~/.config/nautik9-android/release.jks, created
#                      if missing; keep it: an app signed with a new key cannot update the old one)
#        VERSION_NAME / VERSION_CODE
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
: "${ANDROID_HOME:?set ANDROID_HOME}"
: "${LLAMA_SERVER:?set LLAMA_SERVER to an Android arm64 llama-server}"
VERSION_NAME=${VERSION_NAME:-0.2.0}
# DEBUGGABLE=1 (default for now): adb run-as and WebView DevTools over adb, for working on the
# app live over wireless debugging. Set DEBUGGABLE=0 for a build you hand to someone else.
DEBUGGABLE=${DEBUGGABLE:-1}
VERSION_CODE=${VERSION_CODE:-2}
NDK=$ANDROID_HOME/ndk/27.2.12479018
TC=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
BT=$ANDROID_HOME/build-tools/35.0.0
JAR=$ANDROID_HOME/platforms/android-35/android.jar
KEYDIR=$HOME/.config/nautik9-android
KEYSTORE=${KEYSTORE:-$KEYDIR/release.jks}
KSPASS=${KSPASS:-$KEYDIR/keystore.pass}

# 1. The server, cross-compiled for the phone
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$TC/aarch64-linux-android29-clang
export CC_aarch64_linux_android=$TC/aarch64-linux-android29-clang
export AR_aarch64_linux_android=$TC/llvm-ar
# 16 KB page alignment (required on newer Android; harmless on 4 KB devices)
export CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS="-C link-arg=-Wl,-z,max-page-size=16384"
target_dir=${CARGO_TARGET_DIR:-$root/target}
(cd "$root" && cargo build --release --target aarch64-linux-android -p nautik9-app)
server=$target_dir/aarch64-linux-android/release/nautik9-app

out=$here/build
rm -rf "$out"
mkdir -p "$out/classes" "$out/dex" "$out/gen" "$out/native/lib/arm64-v8a"

# 2. Resources and manifest
"$BT/aapt2" compile --dir "$here/res" -o "$out/res.zip"
"$BT/aapt2" link -I "$JAR" --manifest "$here/AndroidManifest.xml" -R "$out/res.zip" \
    --java "$out/gen" --min-sdk-version 29 --target-sdk-version 35 \
    --version-code "$VERSION_CODE" --version-name "$VERSION_NAME" \
    $([ "$DEBUGGABLE" = 1 ] && echo --debug-mode) -o "$out/base.apk"

# 3. Java to dex
javac -source 17 -target 17 -Xlint:-options -encoding UTF-8 -classpath "$JAR" -d "$out/classes" \
    $(find "$here/src" "$out/gen" -name '*.java')
"$BT/d8" --release --min-api 29 --lib "$JAR" --output "$out/dex" $(find "$out/classes" -name '*.class')

# 4. Native executables, shipped as lib*.so so Android installs them executable
"$TC/llvm-strip" -o "$out/native/lib/arm64-v8a/libnautik9.so" "$server"
"$TC/llvm-strip" -o "$out/native/lib/arm64-v8a/libllama_server.so" "$LLAMA_SERVER"

cp "$out/base.apk" "$out/unsigned.apk"
(cd "$out/dex" && zip -q "$out/unsigned.apk" classes.dex)
(cd "$out/native" && zip -q -r "$out/unsigned.apk" lib)
"$BT/zipalign" -p -f 4 "$out/unsigned.apk" "$out/aligned.apk"

# 5. Sign
if [ ! -f "$KEYSTORE" ]; then
    mkdir -p "$KEYDIR" && chmod 700 "$KEYDIR"
    head -c 24 /dev/urandom | base64 | tr -d '/+=' > "$KSPASS" && chmod 600 "$KSPASS"
    keytool -genkeypair -keystore "$KEYSTORE" -alias nautik9 -keyalg RSA -keysize 4096 -validity 10000 \
        -storepass:file "$KSPASS" -keypass:file "$KSPASS" -dname "CN=NautiK9, O=Thom Hadfield" >/dev/null
    chmod 600 "$KEYSTORE"
    echo "Created signing key $KEYSTORE (back it up; updates need the same key)"
fi
"$BT/apksigner" sign --ks "$KEYSTORE" --ks-key-alias nautik9 --ks-pass "file:$KSPASS" \
    --out "$out/nautik9-$VERSION_NAME.apk" "$out/aligned.apk"
"$BT/apksigner" verify "$out/nautik9-$VERSION_NAME.apk"
echo "Built $out/nautik9-$VERSION_NAME.apk"

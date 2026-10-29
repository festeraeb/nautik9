#!/usr/bin/env bash
# Build llama.cpp's llama-server for the phone (Android arm64, CPU) as the on-device model engine.
# Usage: android/build-llama.sh <llama.cpp source dir> [build dir]
# The binary lands in <build dir>/bin/llama-server; pass it to build-apk.sh as LLAMA_SERVER.
# armv8.6-a+dotprod+i8mm suits Snapdragon 8 Gen 2 and newer (S23 and later, S25 Ultra);
# set ARM_ARCH=armv8.2-a+dotprod for older phones.
set -euo pipefail
src=${1:?llama.cpp source dir}
build=${2:-$(cd "$(dirname "$0")" && pwd)/build-llama}
: "${ANDROID_HOME:?set ANDROID_HOME}"
NDK=$ANDROID_HOME/ndk/27.2.12479018
ARM_ARCH=${ARM_ARCH:-armv8.6-a+dotprod+i8mm}

cmake -S "$src" -B "$build" -G Ninja \
    -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=android-29 -DANDROID_STL=c++_static \
    -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF \
    -DCMAKE_EXE_LINKER_FLAGS="-Wl,-z,max-page-size=16384" \
    -DGGML_NATIVE=OFF -DGGML_CPU_ARM_ARCH="$ARM_ARCH" -DGGML_OPENMP=OFF \
    -DLLAMA_CURL=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_SERVER=ON
cmake --build "$build" --target llama-server -j "$(nproc)"
echo "Built $build/bin/llama-server"

#!/usr/bin/env bash
# Wrapper that sets up MSVC + LLVM environment and runs cargo.
# Usage: bash scripts/cargo-win.sh check -p fauna-sync-agent

MSVC_VER="14.50.35717"
VS_PATH="/c/Program Files (x86)/Microsoft Visual Studio/18/BuildTools"
MSVC_BIN="$VS_PATH/VC/Tools/MSVC/$MSVC_VER/bin/Hostarm64/arm64"
SDK_VER="10.0.26100.0"
SDK_PATH="/c/Program Files (x86)/Windows Kits/10"

export PATH="$MSVC_BIN:/c/Program Files/LLVM/bin:$HOME/.cargo/bin:/c/Users/$USERNAME/AppData/Local/Programs/Python/Python313-arm64:/c/Users/$USERNAME/AppData/Local/Programs/Python/Python313-arm64/Scripts:$PATH"

export LIB="$VS_PATH/VC/Tools/MSVC/$MSVC_VER/lib/arm64;$SDK_PATH/Lib/$SDK_VER/um/arm64;$SDK_PATH/Lib/$SDK_VER/ucrt/arm64"
export INCLUDE="$VS_PATH/VC/Tools/MSVC/$MSVC_VER/include;$SDK_PATH/Include/$SDK_VER/ucrt;$SDK_PATH/Include/$SDK_VER/um;$SDK_PATH/Include/$SDK_VER/shared"

export CC=cl
export CXX=cl

exec cargo "$@"

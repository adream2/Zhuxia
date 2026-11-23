#!/usr/bin/env bash
# 驻匣 build env: rustup toolchain + MSVC BuildTools + Windows SDK.
# source this before cargo build (the machine's PATH rustc is a different install).
RT="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc"
MSVC="/c/Program Files (x86)/Microsoft Visual Studio/2022/BuildTools/VC/Tools/MSVC/14.44.35207"
SDK="C:\\Program Files (x86)\\Windows Kits\\10"
SDKVER="10.0.26100.0"

export RUSTC="$RT/bin/rustc.exe"
export CARGO="$RT/bin/cargo.exe"
export PATH="$MSVC/bin/Hostx64/x64:$RT/bin:$PATH"
export LIB="$MSVC\\lib\\x64;$SDK\\Lib\\$SDKVER\\um\\x64;$SDK\\Lib\\$SDKVER\\ucrt\\x64"
export INCLUDE="$MSVC\\include;$SDK\\Include\\$SDKVER\\um;$SDK\\Include\\$SDKVER\\ucrt;$SDK\\Include\\$SDKVER\\shared"

#!/usr/bin/env bash
set -euo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
source_dir="$repo_root/ref/AIMtk-source-20260420/RamDyn"
include_dir="$repo_root/ref/AIMtk-source-20260420/inc"
output_path=${1:-"$repo_root/runtime/RamDyn.exe"}

mingw_cc=${MINGW_CC:-x86_64-w64-mingw32-gcc-posix}
mingw_windres=${MINGW_WINDRES:-x86_64-w64-mingw32-windres}
mingw_objdump=${MINGW_OBJDUMP:-x86_64-w64-mingw32-objdump}

for tool in "$mingw_cc" "$mingw_windres" "$mingw_objdump" sed grep sha256sum; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        printf 'Missing build tool: %s\n' "$tool" >&2
        exit 1
    fi
done

if [[ ! -f "$source_dir/RamDyn.c" || ! -f "$source_dir/resource.rc" || ! -d "$include_dir" ]]; then
    printf 'RamDyn reference source is incomplete: %s\n' "$source_dir" >&2
    exit 1
fi

build_dir=$(mktemp -d)
trap 'rm -rf "$build_dir"' EXIT
mkdir -p "$build_dir/RamDyn"
cp "$source_dir/RamDyn.c" "$source_dir/resource.rc" "$build_dir/RamDyn/"
cp -a "$include_dir" "$build_dir/inc"

# The upstream source uses Windows separators in include directives. Normalize
# only this temporary copy so Linux-hosted MinGW can resolve the sibling folder.
sed -i 's#..\\inc\\#../inc/#g' "$build_dir/RamDyn/RamDyn.c"

"$mingw_windres" \
    --preprocessor="$mingw_cc" \
    --preprocessor-arg=-E \
    --preprocessor-arg=-xc \
    "$build_dir/RamDyn/resource.rc" \
    "$build_dir/RamDyn/resource.o"

# Debian MinGW-w64 14.2 headers fail to compile this source with _CRTBLD;
# omitting that macro still links directly against msvcrt.dll.
"$mingw_cc" \
    "$build_dir/RamDyn/RamDyn.c" "$build_dir/RamDyn/resource.o" \
    -o "$build_dir/RamDyn/RamDyn.exe" \
    -mno-sse3 -municode -mwindows -s -O3 -minline-all-stringops -Wall \
    -fno-ident -nostdlib \
    -lmsvcrt -lkernel32 -lntdll -luser32 -ladvapi32 -lwtsapi32 \
    -Wl,--gc-sections,--nxcompat,--dynamicbase,--high-entropy-va,--no-insert-timestamp \
    -pie -e wWinMain

if ! "$mingw_objdump" -f "$build_dir/RamDyn/RamDyn.exe" | grep -q 'file format pei-x86-64'; then
    printf 'The generated RamDyn.exe is not an x64 PE file.\n' >&2
    exit 1
fi

mkdir -p "$(dirname -- "$output_path")"
cp "$build_dir/RamDyn/RamDyn.exe" "$output_path"
printf 'Generated x64 RamDyn candidate: %s\n' "$output_path"
sha256sum "$output_path"

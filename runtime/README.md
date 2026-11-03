# Runtime files

The `runtime/` directory beside the manager contains three x64 files:

- `RamDyn.exe`: AIM Toolkit's RamDyn backend.
- `aimapi-backend.dll`: the original AIM API, used by the manager and the shim.
- `aimapi.dll`: the adapter shim loaded by RamDyn. A native Windows x64 Cargo build compiles it; the portable build script copies it here. It routes calls to the AIM port selected by `AIM_ADAPTER_PORT`.

Keep all three files together. RamDyn starts directly from this directory; the manager does not create a temporary runtime directory.

Run `scripts/build-dev-portable.ps1` to build a portable folder. The script downloads the pinned AIM Toolkit 20260420 package, verifies the upstream `RamDyn.exe` and `aimapi.dll`, renames the real API to `aimapi-backend.dll`, and copies the compiled shim as `aimapi.dll`. For `cargo run` development, manually place the compiled shim and the two upstream runtime files in this directory. If SourceForge download is blocked, save `AIMtk.zip` to this directory and rerun the script.

The source archive omits `ref/` and runtime binaries. The build script obtains the upstream files; a normal Cargo build does not download them.

---

# 运行文件

管理器旁的 `runtime/` 目录需要包含三个 x64 文件：

- `RamDyn.exe`：AIM Toolkit 的 RamDyn 后端。
- `aimapi-backend.dll`：原版 AIM API，由管理器和 shim 使用。
- `aimapi.dll`：RamDyn 加载的适配 shim。Windows x64 本机 Cargo 编译会在 `target/` 下生成此文件，便携构建脚本会将其复制到这里；它根据 `AIM_ADAPTER_PORT` 将调用转发到所选 AIM 端口。

请将三个文件放在同一目录。RamDyn 直接从这里启动，管理器不会创建临时运行目录。

运行 `scripts/build-dev-portable.ps1` 可生成便携目录。脚本会下载固定版本 AIM Toolkit 20260420，校验上游 `RamDyn.exe` 和 `aimapi.dll`，将真实 API 改名为 `aimapi-backend.dll`，再把已编译的 shim 作为 `aimapi.dll` 一起打包。使用 `cargo run` 调试时，需要手动把已编译的 shim 和两个上游运行文件放进本目录。如果 SourceForge 下载被拦截，请将 `AIMtk.zip` 手动保存到本目录后重跑脚本。

源码包不包含 `ref/` 和运行时二进制；编译脚本会获取上游运行文件，普通 Cargo 编译不会下载它们。

# AIM RAM Disk Manager

A Windows x64 desktop app built with Rust and egui. It uses compatible AIM drivers to create RamDyn disks, AIM native VM or AWEAlloc disks, and manage disks across AIM adapters. It supports NTFS/exFAT/FAT32, saved profiles, optional logon auto-mount, drive-letter or empty-folder mounts, and single- or multi-volume unmounting. Existing AIM image files can be mounted directly (one readable volume per image); a directory can be copied into a new disk and synchronized back on unmount.

The interface follows the operating system UI language by default: Chinese selects Simplified Chinese, and other languages select English. Use the top-right selector to choose **Follow system**, **简体中文**, or **English**; the preference is saved in `data/manager-settings.json`.

Translations use language-neutral message keys. The English and Simplified Chinese catalogs live in `locales/en.json` and `locales/zh-CN.json` and are embedded at build time. Use `scripts/next-locale-key.ps1` to allocate a key for a new message, then add it to both catalogs.

The manager does not install or update drivers or change AIM Toolkit startup settings. Closing the window leaves mounted disks in place. Unmounting or ending the Windows session loses RAM disk data; changes to a mounted image are written to that image, and directory synchronization runs when the disk is unmounted.

Install AIM Toolkit on the computer before using the manager, and make sure its compatible driver is installed. The manager depends on AIM Toolkit and its driver; it does not install them.

## Build

Requires Windows x64, Rust 1.88 or later, and MSVC C++ build tools:

```powershell
cargo build --locked --release --target x86_64-pc-windows-msvc
```

On a native Windows x64 build, Cargo also compiles the RamDyn adapter shim. To build a development portable folder, run `scripts/build-dev-portable.ps1`. It downloads a pinned AIM Toolkit package from SourceForge, verifies RamDyn and the AIM API, then places the API as `aimapi-backend.dll` and the shim as `aimapi.dll` in the package's `runtime/` folder. If automatic download is blocked, save the browser-downloaded `AIMtk.zip` as `runtime/AIMtk.zip` and rerun the script. See [runtime/README.md](runtime/README.md) for details.

The manually triggered GitHub Actions artifact contains only `aim-ramdisk-manager.exe` and `aimapi-adapter-shim.dll`, so it is not a complete portable package. The folder layout below shows which AIM Toolkit files must be added and how to name them.

The app requires administrator approval at launch. RamDyn AWE and Large-page modes also require the **Lock pages in memory** privilege. AIM native AWEAlloc uses AIM's AWEAlloc driver.

## Runtime and data

The local portable build script creates `dist/dev-portable/` with this layout. The folder name can be changed with `-OutputDirectory`.

```text
portable/
├── aim-ramdisk-manager.exe       # Cargo release build; also included in the Actions artifact
├── README.md                     # copied from the repository root by the portable build script
├── runtime/
│   ├── RamDyn.exe                # AIM Toolkit 20260420 package; extracted from files.cab
│   ├── aimapi-backend.dll        # AIM Toolkit aimapi.dll, renamed by the build script
│   ├── aimapi.dll                # shim built from src/ramdyn_adapter_shim.rs; copied and renamed
│   └── README.md                 # copied from runtime/README.md in the repository
└── data/                         # created by the app as needed; absent in a fresh build
    ├── manager-settings.json     # UI language and startup preferences/results
    ├── profiles.json             # saved disk profiles
    ├── pending-creates.json      # unresolved disk-creation records
    └── active-disk-modes.json    # memory-mode and sync metadata for managed live disks
```

For an Actions build, download the two artifact files and assemble the same `runtime/` folder: copy `RamDyn.exe` and the installed AIM Toolkit's original `aimapi.dll` into it, naming the original API `aimapi-backend.dll`; rename the artifact's `aimapi-adapter-shim.dll` to `aimapi.dll`. The two AIM Toolkit files should come from the x64 Toolkit installation on the computer where its compatible driver is installed. The Actions artifact does not include either README or the `data/` folder. The app creates `data/` and its JSON files when it saves the corresponding settings or state; keep this folder beside the executable when moving the app. The app icon from `assets/VD.ico` is embedded in the executable and does not need to be copied separately.

Basic AIM detection, volume-label changes, profile saving, and Large-page disk creation/removal were confirmed earlier. Native AWEAlloc, multi-adapter operations, image writeback, and directory synchronization still need Windows/AIM testing.

---

# AIM 临时内存盘管理器

基于 Rust 和 egui 的 Windows x64 桌面工具，复用兼容的 AIM 驱动来创建 RamDyn、AIM 原生 VM 或 AWEAlloc 磁盘，并管理多个 AIM 适配器上的设备。支持 NTFS/exFAT/FAT32、保存配置、登录自动挂载、盘符或空目录挂载，以及单卷和多卷卸载。可直接挂载已有 AIM 镜像（每个镜像仅支持一个可读取卷），也可在新盘创建时载入目录并在卸载时同步保存。

界面默认跟随操作系统 UI 语言：中文系统显示简体中文，其它语言显示英文。使用右上角选择器切换 **跟随系统**、**简体中文** 或 **English**；语言偏好保存在 `data/manager-settings.json`。

代码使用与语言无关的文案标记。英文和简体中文目录分别保存在 `locales/en.json` 与 `locales/zh-CN.json`，并在编译时嵌入程序。新增文案时，运行 `scripts/next-locale-key.ps1` 分配标记，再将它加入两份目录。

管理器不会安装或更新驱动，也不会修改 AIM Toolkit 的启动设置。关闭窗口不会卸载已挂载的盘；RAM 盘数据会在卸载或结束 Windows 会话时丢失。镜像更改写入镜像文件，目录同步在卸载时执行。

使用前请先在目标电脑安装 AIM Toolkit，并确认其兼容驱动已安装。管理器依赖 AIM Toolkit 及其驱动，不会替你安装。

## 构建

需要 Windows x64、Rust 1.88 或更高版本及 MSVC C++ 构建工具：

```powershell
cargo build --locked --release --target x86_64-pc-windows-msvc
```

在 Windows x64 本机编译时，Cargo 还会编译 RamDyn 适配 shim。运行 `scripts/build-dev-portable.ps1` 可生成开发便携目录：脚本会从 SourceForge 下载固定版本的 AIM Toolkit 安装包，校验 RamDyn 和 AIM API，再将真实 API 作为 `aimapi-backend.dll`、shim 作为 `aimapi.dll` 放入输出目录的 `runtime/`。如果自动下载被拦截，请用浏览器下载 `AIMtk.zip`，保存到 `runtime/AIMtk.zip` 后重跑脚本。详情见 [runtime/README.md](runtime/README.md)。

手动触发的 GitHub Actions 预编译产物只包含 `aim-ramdisk-manager.exe` 和 `aimapi-adapter-shim.dll`，不是完整便携包。所需目录结构及 AIM Toolkit 文件的来源和改名方式见下文。

程序启动需要管理员确认。RamDyn 的 AWE 和 Large-page 模式还需要“锁定内存页”权限；AIM 原生 AWEAlloc 使用 AIM 的 AWEAlloc 驱动。

## 运行文件与数据

本地便携构建脚本默认生成 `dist/dev-portable/`，目录结构如下。可使用 `-OutputDirectory` 更改输出目录。

```text
portable/
├── aim-ramdisk-manager.exe       # Cargo release 编译产物；Actions 产物也包含此文件
├── README.md                     # 便携构建脚本从仓库根目录复制
├── runtime/
│   ├── RamDyn.exe                # AIM Toolkit 20260420 安装包中的 files.cab 解压所得
│   ├── aimapi-backend.dll        # AIM Toolkit 原版 aimapi.dll，由构建脚本改名
│   ├── aimapi.dll                # 由 src/ramdyn_adapter_shim.rs 编译的 shim，复制后改名
│   └── README.md                 # 从仓库的 runtime/README.md 复制
└── data/                         # 管理器按需创建；刚构建完成时通常不存在
    ├── manager-settings.json     # 界面语言、启动选项和启动结果
    ├── profiles.json             # 保存的磁盘配置
    ├── pending-creates.json      # 尚未解决的磁盘创建记录
    └── active-disk-modes.json    # 管理器所管运行中磁盘的内存模式和同步信息
```

使用 Actions 产物时，请下载其中两个文件并自行组装 `runtime/`：从已安装的 x64 AIM Toolkit 中复制 `RamDyn.exe` 和原版 `aimapi.dll`，将原版 API 命名为 `aimapi-backend.dll`；把产物里的 `aimapi-adapter-shim.dll` 改名为 `aimapi.dll`。这两个 AIM Toolkit 文件应来自安装了兼容驱动的那套 Toolkit。Actions 产物不含两个 README，也不含 `data/`。管理器会在保存相应设置或状态时创建 `data/` 及 JSON 文件；迁移程序时请将该目录与 exe 放在一起复制。应用图标来自 `assets/VD.ico`，已嵌入 exe，不需要单独复制。

此前已确认 AIM 设备检测、修改卷标、保存配置及 Large-page 磁盘创建和删除正常。原生 AWEAlloc、多适配器操作、镜像写回和目录同步仍需在 Windows/AIM 环境验证；交叉编译与自动化测试不能代替驱动实机验收。

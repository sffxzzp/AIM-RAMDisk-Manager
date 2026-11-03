# AIM RAM Disk Manager

A Windows x64 desktop app built with Rust and egui. It uses compatible AIM drivers to create RamDyn disks, AIM native VM or AWEAlloc disks, and manage disks across AIM adapters. It supports NTFS/exFAT/FAT32, saved profiles, optional logon auto-mount, drive-letter or empty-folder mounts, and single- or multi-volume unmounting. Existing AIM image files can be mounted directly (one readable volume per image); a directory can be copied into a new disk and synchronized back on unmount.

The interface follows the operating system UI language by default: Chinese selects Simplified Chinese, and other languages select English. Use the top-right selector to choose **Follow system**, **简体中文**, or **English**; the preference is saved in `data/manager-settings.json`.

Translations use language-neutral message keys. The English and Simplified Chinese catalogs live in `locales/en.json` and `locales/zh-CN.json` and are embedded at build time. Use `scripts/next-locale-key.ps1` to allocate a key for a new message, then add it to both catalogs.

The manager does not install or update drivers or change AIM Toolkit startup settings. Closing the window leaves mounted disks in place. Unmounting or ending the Windows session loses RAM disk data; changes to a mounted image are written to that image, and directory synchronization runs when the disk is unmounted.

## Build

Requires Windows x64, Rust 1.88 or later, and MSVC C++ build tools:

```powershell
cargo build --locked --release --target x86_64-pc-windows-msvc
```

On a native Windows x64 build, Cargo also compiles the RamDyn adapter shim. To build a development portable folder, run `scripts/build-dev-portable.ps1`. It downloads a pinned AIM Toolkit package from SourceForge, verifies RamDyn and the AIM API, then places the API as `aimapi-backend.dll` and the shim as `aimapi.dll` in the package's `runtime/` folder. If automatic download is blocked, save the browser-downloaded `AIMtk.zip` as `runtime/AIMtk.zip` and rerun the script. See [runtime/README.md](runtime/README.md) for details.

The app requires administrator approval at launch. RamDyn AWE and Large-page modes also require the **Lock pages in memory** privilege. AIM native AWEAlloc uses AIM's AWEAlloc driver.

## Runtime and data

The `runtime/` folder beside the executable contains `RamDyn.exe`, `aimapi-backend.dll` (the real AIM API), and `aimapi.dll` (the adapter shim loaded by RamDyn). The shim routes RamDyn calls to the selected AIM adapter. Settings and disk profiles are kept in the adjacent `data/` folder. Copy both folders with the executable when moving the app.

Basic AIM detection, volume-label changes, profile saving, and Large-page disk creation/removal were confirmed earlier. Native AWEAlloc, multi-adapter operations, image writeback, and directory synchronization still need Windows/AIM testing.

---

# AIM 临时内存盘管理器

基于 Rust 和 egui 的 Windows x64 桌面工具，复用兼容的 AIM 驱动来创建 RamDyn、AIM 原生 VM 或 AWEAlloc 磁盘，并管理多个 AIM 适配器上的设备。支持 NTFS/exFAT/FAT32、保存配置、登录自动挂载、盘符或空目录挂载，以及单卷和多卷卸载。可直接挂载已有 AIM 镜像（每个镜像仅支持一个可读取卷），也可在新盘创建时载入目录并在卸载时同步保存。

界面默认跟随操作系统 UI 语言：中文系统显示简体中文，其它语言显示英文。使用右上角选择器切换 **跟随系统**、**简体中文** 或 **English**；语言偏好保存在 `data/manager-settings.json`。

代码使用与语言无关的文案标记。英文和简体中文目录分别保存在 `locales/en.json` 与 `locales/zh-CN.json`，并在编译时嵌入程序。新增文案时，运行 `scripts/next-locale-key.ps1` 分配标记，再将它加入两份目录。

管理器不会安装或更新驱动，也不会修改 AIM Toolkit 的启动设置。关闭窗口不会卸载已挂载的盘；RAM 盘数据会在卸载或结束 Windows 会话时丢失。镜像更改写入镜像文件，目录同步在卸载时执行。

## 构建

需要 Windows x64、Rust 1.88 或更高版本及 MSVC C++ 构建工具：

```powershell
cargo build --locked --release --target x86_64-pc-windows-msvc
```

在 Windows x64 本机编译时，Cargo 还会编译 RamDyn 适配 shim。运行 `scripts/build-dev-portable.ps1` 可生成开发便携目录：脚本会从 SourceForge 下载固定版本的 AIM Toolkit 安装包，校验 RamDyn 和 AIM API，再将真实 API 作为 `aimapi-backend.dll`、shim 作为 `aimapi.dll` 放入输出目录的 `runtime/`。如果自动下载被拦截，请用浏览器下载 `AIMtk.zip`，保存到 `runtime/AIMtk.zip` 后重跑脚本。详情见 [runtime/README.md](runtime/README.md)。

程序启动需要管理员确认。RamDyn 的 AWE 和 Large-page 模式还需要“锁定内存页”权限；AIM 原生 AWEAlloc 使用 AIM 的 AWEAlloc 驱动。

## 运行文件与数据

程序旁的 `runtime/` 目录需要包含 `RamDyn.exe`、真实 AIM API `aimapi-backend.dll`，以及由 RamDyn 加载的适配 shim `aimapi.dll`。shim 会将 RamDyn 的调用路由到所选 AIM 适配器。设置和磁盘配置保存在旁边的 `data/` 目录。移动程序时请一并复制这两个目录。

此前已确认 AIM 设备检测、修改卷标、保存配置及 Large-page 磁盘创建和删除正常。原生 AWEAlloc、多适配器操作、镜像写回和目录同步仍需在 Windows/AIM 环境验证；交叉编译与自动化测试不能代替驱动实机验收。

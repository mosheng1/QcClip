# 多平台与多架构打包

使用 `cargo-packager 0.11.8`，统一配置位于 `Cargo.toml` 的 `[package.metadata.packager]`。版本号和主程序名称由 Cargo 元数据读取，不需要另外维护。

`scripts/package.ps1` 使用 PowerShell 7，按当前 Rust 主机平台选择安装格式，支持 `x64`、`arm64`。默认只构建当前主机架构；传入两种架构时依次构建和打包。Windows 默认同时生成 NSIS 安装包和便携 ZIP，其他平台默认只生成安装包。脚本不自动安装工具链或系统依赖。

## 平台与目标

| 平台 | 架构 | Rust 目标 | 默认安装格式 |
| --- | --- | --- | --- |
| Windows | x64 | `x86_64-pc-windows-msvc` | NSIS `.exe` |
| Windows | ARM64 | `aarch64-pc-windows-msvc` | NSIS `.exe` |
| macOS | x64 | `x86_64-apple-darwin` | `.dmg` |
| macOS | ARM64 | `aarch64-apple-darwin` | `.dmg` |
| Linux | x64 | `x86_64-unknown-linux-gnu` | `.deb`、`.AppImage` |
| Linux | ARM64 | `aarch64-unknown-linux-gnu` | `.deb`、`.AppImage` |

打包入口支持这些目标，不代表应用已经完成全部平台适配。当前应用仍有 Windows 专属调用，例如 `src/app.rs` 未按平台隔离的 `image_temp_file` 调用；`build.rs` 的 Windows 图标资源处理也需要在非 Windows 环境检查。macOS 和 Linux 必须先完成应用适配并在对应系统验证，再作为正式发布目标。

## 安装打包工具

```powershell
cargo install cargo-packager --version 0.11.8 --locked
```

首次 Windows NSIS 打包会下载并缓存 NSIS 及其插件，需要能够访问对应下载地址。NSIS 安装器本身可能使用 x86 引导程序；ARM64 包中的应用程序仍应为原生 ARM64，应检查应用二进制的架构。

## Windows

安装 Visual Studio C++ 桌面开发工具和 Windows SDK。构建 ARM64 还需要在 Visual Studio Installer 中安装相应版本的 MSVC ARM64/ARM64EC 编译工具，并添加 Rust 目标：

```powershell
rustup target add aarch64-pc-windows-msvc
```

在项目根目录运行：

```powershell
# 当前主机架构
pwsh -File scripts/package.ps1

# 分别打包 x64 与 ARM64
./scripts/package.ps1 -Arch x64,arm64

# 只打包 ARM64
pwsh -File scripts/package.ps1 -Arch arm64

# 只生成便携 ZIP
pwsh -File scripts/package.ps1 -PackageType portable

# 只生成 NSIS 安装包
pwsh -File scripts/package.ps1 -PackageType installer
```

安装器默认按当前用户安装，支持简体中文和英文，不配置用户数据删除路径，因此默认保留剪贴板历史、配置以及用户指定的数据目录。开机自启由应用设置管理。

脚本从 Visual Studio 的 `VC/Redist/MSVC/<版本>/<架构>/Microsoft.VC143.CRT` 查找可再分发运行库，仅复制 `vcruntime140.dll` 到安装包和便携 ZIP。当前 x64 程序只依赖这一项 VC 运行库，不附带未使用的 `msvcp140.dll`、`concrt140.dll` 等文件。它会验证应用和运行库的 PE 架构，避免将 x64 DLL 放入 ARM64 包。不要使用系统目录中的 DLL 替代官方可再分发文件；ARM64 发布和原生依赖变更后应重新检查实际 DLL 依赖。

没有自动识别到运行库时，可指定目录（一次只打包对应架构）：

```powershell
pwsh -File scripts/package.ps1 -Arch arm64 -RuntimeDirectory 'C:/工具/MSVC/arm64/Microsoft.VC143.CRT'
```

手工直接运行 `cargo packager` 不会经过脚本的运行库收集逻辑。公开发布前仍应在干净的目标 Windows 系统验证运行依赖和安装流程。

## Windows 便携版

便携版使用程序同级的 `QcClip.portable` 文件标记。解压 `QcClip_<版本>_<架构>-portable.zip` 后运行 `QcClip/QcClip.exe`，首次启动自动创建以下数据目录：

```text
QcClip/
  QcClip.exe
  QcClip.portable
  vcruntime140.dll
  QcClipData/
    config.json
    state.json
    qcclip.sqlite3
    data/
    image_library/
    sound/
    log/
```

- `QcClip.portable` 必须是与程序同级的文件；没有标记时使用安装版的数据目录逻辑。
- 便携数据目录固定为可执行文件所在目录下的 `QcClipData`，不受启动工作目录、安装版的 `location.json` 或同级定位文件影响。
- 设置页禁用“更改数据位置”和“恢复默认数据位置”，后端也拒绝数据位置切换；打开目录、导入、导出和清理仍可使用。
- 退出程序后移动整个 `QcClip` 文件夹即可携带数据。目录需要可写；升级时只覆盖程序、标记和 DLL，保留 `QcClipData`。
- 便携包只包含程序、标记和运行库，不打包构建机上的用户数据。安装版的数据不会自动复制到便携版，可通过现有导出和导入功能转移。
- 只生成便携版时无需安装 `cargo-packager`。ARM64 需要对应编译工具与运行库；macOS、Linux 的便携发布尚未接入。

## 发布日志检查

- 默认只记录主项目 `QcClip` 的 `info` 及以上日志，可用 `RUST_LOG` 调整主项目日志级别；依赖库日志始终不输出。库操作失败需要在主项目调用处记录业务上下文。
- 日志附带源码位置；普通日志、错误消息、panic 和调用栈中的 Windows 盘符路径、UNC 路径、文件 URI 及 Unix 绝对路径统一替换为 `<路径>`，相对源码位置、源码行号和系统错误代码保留。未加双引号的路径会同时隐藏该行后续描述，避免带空格或标点的路径被部分泄露。
- 普通日志保存到数据目录的 `log/qcclip-YYYY-MM-DD.log`。警告和错误同步写入，其他日志异步写入；异步队列丢弃会留下统计。
- 主日志目录无法写入时，尝试本机用户数据目录下的 `QcClip/log-fallback`；启动路径解析前的日志也保存在这里。Windows 通常为 `%LOCALAPPDATA%/QcClip/log-fallback`。备用目录也不可写时，输出到标准错误。
- Rust panic 同步保存到 `qcclip-YYYY-MM-DD.panic.log`，包含版本、进程、线程、源码位置和调用栈，不依赖 `RUST_BACKTRACE`。发布优化可能影响栈符号的完整程度。
- 普通日志达到 5 MiB 时轮转，保留当天三个副本；启动和跨日写入时清理 14 天前的应用日志。独立 panic 文件不按大小轮转。
- “复制最新日志”会比较主目录与备用目录，也支持独立 panic 文件和轮转文件；复制的是经过路径脱敏的临时副本，副本保留到应用退出，原文件不修改。旧日志也会在复制时脱敏；之前生成的原日志不会自动改写。
- 日志不主动记录剪贴板正文。路径脱敏不等于任意文本匿名化，新增日志仍应避免记录剪贴板内容、令牌等敏感数据。

发布前至少验证：数据目录不可写、主日志文件不可写、图片解码失败、剪贴板格式读取失败、快捷键冲突、数据目录切换，以及 panic 后重新启动取日志。原生访问违规、强制终止和系统断电不经过 Rust panic hook；Windows 原生崩溃需结合 Windows Error Reporting 或系统转储定位，不能保证由应用日志完整记录。

## macOS

在 macOS 上安装 Xcode Command Line Tools、Rust 和 PowerShell 7。使用对应架构的主机原生构建最简单；在同一台 Mac 上编译另一架构时，需要安装相应 Rust 目标：

```powershell
rustup target add x86_64-apple-darwin aarch64-apple-darwin
./scripts/package.ps1 -Arch x64,arm64
```

只生成 `.app`：

```powershell
pwsh -File scripts/package.ps1 -Formats app
```

两个架构分别输出安装包。公开发布所需的 Developer ID 签名、公证和剪贴板工具所需的系统权限，需要另外配置和验证。

## Linux

在对应架构的 Linux GNU 环境安装 Rust、PowerShell 7、C/C++ 编译工具以及 GPUI、音频、剪贴板相关系统开发库。AppImage 的运行和打包还可能需要 FUSE 支持。

```powershell
# 在 x64 Linux 上执行
pwsh -File scripts/package.ps1 -Arch x64

# 在 ARM64 Linux 上执行
pwsh -File scripts/package.ps1 -Arch arm64

# 只生成 DEB
pwsh -File scripts/package.ps1 -Formats deb
```

Linux 建议分别使用 x64 和 ARM64 构建机。仅执行 `rustup target add` 不会提供跨架构链接器、目标系统开发库或 AppImage 工具运行环境。

## 内嵌资源与图片格式

构建时使用项目已有的 Zstandard 压缩 Unicode 符号、Emoji 清单和中文关键词数据，运行时首次使用再解压；原始数据保留在 `assets/data`，无需手工生成资源包。资源加载器只嵌入界面图标、PNG 图片、Twemoji 压缩包和图片许可，避免重复打包原始数据及辅助文件。

`vendor/image` 保留锁定版本 `image 0.25.10` 的上游源码，仅将默认图片格式限定为 PNG、JPEG、GIF、WebP、BMP、TIFF、ICO。主项目通过 `[patch.crates-io]` 统一应用到 GPUI 等依赖；其他格式的编解码器不再默认编入程序。此调整不修改发布编译参数。

## 产物与已有构建

产物目录：

```text
target/packages/<Rust 目标>/
```

Windows 安装包命名由打包器生成，例如 `QcClip_1.0.0-beta.1_x64-setup.exe`、`QcClip_1.0.0-beta.1_arm64-setup.exe`；便携包命名为 `QcClip_1.0.0-beta.1_x64-portable.zip`、`QcClip_1.0.0-beta.1_arm64-portable.zip`。目录按目标区分，避免不同平台或架构的产物互相覆盖。

重复打包已有构建：

```powershell
pwsh -File scripts/package.ps1 -Arch x64 -SkipBuild
```

脚本对当前主机目标使用 `target/release`，对另一架构使用 `target/<Rust 目标>/release`；实际目标根目录由 `cargo metadata` 读取，兼容自定义 `CARGO_TARGET_DIR`。`-SkipBuild` 要求对应位置已经存在正确架构的最新程序。

脚本使用 JSON 配置明确传递二进制目录和目标架构，规避 `cargo-packager 0.11.8` 未应用 `--binaries-dir` 参数的问题。JSON 由 Cargo 元数据生成，不需要额外维护版本号。

也可按标准目标目录手工构建和打包，例如在 Windows 主机上：

```powershell
cargo build --release --locked --target x86_64-pc-windows-msvc
cargo packager --release --target x86_64-pc-windows-msvc --formats nsis --out-dir target/packages/x86_64-pc-windows-msvc
```

## 发布验证

首发 `v1.0.0-beta.1` 为公开测试版，面向 Windows。仅发布已在对应架构和干净系统上验证过的产物，检查安装、启动、便携模式、数据保留和卸载；后续版本还需验证升级流程。Windows x64 构建机交叉编译 ARM64 时必须预装 ARM64 MSVC 和 SDK，并在 ARM64 系统验证运行结果。

## GitHub Actions 草稿发布

`.github/workflows/release.yml` 在推送标签时触发。标签必须与 `Cargo.toml` 的应用版本一致，例如版本 `1.0.0-beta.1` 对应 `v1.0.0-beta.1` 或 `1.0.0-beta.1`；版本不一致时会停止构建。`Cargo.lock` 中的主包版本也需同步，否则 `--locked` 构建会失败。

工作流使用 Windows 2022 构建机，分别构建 x64 和 ARM64（`aarch64-pc-windows-msvc`），复用 `scripts/package.ps1` 生成各架构的 NSIS 安装包和便携 ZIP。发布构建使用 fat LTO、优化等级 3、panic abort、符号裁剪，并关闭调试信息、增量编译和溢出检查。仅工作流的应用打包步骤设置 `QCCLIP_RELEASE_GUI=1`，启用 Windows GUI 子系统以隐藏终端窗口；未设置该开关的本地 debug、release 构建及普通打包均保留终端。Cargo 会跟踪该开关，切换后自动重新构建。

两个架构均构建成功，并通过 GUI 子系统和产物完整性校验后，工作流将四个文件汇总上传到当前仓库的草稿 Release。重复运行只更新同标签的草稿，不覆盖已经发布的 Release。工作流使用自带的 `GITHUB_TOKEN`，无需另配私有仓库令牌；仓库或组织策略需允许该令牌写入 Contents。

草稿不会自动发布。工作流根据标签中的预发布后缀设置 GitHub 的 `Pre-release` 标记，例如 `v1.0.0-beta.1` 会标记为预发布版，`v1.0.0` 则不标记。应先下载产物，在对应架构的干净 Windows 系统上完成上述发布验证，并将对应的 `docs/发布日志/<标签>.md` 内容填入发布正文，再手动发布草稿。macOS 和 Linux 完成应用适配并在对应系统验证后，再纳入正式发布流程。

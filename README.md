<p align="center">
  <img src="assets/icon128.png" alt="QcClip" width="96" height="96">
</p>

<h1 align="center">QcClip</h1>

<p align="center">纯 Rust · GPUI 原生渲染 · 高性能剪贴板工具</p>

<p align="center">
  <a href="https://github.com/mosheng1/QcClip/stargazers">
    <img src="https://img.shields.io/github/stars/mosheng1/QcClip?style=for-the-badge&amp;logo=github&amp;color=yellow" alt="Stars">
  </a>
  <a href="https://github.com/mosheng1/QcClip/releases">
    <img src="https://img.shields.io/github/v/release/mosheng1/QcClip?style=for-the-badge&amp;label=Release&amp;color=brightgreen" alt="Release">
  </a>
  <a href="https://github.com/mosheng1/QcClip/releases">
    <img src="https://img.shields.io/github/downloads/mosheng1/QcClip/total?style=for-the-badge&amp;color=blueviolet" alt="Downloads">
  </a>
  <a href="https://github.com/mosheng1/QcClip">
    <img src="https://img.shields.io/github/license/mosheng1/QcClip?style=for-the-badge&amp;color=orange" alt="License">
  </a>
</p>

<p align="center">
  <b>简体中文</b>
  · <a href="i18n/README.en.md">English</a>
  · <a href="i18n/README.zh-TW.md">繁體中文</a>
  · <a href="i18n/README.ja.md">日本語</a>
  · <a href="i18n/README.ko.md">한국어</a>
</p>

**QcClip 重写自作者此前开发的 [QuickClipboard](https://github.com/mosheng1/QuickClipboard)，是一款使用纯 Rust、基于 GPUI 框架原生渲染的高性能剪贴板工具。**

复制了新的内容，之前的文字也不必重新去找。QcClip 帮你保存剪贴板历史，把常用回复、链接、图片和文件集中在手边，需要时随时找回、直接粘贴。

这次重写延续了 QuickClipboard 的使用体验，更注重响应速度、浏览流畅度和日常操作的便利，让剪贴板成为一个轻快、顺手的常用工具。

当前主要支持 **Windows**，macOS 和 Linux 支持仍在完善中。

## 为什么选择 QcClip

### 轻快顺畅，适合每天常驻

剪贴板工具每天可能要打开几十次，响应和滚动体验直接影响使用感受。QcClip 采用原生界面，**不依赖 WebView 或浏览器内核来显示界面**，减少额外的运行开销。同时针对大量历史记录、图片预览和后台常驻做了优化，让每次查找和粘贴尽量少等待。

- **历史多，也方便浏览**：针对长列表优化显示与加载，减少翻找旧内容时的等待。
- **图片多，也照顾资源占用**：优先处理正在浏览的内容，及时释放暂时用不到的资源，减少内存负担。
- **重复复制，减少重复存储**：相同内容重复使用时减少冗余保存，让历史更整洁，也节省磁盘空间。

### 复制过的内容，更容易找回来

整理资料时复制的文字、刚才看到的链接、准备发送的图片，都能回到历史中查找，不必重新打开网页或翻聊天记录。

- **搜得到**：输入内容、备注或来源应用名称，快速缩小查找范围。
- **看得清**：按文字、图片、文件和链接筛选，悬停查看预览，再决定要用哪一条。
- **用得对**：需要排版时选择保留格式，需要干净文字时粘贴为纯文本；图片也可按图片文件粘贴。

### 少点几下，少切几个窗口

按下快捷键呼出窗口，选中内容就能粘贴。经常使用的内容，还可以直接通过快捷键输入，减少来回切换和重复操作。

- **便捷粘贴**：在小窗口里用方向键挑选内容，随时切换历史与收藏。
- **快捷粘贴 1～9**：给常用条目配置数字键或 F1～F9 组合，按键即可粘贴。
- **多条一起用**：多段文字、图文内容或多个文件可以合并复制、粘贴，省去逐条操作。文件与文本不支持混合合并。
- **随手可用，不挡工作**：支持贴边隐藏、鼠标附近呼出和固定窗口，也能按习惯设置单击、双击与中键操作。

### 常用内容，整理一次反复用

客服回复、常用地址、工作链接和资料片段，可以整理成自己的常用内容库。需要时直接取用，不必每次寻找原文。

- **收藏与置顶**：保存长期使用的内容，把最常用的放在前面，拖拽即可调整顺序。
- **标签分类**：按工作、生活或项目整理内容，一条内容可以放进多个分类，查找更灵活。
- **备注与编辑**：为内容写个容易辨认的说明，也能新增、修改常用文本；已捕获的原始内容仍可恢复。

### 表情、符号和图片素材，一处取用

聊天时找表情、写文档时输入特殊符号、发消息时寻找常用图片，都可以在 QcClip 中完成。

- **表情与符号**：支持分类、搜索和最近使用记录，选中即可粘贴。
- **自己的图库**：把本地或浏览器中的图片拖入分组，集中保存常用图片和表情包，支持 PNG、JPG、GIF、WebP。
- **素材好整理**：按文件名搜索，重命名、移动分组，也能按名称、类型或时间排序。

### 数据自己掌握，外观自己决定

无需登录账号即可使用，剪贴板历史与素材保存在本地。想保留多久、备份到哪里、窗口是什么样，都可以按自己的习惯设置。

- **保留与清理可控**：设置历史保留时间和数量，超出范围的内容先标记为待清理，清理前还能调整设置找回。
- **隐藏失效文件**：在“设置 → 数据管理”开启“自动隐藏失效的图片和文件”后，失效的图片和文件会从历史、收藏和便捷粘贴中隐藏；关闭开关后立即恢复显示。
- **恢复后回到首行**：开启“失效条目恢复后移到首行”后，恢复的未置顶条目会移到主窗口历史和便捷粘贴的首行。
- **方便备份和换设备**：支持数据导出、导入；便携版可连同数据一起移动，安装版也能更改数据位置。
- **减少干扰**：为指定应用暂停内容记录、快捷键或贴边显示，兼顾隐私与使用习惯。
- **喜欢什么样，就调成什么样**：深浅色主题、背景图片、透明度、列表大小和动画都可调整，支持跟随系统外观。
- **日常使用更省心**：支持开机自启、托盘运行、中英文界面和操作音效，并可检查新版本。

## 界面预览

### 剪贴板与标签

<table>
  <tr>
    <td align="center" width="50%">
      <a href="readme-assets/display/剪贴板.png">
        <img src="readme-assets/display/剪贴板.png" alt="剪贴板历史：文字、图片和文件集中浏览" width="360">
      </a>
      <br>
      <strong>剪贴板历史</strong>
      <br>
      文字、图片、文件，随时找回
    </td>
    <td align="center" width="50%">
      <a href="readme-assets/display/标签.png">
        <img src="readme-assets/display/标签.png" alt="彩色标签：按用途整理常用内容" width="360">
      </a>
      <br>
      <strong>彩色标签</strong>
      <br>
      按用途分类，查找更方便
    </td>
  </tr>
</table>

### 表情、符号与图库

<table>
  <tr>
    <td align="center" width="33%">
      <a href="readme-assets/display/Eomji.png">
        <img src="readme-assets/display/Eomji.png" alt="表情库：分类浏览和搜索常用表情" width="240">
      </a>
      <br>
      <strong>表情</strong>
      <br>
      常用表情，随手输入
    </td>
    <td align="center" width="33%">
      <a href="readme-assets/display/符号.png">
        <img src="readme-assets/display/符号.png" alt="符号库：快速查找和输入特殊符号" width="240">
      </a>
      <br>
      <strong>符号</strong>
      <br>
      特殊符号，不必到处找
    </td>
    <td align="center" width="34%">
      <a href="readme-assets/display/图库.png">
        <img src="readme-assets/display/图库.png" alt="分组图库：集中管理图片和表情包" width="240">
      </a>
      <br>
      <strong>图库</strong>
      <br>
      图片素材，分组收藏
    </td>
  </tr>
</table>

### 悬停内容预览

鼠标停在条目上，即可查看内容，少一次打开，多一点便利。

<p align="center">
  <a href="readme-assets/display/悬停内容预览.webp">
    <img src="readme-assets/display/悬停内容预览.webp" alt="悬停预览文字、图片和文件的动态演示" width="960">
  </a>
</p>

### 丰富主题配色

从浅色到深色，选择自己喜欢的配色，让常驻工具也看着舒服。

<p align="center">
  <a href="readme-assets/display/丰富主题配色.webp">
    <img src="readme-assets/display/丰富主题配色.webp" alt="切换主题配色与外观设置的动态演示" width="960">
  </a>
</p>

## 开始使用

前往 [下载页面](https://github.com/mosheng1/QcClip/releases) 选择适合自己的版本：

- **安装版**：适合固定设备日常使用。
- **便携版**：解压后运行 `QcClip/QcClip.exe`，数据自动保存在同目录的 `QcClipData` 中。移动或升级时保留该数据目录。

默认使用 **`Ctrl + Shift + X`** 显示或隐藏主窗口。复制内容后，在窗口中搜索、筛选并选择条目，即可再次复制或粘贴。便捷粘贴及其他快捷键可在“设置 → 快捷键”中配置。

### 从 QuickClipboard 迁移

在“设置 → 数据管理 → 从旧版 QuickClipboard 迁移”中，可检测本地安装版数据、选择旧版数据目录或便携版目录，也可读取旧版导出的 ZIP。

支持迁移旧版收藏，并可选择带入剪贴板历史。原来的分组会变为标签，收藏标题会保存在备注里，已保存的文字、图片和文件列表也可一并迁移。当前以 QuickClipboard **v0.5.0** 的数据为适配依据。

迁移前可以查看哪些内容能够带入，执行时会自动备份 QcClip 当前数据。**旧版图库、设置和同步信息暂不迁移**；文件列表只保留文件位置，不复制实际文件。更多说明见 [旧版数据迁移说明](docs/legacy-migration.md)。

## 反馈与建议

使用中遇到问题，或有希望改进的地方，欢迎在 [问题反馈页面](https://github.com/mosheng1/QcClip/issues) 留言。

## 社区交流

欢迎加入频道或交流群，交流使用体验、提出建议，了解后续开发进展。可以点击加入链接，也可以用 QQ 扫描二维码。

<table>
  <tr>
    <td align="center" width="33%">
      <strong>QcClip 频道</strong>
      <br>
      频道号：pd80680380
      <br><br>
      <a href="assets/pD_1.png">
        <img src="assets/pD_1.png" alt="QcClip 频道二维码，频道号 pd80680380" width="200">
      </a>
      <br>
      <a href="https://pd.qq.com/s/blp3j847c">加入频道</a>
    </td>
    <td align="center" width="33%">
      <strong>QcClip 交流群 1</strong>
      <br>
      群号：725313287
      <br><br>
      <a href="assets/qG_1.png">
        <img src="assets/qG_1.png" alt="QcClip 交流群 1 二维码，群号 725313287" width="200">
      </a>
      <br>
      <a href="https://qm.qq.com/q/nUCO76MX9C">加入交流群 1</a>
    </td>
    <td align="center" width="34%">
      <strong>QcClip 交流群 2</strong>
      <br>
      群号：1033556729
      <br><br>
      <a href="assets/qG_2.png">
        <img src="assets/qG_2.png" alt="QcClip 交流群 2 二维码，群号 1033556729" width="200">
      </a>
      <br>
      <a href="https://qm.qq.com/q/O5zOi3uTuy">加入交流群 2</a>
    </td>
  </tr>
</table>

## 支持开发

觉得 QcClip 好用，可以通过微信赞赏支持后续开发与维护。

<p align="center">
  <a href="assets/wxzsm.png">
    <img src="assets/wxzsm.png" alt="微信赞赏码" width="240">
  </a>
  <br>
  <strong>微信赞赏</strong>
</p>

## 开发与构建

以下为 Windows 下的开发构建方式。需要安装 Rust（建议通过 rustup）、Visual Studio 的“使用 C++ 的桌面开发”工作负载及 Windows SDK。Rust 工具链版本由 `rust-toolchain.toml` 指定。

在项目根目录运行开发版本：

```powershell
cargo run --locked
```

构建发布版本：

```powershell
cargo build --release --locked
```

默认生成的程序位于 `target/release/QcClip.exe`。安装包、便携版及不同架构的打包方法见 [打包说明](docs/packaging.md)。

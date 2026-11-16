## 这是什么

Forked from [QcClip](https://github.com/mosheng1/QcClip) by [@mosheng1.](https://github.com/mosheng1)
以及参考 [CapsLock ](https://capslox.com/capslock-plus/)、[ @wo52616111/capslock-plus](https://github.com/wo52616111/capslock-plus) 

上游是 @mosheng1 用纯 Rust 和 GPUI 写的 Windows 剪贴板管理器，历史记录、搜索、收藏、标签这些功能都做得挺全。我在它的基础上加了一套 CapsLock 组合键剪贴板，别的地方没动。
改动基于上游 v1.0.0-beta.8，本仓库版本号是 v1.0.0-beta.8-capslock.r1。

## 新增的功能：
### 我的改动：
新增Capslock剪切板；Ctrl+C/V、Capslock+C/V 和 Capslock+Alt+C/V共计3套独立剪切板，各自独立复制粘贴，互不干扰。用于快速复制粘贴。 
| 剪贴板 | 复制 / 剪切 | 粘贴 | 存什么 |
| :--- | :--- | :--- | :--- |
| 系统剪贴板 | Ctrl+C / Ctrl+X | Ctrl+V、GUI 里点粘贴 | 完整历史 |
| CapsLock 槽 1 | CapsLock+C / CapsLock+X | CapsLock+V | 只有最近一次 |
| CapsLock 槽 2 | CapsLock+Alt+C / CapsLock+Alt+X | CapsLock+Alt+V | 只有最近一次 |

<img width="1102" height="899" src="https://github.com/user-attachments/assets/6ea511cd-83a9-4c39-b101-43072faa288a" />
<img src="https://github.com/mosheng1/QcClip/blob/main/readme-assets/display/%E5%89%AA%E8%B4%B4%E6%9D%BF.png" />


### 目的：
将Capslock-plus整合进QcClip里； 
 Capslock-plus优点：不想打开GUI，一些非固定高频粘贴的内容，时不时需要粘贴，又容易被Ctrl+V刷新。 
 Capslock-plus缺点：各种签名提权等原因会报毒，被杀软删除；且只用剪切板功能。 

## 编译
需要 rustup 装的 Rust、Visual Studio 的「使用 C++ 的桌面开发」工作负载，以及 Windows SDK。工具链版本由 rust-toolchain.toml 固定。

```
cargo run --locked                  # 开发版
cargo build --release --locked      # 发布版
```

产物是 target/release/QcClip.exe。安装包、便携版和各架构的打包方式见 docs/packaging.md。

## 版权与致谢
- 代码来自 @mosheng1 的 [QcClip](https://github.com/mosheng1/QcClip)，遵循原项目的 Apache License 2.0。
- 上游各文件头的注释和版权声明原样保留，不删不改；新增文件同样用 Apache License 2.0。
- CapsLock 组合键这套交互参考了 [CapsLock](https://capslox.com/capslock-plus/)+ 和 [@wo52616111/capslock-plus](https://github.com/wo52616111/capslock-plus)，只借鉴交互方式，没有搬代码。
- 提交（Commit）和 Pull Request 里会写明上游来源，并 @ 原作者致谢。

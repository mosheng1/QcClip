# QcClip 项目结构与模块边界

QcClip 使用单 crate 组织应用，通过 GPUI 和 gpui-component 构建原生界面。本文说明现有主要模块及维护边界；完整文件列表以源码为准。

## 主要结构

```text
src/
├── main.rs                         # 程序入口
├── app.rs                          # 应用组合根和启动流程
├── app_paths.rs                    # 数据目录与资源路径
├── assets.rs                       # 内置资源与字体
├── config/                         # 配置模型、默认值和字段级加载
├── config_runtime.rs               # 配置保存和运行时更新
├── state_runtime.rs                # 应用与窗口恢复状态
├── i18n.rs                         # 国际化运行时
├── i18n/                           # 中英文界面资源
├── emoji.rs                        # 表情数据、分类与搜索
├── symbols.rs                      # 符号数据、分类与搜索
├── hotkey.rs                       # 快捷键注册、录制与分发
├── logging.rs                      # 日志初始化与目录切换
├── window_material.rs              # 窗口材质与背景
├── domain/                         # 捕获、剪贴板和 HTML 数据模型
├── platform/                       # 剪贴板、窗口、输入和系统能力
├── services/
│   ├── database/                   # SQLite 连接、结构、兼容校验、仓储与失效条目隐藏
│   ├── missing_scan.rs             # 在数据库快照上探测失效图片和文件
│   ├── storage/                    # 原始字节文件写入、读取和校验
│   ├── capture/                    # 捕获哈希、预览和持久化编排
│   ├── sync/                       # 设备身份、配对、LAN 与 WebDAV 事件同步
│   ├── paste.rs                    # 剪贴板恢复、复制和粘贴
│   ├── custom_text.rs              # 自定义文本和原始内容恢复
│   ├── recovery.rs                 # 启动恢复与未完成数据处理
│   ├── data_management.rs          # 数据统计、导入导出、清理和目录迁移
│   ├── data_runtime.rs             # 数据操作与切换的并发协调
│   ├── legacy_migration.rs         # 旧版 QuickClipboard 数据迁移
│   ├── image_library.rs            # 图库分组、导入和文件管理
│   ├── image_preview.rs            # 图片预览与解码
│   ├── application_icon.rs         # 来源应用图标资源与缓存
│   ├── application_filter.rs       # 指定应用的功能禁用规则
│   ├── updates.rs                  # 版本检查、代理和通知状态
│   ├── tray.rs                     # Windows 托盘
│   └── sound.rs                    # 音效播放
├── ui/                             # 主题、图标及复用组件
└── windows/
    ├── main/
    │   ├── state/                  # 列表、选择、搜索、标签和图库状态
    │   ├── components/             # 主窗口专属组件
    │   ├── tabs.rs                 # 标签页与筛选定义
    │   └── view.rs                 # 主窗口布局
    ├── settings/                   # 设置窗口和页面
    ├── menu/                       # 下拉菜单与命令面板
    ├── list_order.rs               # 列表删除后选择最近仍存在行的辅助逻辑
    ├── quick_paste.rs              # 便捷粘贴窗口
    ├── preview.rs                  # 内容预览窗口
    ├── notification.rs             # 通知窗口
    ├── custom_text_editor.rs       # 自定义文本编辑窗口
    ├── note_editor.rs              # 备注编辑窗口
    ├── color_picker.rs             # 颜色选择窗口
    └── dialog.rs                   # 确认对话框
```

仓库中的其他主要目录：

| 目录 | 内容 |
| --- | --- |
| `assets/` | 图标、字体、音效、表情与符号数据等内置资源 |
| `iconsall/` | 界面图标来源，选用后复制到 `assets/icons/` |
| `themes/` | 内置主题 JSON |
| `scripts/` | 打包脚本和更新检查测试服务 |
| `tests/` | 路径、数据运行时、图库和更新检查等集成测试 |
| `vendor/` | 本地依赖及平台补充实现 |
| `i18n/` | 多语言 README，与 `src/i18n/` 的界面翻译区分 |
| `readme-assets/` | README 截图与演示资源 |
| `docs/` | 配置、数据库、迁移、更新和打包说明 |

## 模块职责

### 领域与数据服务

`domain` 保存不依赖 GPUI、SQLite 和具体操作系统的模型及规则。领域模型不需要与数据库表一一对应，SQL 行与领域数据的映射由数据库服务负责。

`services/database` 统一管理 SQLite 连接、外键、WAL、忙等待、结构版本和读写兼容校验。历史、收藏、标签和备注通过仓储公开接口操作；排序、搜索及筛选在查询层完成。`database/missing_files.rs` 持有失效图片/文件隐藏关系、隐藏过滤 SQL 和扫描所需的数据库快照查询；`services/missing_scan.rs` 只负责在快照上探测文件系统。初始化结构见 [schema.sql](../src/services/database/schema.sql)，辅助隐藏表见 [missing_files.sql](../src/services/database/missing_files.sql)，业务关系见 [数据库设计](database-design.md)。

`services/storage` 管理 `data/*.bin` 和 `data/*.bin.zst`，负责临时写入、原子提交、长度与 SHA-256 校验及路径边界检查。数据库中的 `binary_blobs` 保存文件索引和校验信息。

`services/capture` 将平台捕获结果转换为可持久化内容，编排哈希、预览、格式存储和历史去重。内容哈希不包含时间、数据库 ID、UUID、来源应用、排序、标签或备注。`services/paste` 读取可恢复格式并调用平台接口写回剪贴板。

`services/custom_text` 复用格式表和原始字节存储，创建或更新 `qcclip/custom-text`，并维护哈希、预览和标签。编辑不会覆盖已捕获格式；恢复原始内容时删除生成格式并重建派生数据。

`services/sync` 管理独立设备身份和信任列表，编排 LAN 在线投递与 WebDAV 有界缓存，复用捕获事务保存远程内容。接收后返回具体历史、收藏及标签变更，由 `windows/main/state/sync.rs` 合并并局部更新界面。设置页、主窗口标题栏及条目菜单通过同步服务发起操作；架构和维护入口见[同步实现方案](sync-plan.md)，操作与验证见[同步使用说明](sync-guide.md)。

数据导入、清理、位置迁移和旧版迁移由对应服务组织，受管理操作通过 `data_runtime` 协调。旧版迁移的支持范围见 [迁移说明](legacy-migration.md)。

### 应用运行时与平台

配置模型、持久化和设置页分别由 `config`、`config_runtime`、`windows/settings` 管理，恢复状态由 `state_runtime` 单独保存，详见 [配置系统](config-system-reference.md)。

`services` 中的托盘、更新、应用过滤及数据管理等应用服务可以使用 GPUI 全局状态和窗口服务协调生命周期。数据库、原始存储和捕获等数据逻辑保持与窗口布局分离，不把布局和 SQL 混在一起。

`platform` 封装窗口、剪贴板、全局输入、外部拖放、开机自启、权限提升和系统打开等能力。平台捕获返回原始事实，历史去重、收藏、排序和数据库写入由业务服务决定。

当前正式使用目标为 Windows。打包脚本列出 macOS 和 Linux 目标不代表应用已完成这些系统的适配；跨平台支持范围见 [打包说明](packaging.md)。

### 界面与窗口

`ui` 放跨窗口复用的主题、图标、选择控件和滚动等基础能力。只被主窗口使用的组件放在 `windows/main/components`。

窗口负责持有交互状态、调用服务和渲染。主窗口状态按列表、选择、历史、搜索标签和图库等职责拆分在 `windows/main/state/`；独立窗口按实际复杂度使用单个文件或目录，不强制每个窗口创建相同的文件结构。

列表查询返回窗口需要的投影数据，不为渲染列表加载全部原始格式字节或大型图片。主列表使用虚拟列表，自定义文本编辑器复用 gpui-component 的虚拟化编辑能力，文本预览按可见行渲染。

## 启动与生命周期

`main.rs` 调用 `app::run`。`app.rs` 负责组装应用，主要顺序为：

1. 发现数据目录，初始化日志与数据操作协调，并处理待应用的导入。
2. 打开数据库，初始化原始存储并执行启动恢复。
3. 创建捕获、粘贴和仓储服务，启动剪贴板监听。
4. 初始化 GPUI、组件和字体，安装配置与状态运行时。
5. 使用配置初始化语言、主题、音效及相关应用服务。
6. 安装窗口服务并创建主窗口，连接后台事件与界面更新。

服务生命周期由应用运行时管理，列表组件不自行创建或销毁数据库连接。数据导入和位置切换由数据管理服务协调重建相关服务及刷新窗口。

## 维护约束

- UI 与平台适配器通过服务公开接口访问业务数据，不直接打开数据库或拼接 SQL。
- 数据库与原始文件的事务边界由服务层管理，组件不执行拆开的半套保存操作。
- 数据库查询及文件读写等阻塞任务不在 GPUI `render` 中执行。
- 快速切换列表或筛选时，通过查询代次丢弃过期结果，选中项使用稳定 ID。
- 窗口布局依赖状态与投影数据，不在渲染函数中生成模拟业务数据。
- 服务和平台错误通过 `Result` 传递，UI 负责显示可读提示，日志不记录原始剪贴板内容。
- 新模块按实际职责和复用需求拆分，不创建未来功能的空目录，也不按数据库表数量机械拆分。
- 独立状态、交互生命周期或多个调用方形成清晰边界时再增加抽象。

`services/data_management/package` 管理按范围的数据快照、独立密码加密归档、合并/替换和 WebDAV 数据包；`windows/settings/pages/data_packages.rs` 提供统一抽屉交互。包与在线同步缓存独立，规则见[数据转移与备份](data-packages.md)。

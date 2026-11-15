# 数据库设计

## 1. 设计目标

本文说明 QcClip 当前数据库的结构与业务规则，用于保存剪贴板历史、收藏、全局标签、内容备注、原始格式数据及同步接收状态。业务结构见 [schema.sql](../src/services/database/schema.sql)，同步辅助表见 [sync.sql](../src/services/database/sync.sql)，初始化与升级由迁移执行器统一完成。结构版本和读写兼容规则见第 25 节。

设计目标：

- 同一份剪贴板内容在内容实体中只能有一条逻辑记录；
- 同一份内容在历史列表中只能有一条历史关系；
- 收藏使用一个统一的收藏列表；
- 标签是可复用的内容属性，支持一份内容绑定多个标签；
- 收藏不复制剪贴板内容，只保存对内容实体的唯一收藏关系；
- 内容去重哈希只由剪贴板格式标识和原始字节生成，不包含时间或业务状态；
- 捕获到的原始数据保存为不可变字节快照，不转换为 PNG、不规范化 HTML；用户编辑或手动新增文本时，在同一内容的格式集合中保存 `qcclip/custom-text` 生成格式；
- 标签和备注属于内容，历史与收藏共享；标签可以同时用于历史和收藏列表筛选；
- 初版为可同步的持久化记录统一准备本地 `id` 和稳定 `uuid`；
- 内容和原始字节统一保存哈希，供本地及跨设备同步校验和去重；
- 通过外键、唯一约束和索引把关键业务规则固化在数据库中。

## 2. 核心概念和关系

```text
clipboard_items              唯一的剪贴板内容实体
├── clipboard_formats        原始剪贴板格式
│   └── binary_blobs         外部 .bin/.bin.zst 文件索引
├── clipboard_history        历史列表关系
│   └── application_icon_assets  来源应用图标资源
├── favorites                唯一收藏关系
├── clipboard_item_notes     内容备注
└── clipboard_item_tags      内容与标签关系

tags                         标签定义
application_icon_cache       应用路径与图标资源缓存映射
```

### 2.1 内容实体

`clipboard_items` 表代表一份经过哈希去重后的剪贴板内容，而不是某一次复制事件。

同一内容再次被复制时，不创建新的 `clipboard_items`，只更新或创建对应的历史关系。

用户编辑不创建新的用户可见内容实体，也不修改已捕获格式的原始字节。编辑结果作为当前 `clipboard_items` 下的 `qcclip/custom-text` 生成格式保存；同一条自定义文本格式存在时更新它，不存在时创建它。历史关系、收藏关系、内容 ID 和内容 UUID 保持不变。

### 2.2 历史关系

`clipboard_history` 表决定内容是否出现在历史列表中，并保存历史特有的信息，例如首次捕获时间、最近捕获时间、来源应用、粘贴统计和历史列表排序。

历史关系与内容实体分离后，清空历史时可以只删除历史关系，不影响仍被收藏的内容。

### 2.3 收藏关系

`favorites` 表保存一份内容的唯一收藏关系，不保存文本、HTML、图片或其他原始数据的副本。

一个内容实体最多对应一条 `favorites` 记录。所有收藏都在同一个列表中展示，通过 `favorites.is_pinned` 和 `favorites.sort_order` 维护置顶区、普通区及各自的顺序。

“全部收藏”是直接查询 `favorites` 的默认视图，不创建一个特殊的“全部”记录。

### 2.4 内容标签和备注

标签和备注均关联 `clipboard_items`，不依赖历史或收藏关系。

因此：

- 未收藏的历史项也可以拥有标签；
- 取消收藏时只删除收藏关系，不删除内容标签和备注；
- 一份内容可以绑定多个标签，一个标签也可以被多个内容复用；
- 标签和备注不会成为内容去重的一部分。

各部分职责明确区分：

- `clipboard_items.preview_text`：当前默认展示格式的派生缓存，用于列表显示和搜索兜底；
- `clipboard_formats.preview_text`：对应单个真实格式的格式级预览；列表需要保留并读取同一内容的全部格式级预览集合；
- 标签：全局可复用的短文本属性，用于在当前历史或收藏列表中筛选内容；
- 备注：内容的补充说明，每份内容一份多行纯文本备注；
- 收藏列表：所有收藏共用一个列表，置顶区和普通区分别维护排序空间，标签筛选不会创建新的收藏集合；
- 自定义标题：初版不设计。当前预览、标签和备注已经覆盖显示、分类和说明需求，避免再维护一套与内容含义相近的短文本。

如果未来确实需要给收藏设置自定义标题，只需在 `favorites` 增加收藏元数据字段，不会影响 `clipboard_items`、原始数据、UUID 或内容哈希。

### 2.5 自定义文本和原始快照

`clipboard_formats` 同时保存系统捕获格式和 QcClip 生成格式。自定义文本是通用的生成格式，既用于编辑已有内容，也用于手动新增文本内容：

- 格式身份固定为 `format_namespace = 'qcclip'`、`format_key = 'custom-text'`；
- 格式名称为“自定义文本”，`format_kind = 'text'`；
- 文本原始字节使用 UTF-16LE 编码并以 NUL 结尾；
- 生成格式标记为 `capture_state = generated`、`restore_state = restorable`；
- 数据库中的 QcClip 格式写回 Windows 剪贴板时映射为 `CF_UNICODETEXT`，不能要求外部程序识别 `qcclip/custom-text`；
- 同一内容最多保存一条自定义文本格式，重复编辑执行更新，不追加格式记录；
- 原始 HTML、RTF、原始纯文本及其他格式保持不变，仍可通过指定格式粘贴访问；
- 存在自定义文本时，列表预览、普通粘贴和普通复制优先使用它；
- 用户可通过编辑窗口的“恢复原始内容”删除自定义文本格式；恢复后重新选择剩余可恢复格式作为主格式，并重建默认预览、内容哈希和统计字段。

编辑已有内容时，自定义文本属于当前内容的格式集合，不创建新的 `clipboard_items`、历史关系或收藏关系。确认保存会在事务中更新生成格式、标签和派生数据；取消或关闭编辑窗口不写入文本或标签。手动新增文本的流程见第 21.3 节。

## 3. 身份、哈希和通用字段约定

### 3.1 `id`、`uuid` 和哈希的职责

| 字段 | 作用 | 使用范围 |
| --- | --- | --- |
| `id` | SQLite 本地关联和查询的高效整数主键 | 内容、格式、历史、收藏、标签、备注及资源实体表 |
| `uuid` | 跨设备、导入导出和同步使用的稳定记录身份 | 所有需要长期识别的持久化表 |
| `content_hash` | 完整剪贴板内容的去重身份 | `clipboard_items` |
| `data_hash` / `sha256` | 单个原始格式或外部二进制数据的完整性身份 | `clipboard_formats`、`binary_blobs` |

内容、格式、历史、收藏、标签、备注及二进制和图标资源实体均具有本地 `id` 和稳定 `uuid`。`application_icon_cache` 使用应用路径键，`legacy_import_records` 使用源记录指纹，`database_compatibility` 使用固定单行 ID；这些辅助表不生成 UUID。

本地表之间使用 `id` 建立外键。同步使用来源设备 ID 与内容 UUID 定位来源映射；没有映射且载荷完整或为重复触达时，再按版本化内容哈希匹配。标签按名称复用，不依赖两端本地 ID 一致。

### 3.2 字段约定

| 约定 | 设计 |
| --- | --- |
| 主键 | `id INTEGER PRIMARY KEY`，由 SQLite 生成 |
| UUID | `TEXT NOT NULL UNIQUE`，由应用生成，使用标准 UUID 字符串格式 |
| 时间 | UTC Unix 毫秒时间戳，SQLite 类型为 `INTEGER` |
| 布尔值 | `INTEGER NOT NULL`，只允许 `0` 或 `1` |
| 哈希 | 使用 SQLite `BLOB` 保存原始 32 字节 SHA-256，不保存带分隔符的十六进制文本 |
| 路径 | 只保存相对于应用数据目录的路径，禁止保存绝对路径 |
| 原始数据 | 捕获格式保存系统返回的原始字节；自定义文本保存格式编码器生成的新原始字节，不修改其他格式，也不保存与事实不一致的替代副本 |
| 派生数据 | 预览文本、字符数、内容类型属于可重建缓存，不是内容事实来源 |
| 外键 | 每次连接都必须执行 `PRAGMA foreign_keys = ON` |
| 时间写入 | 由应用层统一生成，避免依赖不同 SQLite 版本的时间函数 |
| 数据库版本 | `PRAGMA user_version` 记录结构版本，`database_compatibility` 独立声明最低读写兼容版本 |

## 4. 表清单

| 表 | 作用 |
| --- | --- |
| `clipboard_items` | 唯一剪贴板内容实体和内容去重身份 |
| `binary_blobs` | 外部 `.bin`/`.bin.zst` 原始字节文件索引 |
| `application_icon_assets` | 来源应用图标 PNG 资源和内容哈希 |
| `application_icon_cache` | 应用路径文件签名与图标资源的缓存映射 |
| `clipboard_formats` | 内容包含的每个真实剪贴板格式 |
| `clipboard_history` | 历史列表关系、历史排序、最近捕获信息和来源应用筛选字段 |
| `favorites` | 内容与收藏功能之间的唯一关系 |
| `tags` | 标签定义 |
| `clipboard_item_tags` | 内容与标签的多对多关系 |
| `clipboard_item_notes` | 历史与收藏共享的内容备注 |
| `hidden_missing_items` | 已确认失效的图片/文件内容；可重建缓存，随内容删除级联清理 |
| `database_compatibility` | 最低读取和写入协议版本声明 |
| `legacy_import_records` | 旧版迁移的源记录指纹、内容关联与导入时间 |
| `sync_items` | 来源设备与内容 UUID 到本机内容的映射，以及来源和本地哈希 |
| `sync_watermarks` | 每个来源设备、内容 UUID、状态流已接收的最大序号 |
| `sync_receipts` | 短期事件回执，用于阻止相同事件重复提交 |
| `sync_sources` | 内容的同步来源：发送设备 ID、名称快照及来源应用名称，不保存远端应用路径或图标 |
| `sync_local_activity` | 最近一次本机新增操作及其历史或收藏去向，固定单槽位 |

当前不创建复制内容的独立收藏表，也不创建收藏专属标签关系表。

## 5. `clipboard_items`：内容实体表

### 5.1 字段

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `id` | `INTEGER` | 否 | SQLite | 主键 | 内容实体的本地内部 ID |
| `uuid` | `TEXT` | 否 | 无 | `UNIQUE` | 内容实体的稳定 UUID，不参与内容哈希 |
| `origin_kind` | `TEXT` | 否 | `captured` | 枚举约束 | `captured`、`manual`、`edited`、`imported`、`synced`；标识内容来源，不参与哈希 |
| `parent_item_id` | `INTEGER` | 是 | `NULL` | 自引用外键 | 导入、同步或其他需要保留来源时使用；自定义文本编辑不创建父内容实体 |
| `hash_version` | `INTEGER` | 否 | 无 | `> 0` | 内容哈希算法版本 |
| `content_hash` | `BLOB` | 否 | 无 | 长度必须为 32，唯一 | 完整剪贴板内容的 SHA-256 |
| `content_kind` | `TEXT` | 否 | 无 | 枚举约束 | 所有格式的聚合摘要：`text`、`link`、`image`、`file_list`、`mixed`、`custom`、`unknown`；不能替代 `clipboard_formats` |
| `capture_state` | `TEXT` | 否 | `pending` | 枚举约束 | `pending`、`ready`、`corrupt`；只有 `ready` 的内容进入列表 |
| `preview_text` | `TEXT` | 是 | `NULL` | 无 | 用于列表显示和搜索的派生预览文本 |
| `preview_char_count` | `INTEGER` | 是 | `NULL` | `>= 0` | 预览文本的字符数，不是原始字节长度 |
| `format_count` | `INTEGER` | 否 | `0` | `>= 0` | 捕获到的格式记录数量，包括成功和失败的格式记录；内容为 `ready` 时必须大于 0 |
| `total_bytes` | `INTEGER` | 否 | `0` | `>= 0` | 成功捕获的原始格式总字节数 |
| `created_at` | `INTEGER` | 否 | 无 | 无 | 第一次创建内容实体的时间 |
| `updated_at` | `INTEGER` | 否 | 无 | 无 | 派生缓存或内容元数据最后更新时间 |

### 5.2 约束和索引

```sql
CREATE UNIQUE INDEX ux_clipboard_items_hash
ON clipboard_items(hash_version, content_hash);

CREATE INDEX ix_clipboard_items_kind
ON clipboard_items(content_kind);
```

`clipboard_items` 创建时先处于 `pending` 状态。原始格式和外部文件全部完成后，必须在同一事务中校验并切换为 `ready`；捕获中途崩溃留下的 `pending` 记录不能展示给用户，启动恢复任务负责清理或重新处理。

`content_hash` 不得包含以下字段：

- 时间；
- 数据库 ID；
- UUID；
- 来源应用；
- 粘贴次数；
- 置顶状态；
- 收藏状态；
- 标签和备注；
- 历史或收藏排序值。

## 6. `binary_blobs`：外部原始数据索引表

大型格式、图片和其他不适合内联保存的原始字节写入 `data/<sha256>.bin` 或
`data/<sha256>.bin.zst`，数据库只保存文件索引。`.bin.zst` 使用 Zstandard
级别 3 无损压缩，仅在压缩后确实更小时使用。

### 6.1 字段

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `id` | `INTEGER` | 否 | SQLite | 主键 | 外部数据索引 ID |
| `uuid` | `TEXT` | 否 | 无 | `UNIQUE` | 外部数据索引的稳定 UUID |
| `sha256` | `BLOB` | 否 | 无 | 长度必须为 32，唯一 | 解压后原始字节的 SHA-256 |
| `byte_length` | `INTEGER` | 否 | 无 | `>= 0` | 解压后原始字节数 |
| `relative_path` | `TEXT` | 否 | 无 | `UNIQUE` | 相对于应用数据目录的路径，例如 `data/<sha256>.bin` 或 `data/<sha256>.bin.zst` |
| `storage_state` | `TEXT` | 否 | `pending` | 枚举约束 | `pending`、`ready`、`missing`、`corrupt` |
| `last_error` | `TEXT` | 是 | `NULL` | 无 | 文件写入或校验失败时的错误信息 |
| `created_at` | `INTEGER` | 否 | 无 | 无 | 索引创建时间 |
| `last_verified_at` | `INTEGER` | 是 | `NULL` | 无 | 最近一次校验文件存在且哈希正确的时间 |

### 6.2 存储规则

- `relative_path` 不得包含绝对路径、盘符或路径穿越片段；
- `.bin` 保存未压缩原始字节，`.bin.zst` 保存 Zstandard 压缩字节，格式由文件后缀确定；
- 文件写入应先写入临时文件，解压校验原始长度和 SHA-256 后再原子重命名为目标文件；
- 只有文件完成写入并通过校验后，`storage_state` 才能变为 `ready`；
- `binary_blobs` 可能被多个格式记录共享，不能因为删除一条格式记录就直接删除文件；
- 删除文件前必须确认没有任何 `clipboard_formats.blob_id` 引用它。

## 6.3 `application_icon_assets` 和 `application_icon_cache`：来源应用图标缓存

应用图标是可重建的 UI 缓存，不属于剪贴板原始事实。固定提取为 32×32 PNG 后，使用完整
SHA-256 作为内容身份，文件保存到 `data/app-icons/<sha256>.png`。

`application_icon_assets` 保存图标资源本身：

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `id` | `INTEGER` | 否 | SQLite | 主键 | 图标资源 ID |
| `uuid` | `TEXT` | 否 | 无 | `UNIQUE` | 图标资源稳定 UUID |
| `sha256` | `BLOB` | 否 | 无 | 长度必须为 32，唯一 | PNG 字节的完整 SHA-256 |
| `relative_path` | `TEXT` | 否 | 无 | `UNIQUE` | `data/app-icons/<sha256>.png` |
| `byte_length` | `INTEGER` | 否 | 无 | `> 0` | PNG 字节长度 |
| `width` / `height` | `INTEGER` | 否 | 无 | `> 0` | 图标像素尺寸 |
| `created_at` | `INTEGER` | 否 | 无 | 无 | 首次保存时间 |
| `last_used_at` | `INTEGER` | 否 | 无 | 无 | 最近关联历史记录的时间 |

`application_icon_cache` 保存应用文件与图标资源的映射：

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `app_key` | `TEXT` | 否 | 无 | 主键 | 规范化后的应用路径 |
| `app_path` | `TEXT` | 否 | 无 | 无 | 原始应用路径 |
| `file_size` | `INTEGER` | 否 | 无 | `>= 0` | 应用文件大小 |
| `modified_at` | `INTEGER` | 否 | 无 | `>= 0` | 应用文件修改时间 |
| `icon_asset_id` | `INTEGER` | 否 | 无 | 外键 | 关联 `application_icon_assets.id` |
| `last_checked_at` | `INTEGER` | 否 | 无 | 无 | 最近检查时间 |

捕获来源应用时，先根据应用路径和文件签名复用缓存；缓存失效后重新提取图标。图标文件
缺失或校验失败时允许重建，不能阻止剪贴板内容保存。

## 7. `clipboard_formats`：原始格式表

每条记录代表内容可用的一个剪贴板格式。它可以是系统剪贴板捕获到的真实格式，也可以是 QcClip 生成的自定义文本格式。记录可以内联保存原始字节，也可以关联外部 `.bin` 或 `.bin.zst` 文件。格式级预览与该格式一一对应，不能把同一内容的多个格式压缩成一条预览。

### 7.1 字段

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `id` | `INTEGER` | 否 | SQLite | 主键 | 原始格式记录 ID |
| `uuid` | `TEXT` | 否 | 无 | `UNIQUE` | 原始格式记录的稳定 UUID |
| `item_id` | `INTEGER` | 否 | 无 | 外键 | 关联 `clipboard_items.id` |
| `format_order` | `INTEGER` | 否 | 无 | `>= 0` | 捕获格式的原始顺序，粘贴时用于选择优先级 |
| `format_namespace` | `TEXT` | 否 | 无 | 非空 | 格式名称的来源命名空间，例如 `windows`、`mime`、`application` |
| `format_key` | `TEXT` | 否 | 无 | 与 `format_namespace` 组合后在同一内容内唯一 | 用于识别格式的稳定键 |
| `format_name` | `TEXT` | 否 | 无 | 非空 | 系统返回的原始格式名称 |
| `native_format_id` | `INTEGER` | 是 | `NULL` | `>= 0` | 平台原生格式 ID，例如 Windows 剪贴板格式 ID |
| `format_kind` | `TEXT` | 否 | 无 | 枚举约束 | `text`、`html`、`rtf`、`image`、`file_list`、`custom`、`unknown` |
| `is_primary` | `INTEGER` | 否 | `0` | `0/1` | 是否为列表预览和默认粘贴使用的主格式 |
| `storage_kind` | `TEXT` | 否 | `none` | 枚举约束 | `inline`、`external`、`none` |
| `inline_data` | `BLOB` | 是 | `NULL` | 与存储类型一致 | 小型原始字节；`storage_kind = inline` 时使用 |
| `blob_id` | `INTEGER` | 是 | `NULL` | 外键 | 关联 `binary_blobs.id`；`storage_kind = external` 时使用 |
| `byte_length` | `INTEGER` | 否 | `0` | `>= 0` | 原始字节长度 |
| `data_hash` | `BLOB` | 是 | `NULL` | 捕获成功时长度为 32 | 当前格式原始字节的 SHA-256 |
| `restore_state` | `TEXT` | 否 | `pending` | 枚举约束 | `pending`、`restorable`、`not_restorable`、`not_applicable` |
| `restore_error_code` | `TEXT` | 是 | `NULL` | 无 | 无法恢复时的稳定错误代码 |
| `restore_error_message` | `TEXT` | 是 | `NULL` | 无 | 面向诊断的恢复错误信息 |
| `mime_type` | `TEXT` | 是 | `NULL` | 无 | 可推断的 MIME 类型，不作为原始数据来源 |
| `encoding` | `TEXT` | 是 | `NULL` | 无 | 文本编码提示，例如 `utf-8` |
| `preview_text` | `TEXT` | 是 | `NULL` | 与其他格式级预览字段成组 | 当前格式的派生预览文本；不是原始格式事实 |
| `preview_char_count` | `INTEGER` | 是 | `NULL` | `>= 0`，与格式级预览成组 | 当前格式预览的用户感知字符数 |
| `preview_content_kind` | `TEXT` | 是 | `NULL` | 枚举约束，与格式级预览成组 | 当前格式预览对应的语义类型 |
| `capture_state` | `TEXT` | 否 | 无 | 枚举约束 | `captured`、`generated`、`unsupported`、`failed` |
| `error_code` | `TEXT` | 是 | `NULL` | 无 | 捕获失败或不支持时的稳定错误代码 |
| `error_message` | `TEXT` | 是 | `NULL` | 无 | 面向诊断的错误信息 |
| `created_at` | `INTEGER` | 否 | 无 | 无 | 格式记录创建时间 |

### 7.2 存储一致性规则

```sql
CHECK (
    (storage_kind = 'inline' AND inline_data IS NOT NULL AND blob_id IS NULL)
    OR
    (storage_kind = 'external' AND inline_data IS NULL AND blob_id IS NOT NULL)
    OR
    (storage_kind = 'none' AND inline_data IS NULL AND blob_id IS NULL)
)
```

捕获状态和原始数据的关系：

- `captured`：从系统剪贴板获得原始数据；
- `generated`：由自定义文本编辑器或其他内容创建流程按平台格式规则生成原始数据；必须有原始数据，`storage_kind` 只能是 `inline` 或 `external`；
- `unsupported`：平台无法捕获该格式，不能伪造数据，`storage_kind` 必须是 `none`；
- `failed`：捕获过程中发生错误，`storage_kind` 必须是 `none`；
- `captured` 和 `generated` 的格式都必须经过恢复能力判断，`restore_state` 可以是 `pending`、`restorable` 或 `not_restorable`；
- `unsupported` 和 `failed` 的格式必须使用 `restore_state = not_applicable`；
- `is_primary = 1` 只能设置在 `capture_state IN (captured, generated) AND restore_state = restorable` 的格式上；
- `preview_text`、`preview_char_count` 和 `preview_content_kind` 必须全部为空，或全部存在；
- 去重哈希使用 `capture_state IN ('captured', 'generated')` 的格式及其原始字节；
- 一个内容至少要有一个可用的 `captured` 或 `generated` 格式，否则不能切换为 `ready`。

### 7.3 约束和索引

```sql
CREATE UNIQUE INDEX ux_clipboard_formats_item_order
ON clipboard_formats(item_id, format_order);

CREATE UNIQUE INDEX ux_clipboard_formats_item_key
ON clipboard_formats(item_id, format_namespace, format_key);

CREATE UNIQUE INDEX ux_clipboard_formats_primary
ON clipboard_formats(item_id)
WHERE is_primary = 1;

CREATE INDEX ix_clipboard_formats_blob
ON clipboard_formats(blob_id);

CREATE INDEX ix_clipboard_formats_item_content_kind
ON clipboard_formats(item_id, preview_content_kind);
```

## 8. `clipboard_history`：历史关系表

一条内容最多对应一条历史关系。历史关系可以被删除，而不会自动删除仍被收藏的内容实体。

### 8.1 字段

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `id` | `INTEGER` | 否 | SQLite | 主键 | 历史关系 ID |
| `uuid` | `TEXT` | 否 | 无 | `UNIQUE` | 历史关系的稳定 UUID |
| `item_id` | `INTEGER` | 否 | 无 | 外键、唯一 | 关联 `clipboard_items.id` |
| `first_seen_at` | `INTEGER` | 否 | 无 | 无 | 第一次捕获该内容的时间 |
| `last_seen_at` | `INTEGER` | 否 | 无 | `>= first_seen_at` | 最近一次捕获该内容的时间 |
| `capture_count` | `INTEGER` | 否 | `1` | `>= 0` | 本机捕获次数；仅远程接收的历史初始为 0，远程接收不增加此计数 |
| `paste_count` | `INTEGER` | 否 | `0` | `>= 0` | 从历史项粘贴的次数 |
| `last_pasted_at` | `INTEGER` | 是 | `NULL` | 无 | 从历史项最近一次粘贴的时间 |
| `is_pinned` | `INTEGER` | 否 | `0` | `0/1` | 是否置顶在历史列表中 |
| `sort_order` | `INTEGER` | 否 | 无 | `>= 0` | 当前历史排序分区内的顺序值 |
| `last_source_app_name` | `TEXT` | 是 | `NULL` | 无 | 最近一次捕获来源应用名称 |
| `last_source_app_path` | `TEXT` | 是 | `NULL` | 无 | 最近一次捕获来源应用路径 |
| `last_source_app_icon_id` | `INTEGER` | 是 | `NULL` | 外键 | 最近一次捕获来源应用的图标资源 |
| `updated_at` | `INTEGER` | 否 | 无 | 无 | 历史关系最后更新时间 |

来源应用图标属于可重建的 UI 缓存，不是剪贴板事实数据。`application_icon_assets` 按固定尺寸 PNG 的完整 SHA-256 去重，文件保存到独立的 `data/app-icons/<sha256>.png`；`application_icon_cache` 按规范化应用路径、文件大小和修改时间复用图标提取结果。历史关系只保存 `last_source_app_icon_id`，不把图标字节写入剪贴板原始格式表。

### 8.2 排序规则

普通项和置顶项使用同一个字段，但排序分区完全独立：

```sql
CREATE UNIQUE INDEX ux_clipboard_history_partition_order
ON clipboard_history(is_pinned, sort_order);
```

列表查询：

```sql
SELECT ...
FROM clipboard_history AS h
JOIN clipboard_items AS i ON i.id = h.item_id
ORDER BY h.is_pinned DESC, h.sort_order DESC, h.last_seen_at DESC, h.id DESC;
```

置顶、取消置顶和重复捕获移动到顶部时，必须在事务中按照目标 `is_pinned` 分区重新分配 `sort_order`。

### 8.3 历史保留和待清理

`config.json` 的 `history.retentionDays` 和 `history.maxItems` 共同定义普通历史的可见范围：时间限制要求 `last_seen_at` 在截止时间内；条数限制从全部普通历史中按列表顺序 `sort_order DESC, last_seen_at DESC, id DESC` 选出前 N 条；两项同时设置时取两者交集。置顶历史始终豁免这两个限制。

超出范围的历史关系暂时保留在 `clipboard_history` 中，列表、分页、搜索和筛选查询都排除这些待清理条目。扩大范围后，未被显式清理的条目会重新进入历史列表。收藏独立查询 `favorites`，所以超出历史范围的收藏仍显示在收藏页，不会因此重新显示在历史页。

用户主动执行“清理待清理历史”时才会删除超出当前范围的历史关系；随后只回收不再被历史或收藏引用的内容及其原始数据。收藏引用仍存在的内容和收藏关系不会被删除。

## 9. `favorites`：唯一收藏关系表

一份内容只创建一条收藏记录。收藏记录是内容实体和收藏功能之间的关系，同时保存收藏自身的统计属性。收藏置顶状态只作用于收藏列表，不继承历史关系的置顶状态。

### 9.1 字段

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `id` | `INTEGER` | 否 | SQLite | 主键 | 收藏关系的本地 ID |
| `uuid` | `TEXT` | 否 | 无 | `UNIQUE` | 收藏关系的稳定 UUID |
| `item_id` | `INTEGER` | 否 | 无 | 外键、唯一 | 关联 `clipboard_items.id`，保证同一内容只有一份收藏 |
| `is_pinned` | `INTEGER` | 否 | `0` | `0/1` | 是否置顶在收藏列表中 |
| `sort_order` | `INTEGER` | 否 | 无 | `>= 0` | “全部收藏”视图中的独立排序值 |
| `paste_count` | `INTEGER` | 否 | `0` | `>= 0` | 从收藏项粘贴的次数，与历史粘贴次数分开统计 |
| `last_pasted_at` | `INTEGER` | 是 | `NULL` | 无 | 从收藏项最近一次粘贴的时间 |
| `added_at` | `INTEGER` | 否 | 无 | 无 | 加入收藏的时间 |
| `updated_at` | `INTEGER` | 否 | 无 | 无 | 收藏关系最后更新时间 |

### 9.2 约束和索引

```sql
CREATE UNIQUE INDEX ux_favorites_item
ON favorites(item_id);

CREATE UNIQUE INDEX ux_favorites_partition_order
ON favorites(is_pinned, sort_order);

CREATE INDEX ix_favorites_list
ON favorites(is_pinned DESC, sort_order DESC, added_at DESC, id DESC);
```

取消收藏只删除 `favorites` 关系及其收藏元数据，不删除 `clipboard_items`。如果内容同时存在历史中，历史记录完全不受影响。

## 10. `tags`：标签定义表

标签定义可以被多个内容复用，通过 `clipboard_item_tags` 关联内容。

### 10.1 字段

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `id` | `INTEGER` | 否 | SQLite | 主键 | 标签 ID |
| `uuid` | `TEXT` | 否 | 无 | `UNIQUE` | 标签稳定 UUID |
| `name` | `TEXT` | 否 | 无 | 非空，长度不超过 100 | 用户看到的标签名称 |
| `normalized_name` | `TEXT` | 否 | 无 | 非空、长度不超过 100、`UNIQUE` | 应用层规范化后的名称，用于去重 |
| `color` | `TEXT` | 否 | `#1677FF` | `#RRGGBB` 格式 | 标签颜色，例如 `#6B7280` |
| `sort_order` | `INTEGER` | 否 | `0` | `>= 0`、`UNIQUE` | 标签面板中的持久化顺序值，数值越大越靠前 |
| `created_at` | `INTEGER` | 否 | 无 | 无 | 创建时间 |
| `updated_at` | `INTEGER` | 否 | 无 | 无 | 最后更新时间 |

`normalized_name` 由应用层生成：先去除前后空格，再执行 Rust 的 Unicode 小写转换。首版不额外引入 Unicode NFKC 或大小写折叠规则；SQLite 的 `LOWER()` 不足以覆盖所有 Unicode 情况，不能只依赖数据库函数。数据库同时保证该字段非空且长度不超过 100。

`sort_order` 由标签仓储维护。标签面板按 `sort_order DESC, id DESC` 查询；拖拽重排时在事务中重新分配全部标签的连续顺序值，避免唯一约束冲突。

## 11. `clipboard_item_tags`：内容标签关系表

### 11.1 字段

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `id` | `INTEGER` | 否 | SQLite | 主键 | 内容标签关系本地 ID |
| `uuid` | `TEXT` | 否 | 无 | `UNIQUE` | 内容标签关系稳定 UUID |
| `item_id` | `INTEGER` | 否 | 无 | 外键 | 关联 `clipboard_items.id` |
| `tag_id` | `INTEGER` | 否 | 无 | 外键 | 关联 `tags.id` |
| `created_at` | `INTEGER` | 否 | 无 | 无 | 添加标签的时间 |
| `updated_at` | `INTEGER` | 否 | 无 | 无 | 内容标签关系最后更新时间 |

约束：

```sql
UNIQUE(item_id, tag_id)
```

同一标签不能重复添加到同一内容，但可以添加到多个内容。

## 12. `clipboard_item_notes`：内容备注表

备注通过 `item_id` 关联内容实体，历史和收藏共享同一份备注。`UNIQUE(item_id)` 保证每份内容最多一份备注；清空或保存纯空白文本时删除备注记录，保留内容本身。

### 12.1 字段

| 字段 | SQLite 类型 | 可空 | 默认值 | 约束 | 说明 |
| --- | --- | --- | --- | --- | --- |
| `id` | `INTEGER` | 否 | SQLite | 主键 | 备注 ID |
| `uuid` | `TEXT` | 否 | 无 | `UNIQUE` | 备注稳定 UUID |
| `item_id` | `INTEGER` | 否 | 无 | 外键、`UNIQUE` | 关联 `clipboard_items.id` |
| `note_text` | `TEXT` | 否 | 无 | 去除空白后不能为空 | 备注内容 |
| `created_at` | `INTEGER` | 否 | 无 | 无 | 创建时间 |
| `updated_at` | `INTEGER` | 否 | 无 | 无 | 最后更新时间 |

按内容查询使用 `UNIQUE(item_id)` 的索引，无需额外索引。更新备注保留原 `id`、`uuid` 和创建时间；备注不参与去重和复制粘贴，搜索同时匹配备注与内容。

列表默认显示一行备注和内容摘要；极小行高有备注时只显示备注。`appearance.onlyShowNotes` 开启后，其他行高也只显示备注，无备注时继续显示内容。悬停预览保留原内容，并在顶部显示可滚动的备注；右键菜单和悬停工具栏都提供备注编辑入口。

## 13. 外键删除策略

所有数据库连接必须开启外键：

```sql
PRAGMA foreign_keys = ON;
```

| 父表 | 子表 | 删除策略 | 原因 |
| --- | --- | --- | --- |
| `clipboard_items` | `clipboard_formats` | `CASCADE` | 删除内容时删除其格式索引 |
| `clipboard_items` | `clipboard_history` | `CASCADE` | 删除内容时删除历史关系 |
| `clipboard_items` | `favorites` | `RESTRICT` | 防止误删仍被收藏的内容 |
| `binary_blobs` | `clipboard_formats` | `RESTRICT` | 防止删除仍被引用的原始数据文件索引 |
| `clipboard_items` | `clipboard_item_tags` | `CASCADE` | 删除内容时删除标签关系 |
| `clipboard_items` | `clipboard_item_notes` | `CASCADE` | 内容被最终删除时删除备注 |
| `clipboard_items` | `hidden_missing_items` | `CASCADE` | 删除内容时删除隐藏关系 |
| `tags` | `clipboard_item_tags` | `CASCADE` | 删除标签时删除关联关系 |
| `clipboard_items` | `legacy_import_records` | `SET NULL` | 删除内容后保留迁移指纹，重复迁移不恢复已删除记录 |

历史清理不能直接删除 `clipboard_items`。正确流程是：

1. 删除 `clipboard_history` 关系；
2. 保留仍被 `favorites` 引用的内容；
3. 对没有历史和收藏引用的内容执行垃圾回收；
4. 删除内容前级联删除 `clipboard_formats`；
5. 对没有任何格式引用的 `binary_blobs` 删除对应 `.bin`/`.bin.zst` 文件和索引。

取消收藏只删除 `favorites`，保留内容标签和备注；清空历史不会影响仍被收藏的内容、标签和备注。没有历史和收藏引用的内容在垃圾回收时连同备注一起删除。

## 14. 内容哈希和去重算法

### 14.1 哈希输入

哈希版本 `1` 使用以下逻辑生成 `content_hash`。当前版本定义的是“原始格式快照去重”，不是“用户可见语义去重”：相同文本如果来自不同应用并携带不同原始格式，允许被识别为不同内容。

1. 捕获并等待所有延迟格式稳定；
2. 只取 `capture_state IN ('captured', 'generated')` 的格式；
3. 按格式身份排序，不使用系统枚举顺序；
4. 对每个格式写入格式身份和原始字节的长度前缀；
5. 对完整的规范化字节流计算 SHA-256。

规范化输入至少包含：

```text
hash_version
format_namespace 的字节长度 + format_namespace
format_key 的字节长度 + format_key
format_name 的字节长度 + format_name
native_format_id 是否存在 + native_format_id
原始字节长度 + 原始字节
```

长度前缀用于避免不同字段拼接后产生歧义。`format_namespace` 用于避免不同平台或应用的同名格式相互混淆。排序只用于计算哈希，`format_order` 仍然按照系统捕获顺序保存，用于后续粘贴和格式选择。

哈希算法升级时不能只提高 `hash_version` 后继续写入，否则 `(hash_version, content_hash)` 只保证同一版本内唯一，跨版本仍可能出现重复内容。升级流程必须先扫描现有原始格式，完成重算、冲突合并和关系重映射，再启用新版本。

### 14.2 重复捕获处理

捕获、哈希和历史更新必须在清晰的事务边界内完成：

1. 捕获所有可用原始格式；
2. 等待延迟格式稳定；
3. 计算不包含时间的内容哈希；
4. 根据 `(hash_version, content_hash)` 查询或创建 `clipboard_items`；
5. 新内容写入 `clipboard_formats` 和必要的 `binary_blobs`；
6. 已存在内容不覆盖原始格式数据，除非明确执行原始数据修复流程；
7. 创建或更新唯一的 `clipboard_history`；
8. 更新 `last_seen_at`、来源应用、捕获次数和历史排序；
9. 如果内容已经收藏，不重复创建 `favorites`。

收藏状态、置顶状态、标签、备注、排序值和时间都不参与内容去重。

## 15. 收藏和标签筛选流程

### 15.1 从历史收藏到统一收藏列表

操作流程：

1. 从历史项通过 `clipboard_history.item_id` 找到 `clipboard_items.id`；
2. 如果不存在 `favorites(item_id)`，创建一条唯一收藏关系；
3. 收藏项进入统一的收藏列表，并按 `favorites.is_pinned`、`favorites.sort_order` 排序；
4. 标签通过内容项编辑或标签管理流程单独添加，不参与内容去重和收藏创建。

一份内容只对应一条收藏关系。标签只增加 `clipboard_item_tags` 关系，不复制收藏内容，也不创建新的收藏列表。

### 15.2 列表查询

全部收藏：

```sql
SELECT ...
FROM favorites AS f
JOIN clipboard_items AS i ON i.id = f.item_id
ORDER BY f.is_pinned DESC, f.sort_order DESC, f.added_at DESC, f.id DESC;
```

历史列表按标签筛选：

```sql
SELECT ...
FROM clipboard_history AS h
JOIN clipboard_items AS i ON i.id = h.item_id
WHERE EXISTS (
    SELECT 1
    FROM clipboard_item_tags AS cit
    WHERE cit.item_id = i.id
      AND cit.tag_id = ?1
)
ORDER BY h.is_pinned DESC, h.sort_order DESC, h.last_seen_at DESC, h.id DESC;
```

收藏列表按标签筛选：

```sql
SELECT ...
FROM favorites AS f
JOIN clipboard_items AS i ON i.id = f.item_id
WHERE EXISTS (
    SELECT 1
    FROM clipboard_item_tags AS cit
    WHERE cit.item_id = i.id
      AND cit.tag_id = ?1
)
ORDER BY f.is_pinned DESC, f.sort_order DESC, f.added_at DESC, f.id DESC;
```

未选择标签时显示当前列表的全部内容。默认多个标签使用 OR 语义，匹配任意选中标签；切换为 AND 后，每个选中的标签都必须满足。标签筛选与搜索、内容类型和粘贴状态等其他条件始终使用 AND 组合，不会切换当前列表。

### 15.3 标签删除和收藏取消

- 删除标签：删除 `tags`，级联删除 `clipboard_item_tags`，不删除内容；
- 从内容移除标签：只删除一条 `clipboard_item_tags` 关系；
- 取消收藏：删除 `favorites`，不影响内容标签和备注；
- 清空历史：只删除 `clipboard_history`，不影响仍被收藏的内容和内容标签；
- 永久删除内容：必须确认没有历史关系和收藏关系后才能删除 `clipboard_items`。

## 16. 设计取舍

主要业务能力与对应结构如下：

| 能力 | 当前设计 |
| --- | --- |
| 收藏列表 | 所有收藏共用 `favorites` 列表，`is_pinned` 分区后分别使用 `sort_order` 排序 |
| 收藏内容 | 只保存 `favorites.item_id`，不复制文本、HTML、图片和原始格式 |
| 收藏显示和搜索 | 默认使用 `clipboard_items.preview_text`，完整格式候选来自 `clipboard_formats.preview_*`，都由原始格式派生并可重建 |
| 内容分类和说明 | 使用全局 `clipboard_item_tags` 与 `clipboard_item_notes`，不增加重复标题字段 |
| 标签筛选 | `clipboard_item_tags` 支持历史和收藏列表的复用及多标签组合筛选 |
| 收藏粘贴统计 | `favorites.paste_count`、`favorites.last_pasted_at`，与历史统计分离 |
| 历史粘贴统计 | `clipboard_history.paste_count`、`clipboard_history.last_pasted_at` |
| 原始格式 | `clipboard_formats` 直接关联内容实体，有真正外键 |
| 大型原始数据 | `binary_blobs` + 相对路径 `.bin`/`.bin.zst`，只做无损压缩，不转换图片格式 |
| 内容去重 | 数据库唯一约束固化 `(hash_version, content_hash)`，不局限最近若干条 |
| 排序 | 历史和全部收藏分别维护置顶分区及排序值；标签筛选复用当前列表排序 |
| 稳定身份 | 业务实体使用 `id + uuid`，内容和二进制数据使用哈希；辅助表使用各自的本地键 |

## 17. 初始化结构

业务建表 SQL、索引和触发器维护在 [schema.sql](../src/services/database/schema.sql)，同步辅助表维护在 [sync.sql](../src/services/database/sync.sql)，失效条目隐藏表维护在 [missing_files.sql](../src/services/database/missing_files.sql)，文档不另行维护初始化脚本。数据库服务通过 [migrations.rs](../src/services/database/migrations.rs) 在事务中执行初始化或升级，并核验结构与写入约束。

当前结构版本为 **2**，空库初始化时创建业务表、应用图标缓存、兼容声明、旧版迁移记录、同步辅助表及失效条目隐藏表。已发布 schema 1 在事务中升级到 schema 2，放宽历史捕获次数约束以允许远程接收为 0，并补充同步表。`PRAGMA user_version` 记录结构版本，当前最低读取协议和最低写入协议均为 1。

连接级 PRAGMA 由数据库连接模块设置；不要只执行建表 SQL 来替代应用的连接初始化和结构核验。

## 18. 同步支持边界

当前支持 LAN 与 WebDAV 的事件投递，不维护完整数据库一致性，不传播删除、取消收藏、置顶或排序。数据库为业务实体保留稳定身份：

- 内容、格式、历史、收藏、标签、备注及资源实体具有本地 `id` 和稳定 `uuid`；
- `clipboard_items` 有版本化的 `content_hash`；
- `clipboard_formats` 和 `binary_blobs` 有原始数据哈希；
- 收藏、标签、备注以及它们的关系也都有 `uuid`；
- 所有关系都使用明确外键，跨设备数据导入时可以先按 UUID 建立映射。

`sync_items` 保存来源映射及哈希，手动内容更新前检查本机修改；`sync_watermarks` 按来源设备、内容 UUID 和状态流持久记录最大序号，防止乱序回退及重启后重复拉取。`sync_receipts` 保存短期回执；`sync_local_activity` 只记录本机操作，供标题栏上传使用，远程接收不改写它。

`sync_sources` 以内容 ID 为主键，随内容删除级联清理，独立于历史及收藏关系。设备 ID 取自认证事件，名称快照取自可信配对资料；移除配对不会抹除已有内容的来源。内容事件更新来源，重复触达不修改来源，同一发送设备迟到的旧内容事件不回退来源。本机捕获次数大于 0 时优先显示本机来源，否则使用同步来源。转发只保留应用名称，设备表示本次发送方，不维护完整转发链。

历史发送进历史，收藏发送进收藏，已有内容可补充另一种关系。标签补充合并、同名复用；备注为空时填入，冲突时保留本机并通知。接收事务返回受影响的行关系，UI 局部更新，不重载无关列表。设备身份、配对和 WebDAV 凭据单独保存在 Windows DPAPI 保护的身份文件中，不存入内容数据库。完整规则见[同步实现方案](sync-plan.md)。

## 19. 未采用的结构

当前不创建以下表，避免在没有实际需求时引入额外状态：

- 复制一份文本和图片的独立收藏内容表；
- 把不同目标类型混在一起的多态原始数据表；
- 独立图片表；
- 用于记录每一次复制事件的高频事件表；
- 同步设备表、同步冲突表和删除墓碑表。

## 20. 数据写入流程

数据库结构只能约束静态关系，剪贴板捕获还必须遵守以下事务流程：

1. 使用 `BEGIN IMMEDIATE` 开始写事务，避免监听线程和 UI 线程同时分配排序值；
2. 根据完整原始格式计算 `content_hash`；
3. 已存在 `ready` 内容时，不创建重复内容，只更新历史关系；
4. 新内容先插入 `clipboard_items(capture_state = 'pending')`；
5. 写入全部 `clipboard_formats`，并完成内联数据或外部 `.bin`/`.bin.zst` 文件的校验；
6. 为每个成功格式计算 `data_hash`，判断 `restore_state`；
7. 选择一个 `restore_state = 'restorable'` 的主格式；
8. 根据格式表重新计算 `format_count` 和 `total_bytes`；
9. 更新内容为 `capture_state = 'ready'`；
10. 创建或更新唯一的 `clipboard_history`；
11. 提交事务。

如果任一步骤失败，必须回滚数据库事务，并将未完成的外部文件交给恢复任务处理。`pending` 内容不能出现在历史、收藏或搜索列表中。

重复捕获的唯一判断必须使用数据库唯一约束和冲突后查询，不能只依赖“最近 N 条”或内存缓存：

```sql
INSERT INTO clipboard_items (...)
VALUES (...)
ON CONFLICT(hash_version, content_hash) DO NOTHING;
```

然后根据哈希重新查询 `id`。如果冲突记录仍为 `pending`，当前事务必须等待或清理该未完成记录，不能直接把它当作可粘贴内容使用。

## 21. 原始数据恢复和不可变规则

### 21.1 恢复不是通用的字节复制

写回剪贴板时必须按 `format_namespace + format_name + format_kind` 选择平台格式处理器。不能假设所有格式都能通过同一个“字节数组写入”函数恢复。

以下情况应允许保存原始快照，但标记为 `not_restorable`：

- 依赖 GDI 或其他系统句柄的格式；
- 延迟渲染但未能获得实际数据的格式；
- 依赖源应用状态的自定义格式；
- 只包含文件路径、但目标文件已经不存在的文件剪贴板格式；
- 平台无法安全重建的虚拟文件格式。

只要存在一个可恢复的格式，内容可以进入历史；但默认粘贴格式必须是 `restorable`。如果没有可恢复格式，内容只能进入诊断或失败记录，不能进入正常历史。

### 21.2 原始格式默认不可变

系统捕获的 `clipboard_formats` 原始格式字节、格式身份和 `data_hash` 写入后不得由普通业务更新。`qcclip/custom-text` 是唯一的通用用户编辑格式，可以由专用自定义文本事务创建、更新或删除；更新时必须重新计算该格式的 `data_hash` 和全部派生缓存。

系统捕获格式需要修复时，仍必须走专用事务：

1. 重新捕获完整原始格式；
2. 重新计算格式哈希和内容哈希；
3. 根据新哈希查找已有内容；
4. 将历史和收藏关系迁移到确定的内容实体；
5. 最后清理旧内容及其不再引用的文件。

不能直接更新系统捕获格式的原始数据后继续保留旧的 `content_hash`。

`format_count`、`total_bytes`、`clipboard_items.preview_text` 和格式级预览字段都是派生缓存。写入格式后必须重新计算，或者由校验任务定期修复，不能把它们当作独立事实来源。任何需要切换展示优先级的功能，都必须从同一内容的 `clipboard_formats` 格式集合中选择，而不是修改或替代原始格式记录。

### 21.3 用户编辑和手动新增流程

编辑已有内容时：

1. 读取当前内容完整的纯文本来源；
2. 在编辑窗口中修改文本；
3. 使用 UTF-16LE 编码器生成 `qcclip/custom-text` 原始字节；
4. 在同一内容下创建或更新该格式；
5. 重新计算该格式的 `data_hash`、格式预览、完整格式集合的 `content_hash`、内容默认预览、`format_count` 和 `total_bytes`；
6. 将自定义文本标记为默认格式，普通预览、普通粘贴和普通复制使用它；
7. 原始 HTML、RTF、原始纯文本及其他格式不被删除或覆盖。

手动新增文本时，创建一个新的 `clipboard_items` 和历史关系，并在其中写入一条 `qcclip/custom-text` 格式。它与捕获内容使用相同的格式表、原始字节存储、预览和粘贴链路，不新增专用数据表。收藏页面的“新增收藏”入口在同一事务中额外创建 `favorites` 关系，因此不会出现内容已创建但未进入收藏列表的中间状态；空收藏页也显示该入口。

多格式菜单必须显示“自定义文本”。选择该格式时，数据库中的 `qcclip/custom-text` 载荷写出为系统 `CF_UNICODETEXT`；选择其他格式时，仍按其原始格式处理。编辑已有内容时可通过“恢复原始内容”删除自定义格式并重新选择剩余格式；手动新增文本没有原始格式，不提供该按钮，应删除整个内容项。

编辑范围作用于 `clipboard_items`，因此同一内容被历史和收藏同时引用时，两处都看到同一份自定义文本和标签。编辑窗口的标签区域默认折叠，展开后从全局 `tags` 列表中搜索并勾选标签；保存时用选中的标签集合替换 `clipboard_item_tags` 关系，取消和关闭不写文本或标签。新增文本和新增收藏也可以携带标签；新增收藏创建内容、历史关系、收藏关系和标签关系必须位于同一个事务中。

## 22. 外部原始文件和数据库一致性

### 22.1 外部文件写入

外部原始数据必须按照以下顺序写入：

1. 对原始字节计算 SHA-256；
2. 尝试使用 Zstandard 级别 3 压缩；
3. 在 `data/.pending/` 创建临时文件并写入压缩后或原始字节；
4. 解压校验原始长度和 SHA-256；
5. 将临时文件原子重命名为 `data/<sha256>.bin` 或 `data/<sha256>.bin.zst`；
6. 在数据库中将 `storage_state` 更新为 `ready`；
7. 提交数据库事务。

目标文件已存在时必须解压并重新校验原始长度和哈希，不能仅凭文件名认为文件正确。

### 22.2 启动恢复和垃圾回收

应用启动时必须检查：

- `storage_state = pending` 的索引；
- `storage_state = ready` 但文件不存在或哈希不匹配的索引；
- `data/.pending/` 中没有数据库引用的临时文件；
- `data/` 中没有任何 `binary_blobs.relative_path` 引用的孤儿文件；
- 没有任何 `clipboard_formats.blob_id` 引用的二进制索引。

文件删除应采用“先标记、后删除、再确认”的流程，避免数据库删除成功而文件仍被活跃读任务使用。

## 23. 并发、排序和数据库连接规则

### 23.1 数据库连接

每个 SQLite 连接都必须设置：

```sql
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;
```

写入内容、收藏、标签和排序的操作必须经过统一数据库服务，不允许 UI 组件直接持有连接或拼接 SQL。

### 23.2 排序重排

历史和全部收藏分别维护自己的排序空间。标签筛选复用当前列表的排序空间，不创建独立排序。重排不能直接逐行覆盖最终值，而应使用两阶段更新：

```text
第一阶段：将受影响记录改为临时不冲突的排序值
第二阶段：写入最终排序值
```

两个阶段必须位于同一个 `BEGIN IMMEDIATE` 事务中。事务提交前不能让其他查询看到中间状态。

捕获线程、列表重排和置顶操作之间不能使用“读取 MAX 后在事务外写入”的方式分配顺序，必须在同一写事务中读取和更新。

### 23.3 时间和更新字段

时间统一保存 UTC Unix 毫秒。系统时间回拨时：

- `last_seen_at` 使用 `MAX(旧值, 新值)`；
- 不用时间字段作为唯一排序身份；
- 列表查询始终使用 `sort_order` 和 `id` 作为稳定次排序；
- `updated_at` 的更新必须集中在数据库服务；跨设备接收顺序使用持久序号，不依赖设备墙钟一致。

## 24. 容量、隐私和安全边界

当前数据管理页提供数据占用统计、历史时间和条数范围、主动清理，以及数据转移与备份、数据位置迁移。数据包可按收藏、历史、图库、应用设置选择导出，支持收藏标签和历史天数筛选，以及独立密码加密；本地文件和 WebDAV 使用同一格式。导入先预览，再按所选范围合并或替换，应用前自动在本机保留备份。设备身份、配对和 WebDAV 凭据不随包覆盖，普通文件条目仅携带路径。格式、资源及冲突规则见[数据转移与备份](data-packages.md)。

导出使用统一独占操作建立 SQLite 快照并复制所选资源，再释放操作锁进行压缩、加密或上传。导入先在临时目录校验、备份并合并，成功后交给可回滚的暂存替换流程；暂停受管理操作和监听，保持切换锁直到配置热加载完成，再刷新窗口与缓存，无需重启。暂存完成后若程序退出，下次启动继续应用。快照、备份和归档会额外占用磁盘空间。

数据位置迁移使用固定宿主目录的 `location.json` 定位实际数据根目录，拒绝覆盖未受管理的顶层内容，迁移数据库、资源、图库、音效、配置、状态、日志和导入前备份；成功后清理旧位置的应用管理内容，保留用户文件。`hidden_missing_items` 是可重建的派生缓存：随数据库一起迁移；导入数据后清空，由下一次扫描按本机文件系统重新建立。没有按磁盘容量自动淘汰原始数据的配置。

历史保留范围只控制历史列表中的可见条目，不是磁盘容量硬上限。超出时间或条数范围的条目会进入待清理状态，用户可扩大范围恢复；只有主动清理才永久删除历史关系。置顶历史豁免范围限制，收藏列表按独立收藏关系查询。

指定应用过滤和暂停监听已接入运行时，可在设置或快捷键中控制。历史保留范围不等于原始数据的容量限制，删除记录也不提供安全擦除保证。旧版迁移的输入容量限制见 [迁移说明](legacy-migration.md)。

安全要求：

- 原始剪贴板数据、预览文本和文件路径不能写入普通日志；
- HTML 只作为原始字节保存，预览和渲染必须禁止脚本执行和不受控的远程资源加载；
- 解析 `relative_path` 时必须校验规范化路径仍位于应用数据目录内；
- 数据库、WAL 文件和 `.bin`/`.bin.zst` 文件必须继承应用数据目录的用户访问权限；
- 删除数据库记录不等于安全擦除磁盘内容，涉及敏感数据时需要单独的清理策略。

搜索使用预览文本、备注及来源应用等查询字段，但不能假设普通 `%keyword%` 查询在大量历史下仍然足够快。数据量增长后应评估 SQLite FTS5，并把搜索索引视为可重建缓存。

## 25. 版本兼容与同步边界

### 25.1 数据库迁移

`PRAGMA user_version` 只能表示版本，不能代替迁移执行器。每个迁移必须：

1. 在事务中执行；
2. 完成后校验表、索引、触发器和数据约束；
3. 成功后再更新 `user_version`；
4. 失败时回滚并保留诊断信息；
5. 对涉及原始数据或哈希的迁移先创建数据库备份。

哈希算法、格式命名空间和恢复策略变化都属于数据语义变化，不能只执行简单的 `ALTER TABLE`。

首发结构版本为 **1**，初始化直接创建完整的表、字段、索引和触发器。以正式发布版本的 Git Tag 为兼容边界：已发布结构的变更须评估兼容性并维护事务迁移；正式 Tag 之后、下一次正式发布前的开发变更可直接修改，不保留开发迁移链。

兼容声明存储在 `database_compatibility` 的唯一记录（`id = 1`）中：

| 字段 | 含义 |
| --- | --- |
| `minimum_read_version` | 读取数据库所需的最低数据协议版本 |
| `minimum_write_version` | 写入数据库所需的最低数据协议版本 |

已发布 schema 1 的结构版本、读取协议和写入协议均为 1；当前 schema 2 只放宽了 `capture_count` 约束并新增旧版不访问的同步表，旧版写入的数据仍满足新结构，因此读取和写入协议都保持 1，回退到旧版后可继续记录和编辑；涉及重写的维护操作仍因结构版本较高而要求使用新版。放宽约束、新增旧版不访问的表通常不需要提高写入协议；收紧约束或要求旧版同步维护新表时才需要。兼容版本不是应用版本号，也不随结构版本机械递增。增加索引、可空字段或具有可用默认值的扩展字段，通常只增加结构版本。删除核心字段、修改存储编码、改变默认值含义或资源引用语义时，必须评估并提高相应兼容版本；写入兼容版本不得低于读取兼容版本。

打开较高结构版本时：

1. 核验读取兼容声明及所有核心字段和 SQL 类型；不覆盖数据库版本、不执行旧程序的迁移。
2. 核验写入兼容声明、已知字段默认值和主键、必填字段及写入依赖的唯一约束和触发器。不满足写入要求时使用 SQLite 只读连接，仍允许查询、复制和粘贴。
3. 未知内容类型映射为 `Unknown`，未知格式保留预览但标记不可恢复；原始类型和值不回写。粘贴仅加载当前认识且支持存储编码的格式。
4. 较高结构版本暂停启动恢复、未完成文件清理、历史批量清理和孤立资源回收。删除历史关系时保留内容及原始资源，交由新版回收。
5. 较高结构版本禁止重写已有条目的格式集合；仍可按已知写入协议新增文本、收藏和标签。只读模式跳过粘贴计数与排序写入。
6. 数据目录切换和归档导出涉及完整资源集合，较高结构版本或只读模式下要求使用新版程序。

结构核验无法推断新触发器、CHECK 表达式、哈希或外部文件编码的业务含义，未来发布必须准确声明兼容版本，并验证“新版升级 → 旧版读取/写入 → 新版再次打开”，不能仅凭字段名称一致宣布兼容。

### 25.2 同步状态与迁移

同步表随内容数据库导入和迁移，设备私钥、信任列表、发送序号和 WebDAV 凭据不随内容导入覆盖。数据库热切换后取消旧代次任务，清空待应用的列表变更，重新绑定入库数据源。

接收去重以来源身份、内容 UUID、状态流和持久序号为准；本地 `id` 只用于本机关系，不作为跨设备身份，`updated_at` 不作为跨设备先后顺序。长期离线接收依赖 WebDAV 有界缓存，超出缓存保留范围的内容不自动补齐。删除不传播，因此无需删除墓碑或全库版本向量。

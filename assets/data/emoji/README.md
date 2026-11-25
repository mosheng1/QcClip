# Emoji 数据

本目录包含 QcClip 表情面板使用的本地 Unicode Emoji 数据。

- Emoji 数据版本：Unicode Emoji 17.0
- Emoji 主清单：`emoji-test.txt`
- Emoji 序列：`emoji-sequences.txt`、`emoji-zwj-sequences.txt`
- Emoji 属性：`emoji-data.txt`
- 名称和搜索关键词：CLDR 47 `annotations-zh.xml`、`annotations-en.xml`

来源：

- https://www.unicode.org/Public/emoji/latest/（下载时文件标记为 Unicode 17.0）
- https://www.unicode.org/Public/UCD/latest/ucd/emoji/（下载时文件标记为 Unicode 17.0）
- https://github.com/unicode-org/cldr/tree/release-47/common/annotations

Unicode 数据按照 Unicode 官方条款使用；CLDR 数据按照 Unicode 许可条款使用。

## 表情图片

- 素材来源：[Twemoji 17.0.3](https://github.com/jdecked/twemoji)，由 Twitter、Jason Sofonia、Justine De Caires 及其他贡献者提供。
- 图片许可：[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/)，完整许可见 `TWEMOJI-LICENSE.txt`。
- `twemoji.json.zst` 包含当前面板所需的基础表情和肤色变体，不包含 Flags 和 Component 分组；SVG 内容保持原样，仅提取并无损压缩。
- 包内以移除 U+FE0F 后的 Unicode 字符串为键，SVG 文本为值。进入表情页时在后台解压，仅渲染可见区域及前后一行的图片。
- 离开表情页或窗口隐藏完成后，释放解压数据、图片及对应纹理，并取消加载回调；再次显示时重新加载。资源不进入 GPUI 的全局图片缓存。
- 项目已包含生成的资源包，正常构建不需要 Twemoji 源码或 Python。

更新资源时，先安装 Python 的 `zstandard` 包，再在项目根目录运行：

```powershell
python scripts/pack_twemoji.py "C:/Users/canrong/Downloads/twemoji-main"
```

脚本从 `emoji-test.txt` 提取 fully-qualified 序列，校验资源齐全后生成资源包并复制图片许可。更新后同步记录素材版本，并运行 `cargo test twemoji_covers_picker_and_skin_variants`。

# QcClip 本地调整

源码来自当前 Cargo.lock 锁定的 crates.io `image 0.25.10`，保留上游源码和 MIT、Apache-2.0 许可证。

仅调整 `Cargo.toml` 的 `default-formats`，保留 PNG、JPEG、GIF、WebP、BMP、TIFF、ICO，避免 GPUI 等依赖通过默认特性重新启用额外格式。其他特性和源码保持上游实现。

主项目通过 `[patch.crates-io]` 统一使用此副本。更新 image 时需同步检查上游默认特性，并运行主项目的图片预览测试。

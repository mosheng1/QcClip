fn main() {
    println!("cargo:rerun-if-env-changed=QCCLIP_RELEASE_GUI");
    println!("cargo:rustc-check-cfg=cfg(qcclip_release_gui)");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("QCCLIP_RELEASE_GUI").as_deref() == Ok("1")
    {
        println!("cargo:rustc-cfg=qcclip_release_gui");
    }

    if cfg!(debug_assertions)
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // GPUI 在未优化构建中渲染复杂页面时，默认的 1 MiB 主线程栈不足。
        println!("cargo:rustc-link-arg-bin=QcClip=/STACK:8388608");
    }

    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("缺少构建输出目录"));
    // 原始数据保留在仓库中，二进制仅嵌入无损压缩后的内容。
    for (source, name) in [
        ("assets/data/symbols/UnicodeData.txt", "UnicodeData.txt.zst"),
        ("assets/data/emoji/emoji-test.txt", "emoji-test.txt.zst"),
        (
            "assets/data/emoji/annotations-zh.xml",
            "annotations-zh.xml.zst",
        ),
    ] {
        println!("cargo:rerun-if-changed={source}");
        let data = std::fs::read(source).expect("无法读取 Unicode 数据");
        let packed = zstd::stream::encode_all(data.as_slice(), 19).expect("无法压缩 Unicode 数据");
        std::fs::write(out_dir.join(name), packed).expect("无法保存压缩后的 Unicode 数据");
    }

    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    embed_resource::compile("assets/icon.rc", embed_resource::NONE)
        .manifest_required()
        .expect("无法将应用图标嵌入 Windows 二进制");
}

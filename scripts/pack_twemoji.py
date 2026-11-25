"""按项目的 Unicode 清单提取 Twemoji；运行前安装 zstandard：python -m pip install zstandard。"""

import argparse
import json
from pathlib import Path

import zstandard


def normalize(glyph):
    return glyph.replace("\ufe0f", "")


def main():
    parser = argparse.ArgumentParser(description="提取并压缩当前表情面板需要的 Twemoji SVG")
    parser.add_argument("source", type=Path, help="Twemoji 源码目录，包含 assets/svg")
    args = parser.parse_args()
    project = Path(__file__).resolve().parents[1]
    destination = project / "assets/data/emoji"
    sources = {
        normalize("".join(chr(int(code, 16)) for code in path.stem.split("-"))): path
        for path in (args.source / "assets/svg").glob("*.svg")
    }
    images = {}
    group = None
    for line in (destination / "emoji-test.txt").read_text(encoding="utf-8").splitlines():
        if line.startswith("# group: "):
            group = line.removeprefix("# group: ")
        if ";" not in line or group in (None, "Flags", "Component"):
            continue
        code_points, metadata = line.split(";", 1)
        if metadata.split("#", 1)[0].strip() != "fully-qualified":
            continue
        glyph = normalize("".join(chr(int(code, 16)) for code in code_points.split()))
        if glyph not in sources:
            raise SystemExit(f"Twemoji 缺少表情资源：{code_points.strip()}")
        images[glyph] = sources[glyph].read_bytes().decode("utf-8")

    payload = json.dumps(
        images, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    packed = zstandard.ZstdCompressor(level=19).compress(payload)
    if len(packed) >= 2_000_000:
        raise SystemExit(f"表情资源包超过 2 MB：{len(packed):,} 字节")
    license_text = (args.source / "LICENSE-GRAPHICS").read_bytes()
    (destination / "twemoji.json.zst").write_bytes(packed)
    (destination / "TWEMOJI-LICENSE.txt").write_bytes(license_text)
    version = json.loads((args.source / "package.json").read_text(encoding="utf-8"))["version"]
    print(f"Twemoji {version}：已打包 {len(images)} 张 SVG，共 {len(packed):,} 字节")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""校验文档内的相对链接与 GitHub 风格锚点是否可解析。

用法：python scripts/check-doc-links.py [文件 ...]
省略文件时校验 README.md、CHANGELOG.md 与 docs/ 下随仓库发布的 Markdown，
跳过 docs/superpowers/ 中不发布的本地计划；显式指定文件时仍按指定范围检查。
只检查相对路径与锚点；外部 http(s) 链接不访问网络。
"""

from __future__ import annotations

import pathlib
import re
import sys
import unicodedata

ROOT = pathlib.Path(__file__).resolve().parent.parent

LINK = re.compile(r"\[(?:[^\[\]]|\[[^\]]*\])*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
HEADING = re.compile(r"^(#{1,6})[ \t]+(.*?)[ \t]*$")


def slug(heading: str) -> str:
    """GitHub 锚点：小写、去标点、空格转连字符。"""
    text = heading.strip()
    text = re.sub(r"`", "", text)
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = text.lower()
    out = []
    for ch in text:
        if ch in " -":
            out.append("-" if ch == " " else ch)
        elif ch == "_":
            out.append(ch)
        else:
            cat = unicodedata.category(ch)
            if cat[0] in "LN" or cat == "Mn":
                out.append(ch)
    return "".join(out)


def anchors(path: pathlib.Path) -> set[str]:
    found: set[str] = set()
    seen: dict[str, int] = {}
    fence = False
    in_code = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("```") or line.startswith("~~~"):
            fence = not fence
            continue
        if fence:
            continue
        m = HEADING.match(line)
        if m:
            base = slug(m.group(2))
            count = seen.get(base, 0)
            seen[base] = count + 1
            found.add(base if count == 0 else f"{base}-{count}")
            continue
    # HTML 显式 id / name 也认。
    for m in re.finditer(r'(?:id|name)="([^"]+)"', path.read_text(encoding="utf-8")):
        found.add(m.group(1))
    return found


def main(argv: list[str]) -> int:
    if argv:
        files = [pathlib.Path(a).resolve() for a in argv]
    else:
        local_plans = ROOT / "docs" / "superpowers"
        files = sorted(p for p in ROOT.glob("docs/**/*.md") if not p.is_relative_to(local_plans))
        files += [ROOT / "README.md", ROOT / "CHANGELOG.md"]
    cache: dict[pathlib.Path, set[str]] = {}
    bad: list[str] = []
    checked = 0
    for path in files:
        if not path.exists():
            bad.append(f"缺少文件：{path}")
            continue
        text = path.read_text(encoding="utf-8")
        for m in LINK.finditer(text):
            raw = m.group(1)
            if raw.startswith(("http://", "https://", "mailto:")):
                continue
            checked += 1
            target, _, frag = raw.partition("#")
            rel = path.relative_to(ROOT).as_posix()
            line = text[: m.start()].count("\n") + 1
            if target:
                dest = (path.parent / target).resolve()
                if not dest.exists():
                    bad.append(f"{rel}:{line}: 目标不存在 {target}")
                    continue
            else:
                dest = path
            if frag:
                # GitHub 行号锚点（#L12、#L12-L34）指向文件内容，不是文档标题。
                if re.fullmatch(r"L\d+(-L\d+)?", frag):
                    continue
                if dest not in cache:
                    cache[dest] = anchors(dest)
                if frag not in cache[dest]:
                    try:
                        shown = dest.relative_to(ROOT).as_posix()
                    except ValueError:
                        shown = str(dest)
                    bad.append(f"{rel}:{line}: 锚点缺失 {shown}#{frag}")
    print(f"检查 {len(files)} 个文件、{checked} 条相对链接。")
    if bad:
        for item in bad:
            print("  失败", item)
        print(f"{len(bad)} 条未解析。")
        return 1
    print("全部相对链接与锚点均可解析。")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

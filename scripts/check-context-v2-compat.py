"""Offline old/new CLI comparison for N0 section 23.

Pass --old and --new absolute paths. For the 9f9c44dc foundation CLI,
also pass --old-supports-v2; the default old reader is commit 8284a1fb.
Only generated fixture data is read or written. No model command is executed.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


V2 = b'{"format":2,"terms":[],"persons":[]}\n'
V2_PERSON = (json.dumps({
    "format": 2, "terms": [], "persons": [{
        "person_id": "person-one", "name": "人工姓名", "aliases": ["人工别名"],
        "terms": [], "address_rules": [], "voice": {"description": "语气平实"},
        "evidence": [], "status": "tentative",
        "available_from": {"chapter": "c0001", "paragraph": "c0001-p0001"},
    }],
}, ensure_ascii=False) + "\n").encode()
LEGACY = (
    '[ { "source": "勇者", "allowed": ["勇士"], '
    '"scope": "第二章以后；先人工核对", "note": "统一" } ]\n'
).encode()
BLOCK = "c0001-b0001"


def run(binary, root, *args, fail=False):
    env = {k: v for k, v in os.environ.items() if not k.startswith("ZIYUAN_")}
    json_mode = args[0] != "init"
    result = subprocess.run(
        [str(binary), *args, *(["--json"] if json_mode else [])], cwd=root, env=env,
        capture_output=True, timeout=30,
    )
    try:
        message = result.stderr.decode("utf-8")
    except UnicodeDecodeError:
        message = result.stderr.decode("gbk", errors="replace")
    assert (result.returncode != 0) == fail, (args, result.returncode, message)
    if fail:
        return message
    return json.loads(result.stdout) if json_mode and result.stdout.strip() else None


def tree(root):
    return {
        p.relative_to(root).as_posix(): None if p.is_dir() else p.read_bytes()
        for p in root.rglob("*")
    }


def assert_same(left, right):
    a, b = tree(left), tree(right)
    changed = sorted(k for k in a.keys() | b.keys() if k not in a or k not in b or a[k] != b[k])
    assert not changed, f"Changed files/directories: {changed}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--old", required=True, type=Path)
    parser.add_argument("--new", required=True, type=Path)
    parser.add_argument("--old-supports-v2", action="store_true")
    args = parser.parse_args()
    old, new = args.old.resolve(strict=True), args.new.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="ziyuan-context-v2-compat-") as temp:
        root = Path(temp)
        seed = root / "seed"
        seed.mkdir()
        run(old, seed, "init")
        p = run(
            old, seed, "text", "init", "--title", "兼容定本", "--language", "ja",
            "--source", "self", "--rights", "self",
        )
        project = p["id"]
        (seed / "source.txt").write_text("勇者は旅立った。\n\n城へ。\n", encoding="utf-8")
        p = run(old, seed, "text", "import", project, "source.txt")
        note = run(old, seed, "note", "add", "--source", "勇者", "--zh", "勇者大人")
        draft_rel = Path("translations") / project / "context"
        snapshot_rel = Path(".ziyuan/text-projects") / project / "context"
        block_rel = Path("translations") / project / BLOCK
        roots = []
        for name, binary in [("old", old), ("new", new)]:
            work = root / name
            shutil.copytree(seed, work)
            roots.append(work)
            draft = work / draft_rel
            draft.mkdir(parents=True)
            (draft / "glossary.json").write_bytes(LEGACY)
            assert run(binary, work, "text", "context", "approve", project)["version"] == 1
            snapshot = work / snapshot_rel / "v1"
            assert (snapshot / "glossary.json").read_bytes() == LEGACY
            manifest_file = snapshot / "manifest.json"
            manifest = json.loads(manifest_file.read_bytes())
            # Only wall-clock creation time differs between independent approvals.
            # Pin it before all subsequent calls; no other field is normalized.
            manifest["created"] = 1
            manifest_file.write_bytes((json.dumps(manifest, ensure_ascii=False, indent=2) + "\n").encode())
            before = tree(snapshot)
            assert run(binary, work, "text", "context", "approve", project)["version"] == 1
            assert tree(snapshot) == before
            run(binary, work, "text", "translate", project, BLOCK)
            target = json.loads(json.dumps(p["chapters"][0]["blocks"][0]))
            for paragraph in target["paragraphs"]:
                paragraph["text"] = "译文没有允许译法。"
            (work / block_rel / "zh.json").write_bytes(
                (json.dumps(target, ensure_ascii=False, indent=2) + "\n").encode()
            )
            view = run(binary, work, "text", "verification", "show", project, BLOCK)
            assert view["hints"] == [{
                "paragraph": "c0001-p0001", "source": "勇者", "allowed": ["勇士"],
                "scope": "第二章以后；先人工核对", "note": "统一",
                "message": "原文命中词条而译文未见允许译法；作用范围为自由文本，先人工核对是否适用",
            }]
            promoted = run(binary, work, "note", "promote", note["id"], "--project", project)
            assert promoted["added"]
            glossary_before = (draft / "glossary.json").read_bytes()
            assert not run(binary, work, "note", "promote", note["id"], "--project", project)["added"]
            assert (draft / "glossary.json").read_bytes() == glossary_before
        for relative in [draft_rel, snapshot_rel, block_rel]:
            assert_same(roots[0] / relative, roots[1] / relative)
        print("PASS: legacy draft/snapshot/export bytes, free-text scope hints, note promotion and replay")

        for name, binary in [("old", old), ("new", new)]:
            for shape, payload in [("minimal", V2), ("person-availability", V2_PERSON)]:
                expected = "人物／术语资料尚未开放"
                if name == "old" and (shape == "person-availability" or not args.old_supports_v2):
                    expected = "未知字段" if args.old_supports_v2 else "格式不对"
                work = root / f"{name}-{shape}-draft"
                shutil.copytree(seed, work)
                draft = work / draft_rel
                draft.mkdir(parents=True)
                (draft / "glossary.json").write_bytes(payload)
                before = tree(work)
                error = run(binary, work, "text", "context", "approve", project, fail=True)
                if expected not in error:
                    print("DEBUG", name, shape, expected.encode("unicode_escape"), error.encode("unicode_escape"))
                assert expected in error, (name, shape, error)
                assert not (work / snapshot_rel).exists()
                if name == "new":
                    assert tree(work) == before
                # Read a hash-correct, manually introduced second-version snapshot.
                work = root / f"{name}-{shape}-snapshot"
                shutil.copytree(roots[0], work)
                snapshot = work / snapshot_rel / "v1"
                (snapshot / "glossary.json").write_bytes(payload)
                manifest_file = snapshot / "manifest.json"
                manifest = json.loads(manifest_file.read_bytes())
                for entry in manifest["files"]:
                    if entry["path"] == "glossary.json":
                        entry["sha256"] = hashlib.sha256(payload).hexdigest()
                        entry["bytes"] = len(payload)
                manifest_file.write_bytes(json.dumps(manifest, ensure_ascii=False).encode())
                before = tree(work)
                error = run(binary, work, "text", "verification", "show", project, BLOCK, fail=True)
                assert expected in error, (name, shape, error)
                assert tree(work) == before
                assert sorted(p.name for p in (work / snapshot_rel).iterdir()) == ["v1"]
                if name == "new":
                    draft = work / draft_rel
                    (draft / "style.md").unlink()
                    (draft / "summaries").rmdir()
                    before = tree(work)
                    error = run(binary, work, "text", "context", "approve", project, fail=True)
                    assert "人物／术语资料尚未开放" in error, error
                    assert tree(work) == before
        print("PASS: minimal v2 and Person.available_from approval/snapshot refusal; new CLI leaves disk unchanged")


if __name__ == "__main__":
    main()

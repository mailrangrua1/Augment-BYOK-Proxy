#!/usr/bin/env python3
# -*- coding: utf-8 -*-

from __future__ import annotations

import argparse
import gzip
import json
import re
import shutil
import sys
import time
import urllib.request
import zipfile
from pathlib import Path

# Ensure UTF-8 stdout/stderr on Windows (cp1252 default breaks emoji/CJK output)
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass


MARKETPLACE_VSIX_URL = (
    "https://marketplace.visualstudio.com/_apis/public/gallery/"
    "publishers/{publisher}/vsextensions/{extension}/latest/vspackage"
)

BYOK_PROXY_PANEL_COMMAND_ID = "vscode-augment.byokProxy.settings"


def die(message: str, *, code: int = 1) -> None:
    print(f"✗ {message}", file=sys.stderr)
    raise SystemExit(code)


def info(message: str) -> None:
    print(f"• {message}")


def safe_extract_zip(zip_path: Path, dest_dir: Path) -> None:
    dest_dir.mkdir(parents=True, exist_ok=True)
    dest_root = dest_dir.resolve()
    with zipfile.ZipFile(zip_path) as zf:
        for member in zf.infolist():
            if member.filename.endswith("/"):
                continue
            target = (dest_root / member.filename).resolve()
            if dest_root not in target.parents:
                die(f"压缩包路径异常（可能 ZipSlip）：{member.filename}")
            target.parent.mkdir(parents=True, exist_ok=True)
            with zf.open(member, "r") as src, open(target, "wb") as dst:
                shutil.copyfileobj(src, dst)


def zip_dir(src_dir: Path, out_path: Path) -> None:
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(out_path, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
        for p in sorted(src_dir.rglob("*")):
            if p.is_dir():
                continue
            rel = p.relative_to(src_dir).as_posix()
            zf.write(p, rel)


def http_download(url: str, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    req = urllib.request.Request(
        url,
        headers={"User-Agent": "augment-byok-proxy-local-vsix-repacker/1.0"},
    )
    with urllib.request.urlopen(req, timeout=120) as resp, open(dest, "wb") as f:
        shutil.copyfileobj(resp, f)


def maybe_gunzip_file(path: Path) -> None:
    with open(path, "rb") as f:
        head = f.read(2)
    if head != b"\x1f\x8b":
        return
    tmp = path.with_suffix(path.suffix + ".gunzip")
    with gzip.open(path, "rb") as src, open(tmp, "wb") as dst:
        shutil.copyfileobj(src, dst)
    tmp.replace(path)


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def find_main_js(extracted_root: Path) -> Path:
    ext_dir = extracted_root / "extension"
    pkg_path = ext_dir / "package.json"
    if pkg_path.exists():
        try:
            pkg = json.loads(read_text(pkg_path))
            main_entry = str(pkg.get("main") or "").strip()
            if main_entry:
                candidate = (ext_dir / main_entry).resolve()
                if candidate.exists() and candidate.is_file():
                    return candidate
        except Exception:
            pass

    js_files = sorted(ext_dir.rglob("*.js"))
    for f in js_files:
        try:
            s = read_text(f)
            if "handleAuthURI" in s or "augment.sessions" in s:
                return f
        except Exception:
            continue
    if js_files:
        return js_files[0]
    die("未找到任何 JS 文件（extension/*.js）")


def parse_version(extracted_root: Path) -> str:
    pkg_path = extracted_root / "extension" / "package.json"
    if not pkg_path.exists():
        die("无法读取扩展版本：缺少 extension/package.json")
    try:
        pkg = json.loads(read_text(pkg_path))
        v = str(pkg.get("version") or "").strip()
        if not v:
            die("无法读取扩展版本：package.json.version 为空")
        return v
    except Exception as e:
        die(f"无法解析 extension/package.json：{e}")

def patch_package_json(extracted_root: Path) -> None:
    pkg_path = extracted_root / "extension" / "package.json"
    if not pkg_path.exists():
        die("无法 patch：缺少 extension/package.json")
    try:
        pkg = json.loads(read_text(pkg_path))
    except Exception as e:
        die(f"无法解析 extension/package.json：{e}")

    contributes = pkg.get("contributes") if isinstance(pkg.get("contributes"), dict) else {}
    commands = contributes.get("commands") if isinstance(contributes.get("commands"), list) else []
    if not any(isinstance(c, dict) and c.get("command") == BYOK_PROXY_PANEL_COMMAND_ID for c in commands):
        commands.insert(
            0,
            {
                "category": "Augment",
                "command": BYOK_PROXY_PANEL_COMMAND_ID,
                "title": "BYOK Proxy: Settings...",
            },
        )
        info(f"已 patch commands: {BYOK_PROXY_PANEL_COMMAND_ID}")

    activation = pkg.get("activationEvents") if isinstance(pkg.get("activationEvents"), list) else []
    ev = f"onCommand:{BYOK_PROXY_PANEL_COMMAND_ID}"
    if ev not in activation:
        activation.append(ev)
        info(f"已 patch activationEvents: {ev}")

    contributes["commands"] = commands
    pkg["contributes"] = contributes
    pkg["activationEvents"] = activation
    pkg_path.write_text(json.dumps(pkg, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    info(f"已更新：{pkg_path}")


def build_header(repo_root: Path) -> str:
    header = ""
    inject_code = repo_root / "vsix-patch" / "inject-code.txt"
    auth_code = repo_root / "vsix-patch" / "byok-proxy-auth-header-inject.js"
    panel_code = repo_root / "vsix-patch" / "byok-proxy-panel-inject.js"

    if inject_code.exists():
        header += read_text(inject_code) + "\n;\n"
        info(f"已注入：{inject_code}")
    else:
        info(f"未找到 {inject_code}，跳过 header 注入")

    if not auth_code.exists():
        die(f"缺少注入文件：{auth_code}")
    header += read_text(auth_code) + "\n;\n"
    info(f"已注入：{auth_code}")

    if not panel_code.exists():
        die(f"缺少注入文件：{panel_code}")
    header += read_text(panel_code) + "\n;\n"
    info(f"已注入：{panel_code}")
    return header


def patch_main_js(repo_root: Path, main_js: Path, *, force: bool) -> None:
    original = read_text(main_js)
    if not force and "__augment_byok_proxy_panel_injected" in original:
        die("看起来已经被注入过（命中 byok-proxy-panel 关键字）；如需强制覆盖请加 --force")

    header = build_header(repo_root)
    content = header + original

    if "__augment_byok_proxy_auth_header_injected" not in content:
        die("注入失败：主入口文件里未检测到 __augment_byok_proxy_auth_header_injected")
    if "__augment_byok_proxy_panel_injected" not in content:
        die("注入失败：主入口文件里未检测到 __augment_byok_proxy_panel_injected")

    main_js.write_text(content, encoding="utf-8")


def main() -> None:
    repo_root = Path(__file__).resolve().parents[1]
    ap = argparse.ArgumentParser(description="本地一键重打注入版 Augment VSIX（含 BYOK Proxy Panel）")
    ap.add_argument("--in", dest="in_vsix", default="", help="指定本地 original.vsix（不指定则自动下载最新）")
    ap.add_argument("--out", dest="out_vsix", default="", help="输出 VSIX 路径（默认 dist/augment-vscode-modified-v{version}.vsix）")
    ap.add_argument("--publisher", default="augment", help="Marketplace publisher（默认 augment）")
    ap.add_argument("--extension", default="vscode-augment", help="Marketplace extension name（默认 vscode-augment）")
    ap.add_argument("--keep-workdir", action="store_true", help="保留 dist/_vsix_work_* 目录便于排查")
    ap.add_argument("--force", action="store_true", help="允许对已注入的 VSIX 再次注入（一般不建议）")
    args = ap.parse_args()

    dist_dir = repo_root / "dist"
    work_dir = dist_dir / f"_vsix_work_{int(time.time())}"
    extracted_root = work_dir / "unpacked_ext"
    work_dir.mkdir(parents=True, exist_ok=True)

    try:
        if args.in_vsix:
            in_path = Path(args.in_vsix).expanduser().resolve()
            if not in_path.exists():
                die(f"输入 VSIX 不存在：{in_path}")
            vsix_path = work_dir / "original.vsix"
            shutil.copyfile(in_path, vsix_path)
            maybe_gunzip_file(vsix_path)
            info(f"使用本地 VSIX：{in_path}")
        else:
            url = MARKETPLACE_VSIX_URL.format(publisher=args.publisher, extension=args.extension)
            vsix_path = work_dir / "original.vsix"
            info(f"下载 VSIX：{url}")
            http_download(url, vsix_path)
            maybe_gunzip_file(vsix_path)

        info("解压 VSIX…")
        safe_extract_zip(vsix_path, extracted_root)

        version = parse_version(extracted_root)
        main_js = find_main_js(extracted_root)
        info(f"扩展版本：{version}")
        info(f"主入口：{main_js.relative_to(extracted_root)}")

        info("注入修改…")
        patch_main_js(repo_root, main_js, force=args.force)
        patch_package_json(extracted_root)

        out_path = Path(args.out_vsix).expanduser() if args.out_vsix else (dist_dir / f"augment-vscode-modified-v{version}.vsix")
        out_path = (repo_root / out_path).resolve() if not out_path.is_absolute() else out_path.resolve()

        info("重新打包 VSIX…")
        zip_dir(extracted_root, out_path)
        info(f"完成：{out_path}")
        info("安装：VS Code → Extensions → … → Install from VSIX…")
    finally:
        if args.keep_workdir:
            info(f"保留工作目录：{work_dir}")
        else:
            shutil.rmtree(work_dir, ignore_errors=True)


if __name__ == "__main__":
    main()

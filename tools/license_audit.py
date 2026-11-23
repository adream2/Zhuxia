#!/usr/bin/env python3
"""依赖许可证审查：把 Cargo 依赖的许可证表达式归类，找出会污染项目许可的依赖。

为什么需要：项目整体是 Apache License 2.0，要求保留署名并附带许可文本。
GPL / AGPL 这类强著佐权依赖会要求整件作品按 GPL 分发，与本项目的署名条款冲突；
许可证缺失（`license` 为 None）属于"未授权"，同样不能分发。

用法：
    python tools/license_audit.py            # 报告 + 非零退出码表示有阻断项
    python tools/license_audit.py --all      # 连同宽松许可的包一起列出

判定规则（按 SPDX 表达式求值）：
- `A OR B`：任选其一，只要存在一个可用分支就算通过；
- `A AND B`：必须同时满足，取最严格的一项；
- 括号按常规优先级处理。
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys

# 强著佐权：会要求衍生作品整体换许可，与 Apache-2.0 的署名条款冲突。
STRONG = ("GPL-", "AGPL-", "SSPL-", "OSL-", "RPL-", "QPL-")
# 弱著佐权：只影响被修改的文件，通常可与闭源/NC 共存，但需要保留声明。
WEAK = ("LGPL-", "MPL-", "EPL-", "CDDL-", "IPL-", "CPL-")
# 宽松：只要求保留版权与许可声明。
PERMISSIVE = (
    "MIT",
    "Apache-2.0",
    "BSD-",
    "ISC",
    "Zlib",
    "0BSD",
    "Unlicense",
    "CC0-1.0",
    "BSL-1.0",
    "MS-PL",
    "Unicode-",
    "NCSA",
    "WTFPL",
    "Apache-1.1",
    "PostgreSQL",
    "OpenSSL",
    # bzip2 自带许可：宽松，但保留其条款原文（无著佐权义务）
    "bzip2-1.0.6",
)

RANK = {"permissive": 0, "weak": 1, "strong": 2, "unknown": 3}


def classify(ident: str) -> str:
    ident = ident.strip().strip("()")
    if not ident or ident.upper() in ("UNLICENSE",):
        return "permissive" if ident else "unknown"
    if ident.upper().startswith(("GPL", "AGPL", "SSPL", "OSL", "RPL", "QPL")):
        return "strong"
    if ident.upper().startswith(("LGPL", "MPL", "EPL", "CDDL", "IPL", "CPL")):
        return "weak"
    for p in PERMISSIVE:
        if ident.upper().startswith(p.upper()):
            return "permissive"
    return "unknown"


def evaluate(expr: str) -> str:
    """返回整个表达式的严格程度。"""
    tokens = re.findall(r"\(|\)|AND|OR|[^\s()]+", expr)
    pos = 0

    def parse_or() -> str:
        nonlocal pos
        worst = parse_and()
        while pos < len(tokens) and tokens[pos] == "OR":
            pos += 1
            # OR：取更宽松的一支
            other = parse_and()
            worst = min(worst, other, key=lambda r: RANK[r])
        return worst

    def parse_and() -> str:
        nonlocal pos
        worst = parse_atom()
        while pos < len(tokens) and tokens[pos] == "AND":
            pos += 1
            other = parse_atom()
            worst = max(worst, other, key=lambda r: RANK[r])
        return worst

    def parse_atom() -> str:
        nonlocal pos
        if pos >= len(tokens):
            return "unknown"
        tok = tokens[pos]
        if tok == "(":
            pos += 1
            inner = parse_or()
            if pos < len(tokens) and tokens[pos] == ")":
                pos += 1
            return inner
        pos += 1
        return classify(tok)

    return parse_or()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--all", action="store_true", help="列出全部依赖")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--quiet"],
        capture_output=True,
        text=True,
        check=False,
    )
    if out.returncode != 0:
        print(out.stderr.strip() or "cargo metadata 失败", file=sys.stderr)
        return 2

    data = json.loads(out.stdout)
    own = {p["name"] for p in data["packages"] if not p.get("source")}
    rows = []
    for p in data["packages"]:
        if not p.get("source") or p["name"] in own:
            continue
        expr = p.get("license") or ""
        level = evaluate(expr) if expr else "unknown"
        rows.append((p["name"], p["version"], expr or "(未声明)", level))

    counts = {k: 0 for k in RANK}
    for _, _, _, level in rows:
        counts[level] += 1

    print(f"依赖总数：{len(rows)}")
    print(f"  宽松 {counts['permissive']} / 弱著佐权 {counts['weak']} / 强著佐权 {counts['strong']} / 未声明 {counts['unknown']}")
    print("-" * 92)

    strong = [r for r in rows if r[3] == "strong"]
    unknown = [r for r in rows if r[3] == "unknown"]
    weak = [r for r in rows if r[3] == "weak"]
    flagged = strong + unknown
    for title, group in (
        ("强著佐权（阻断）", strong),
        ("未声明许可证（阻断）", unknown),
        ("弱著佐权（保留声明即可）", weak),
    ):
        if group:
            print(f"{title}：")
            for name, ver, expr, _ in sorted(group):
                print(f"  {name:32} {ver:12} {expr}")
            print()

    if args.all:
        print("全部依赖：")
        for name, ver, expr, level in sorted(rows):
            print(f"  {level:11} {name:32} {ver:12} {expr}")

    if args.json:
        print(json.dumps(
            [{"name": n, "version": v, "license": e, "level": l} for n, v, e, l in sorted(rows)],
            ensure_ascii=False,
        ))

    if flagged:
        print("FAIL：存在强著佐权或未声明许可证的依赖，不能按当前许可分发。", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

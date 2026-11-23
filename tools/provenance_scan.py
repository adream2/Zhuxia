#!/usr/bin/env python3
"""来源扫描：统计源码里仍带上游项目标识或「移植/照抄」措辞的文件。

用途：
- 重写分支的门槛检查：`python tools/provenance_scan.py --fail`（只拦阻挡分发的类别）；
- 主线风险盘点：默认只报告，不退出非零。

两类标记要区别对待，不能一刀切：

- **阻断类**（无许可证 / GPL 系列）：这类代码不能出现在 Apache-2.0 项目里，
  必须重写或拿到书面许可，所以 `--fail` 默认只拦这一类。
- **宽松类**（Apache-2.0 / MIT / MS-PL）：可以保留，只要在
  `THIRD_PARTY_NOTICES.md` 里留住版权与许可声明，并在文件里注明来源。
  这类命中是**合规要求**，清掉反而不合规——所以默认不算失败，用
  `--fail-all` 才要求全清零。

注意：脚本只能发现**显式标识**，不能替代代码相似度比对与法律审查。
"""

from __future__ import annotations

import argparse
import pathlib
import re
import sys

# 名称 -> (匹配词, 许可证, 是否阻断)
MARKERS = {
    "WuGesture": (["wugesture"], "未声明许可证", True),
    "quickshot": (["quickshot"], "GPL-3.0", True),
    "tiez-clipboard": (["tiez"], "GPL-3.0", True),
    "PicX": (["picx"], "AGPL-3.0", True),
    "TaskMonitor": (["taskmonitor"], "Apache-2.0", False),
    "sniplite": (["sniplite"], "MIT", False),
    "Captura": (["captura"], "MIT", False),
    "Filee": (["filee"], "MIT", False),
    "ScreenToGif": (["screentogif"], "MS-PL", False),
}

# 中文移植措辞：出现即说明是照搬，不管上游是什么许可证。
PHRASES = ["逐行移植", "照抄", "抄的是", "全部取自", "一一对应", "同构", "复刻"]


def matches(low: str, key: str) -> bool:
    # 词边界匹配：`MoveFileEx` 不该被当成 Filee，`screentogif` 之类整词才算。
    return re.search(rf"(?<![a-z0-9]){re.escape(key)}(?![a-z0-9])", low) is not None


def scan(root: pathlib.Path):
    rows = []
    total = 0
    for path in sorted(root.rglob("*.rs")):
        if "target" in path.parts or "_ref" in path.parts:
            continue
        try:
            lines = path.read_text(encoding="utf-8", errors="ignore").splitlines()
        except OSError:
            continue
        hits = []
        for i, line in enumerate(lines, 1):
            low = line.lower()
            for project, (keys, license_name, blocking) in MARKERS.items():
                if any(matches(low, k) for k in keys):
                    hits.append((i, project, license_name, blocking, line.strip()[:110]))
            if any(p in line for p in PHRASES):
                hits.append((i, "移植措辞", "-", True, line.strip()[:110]))
        if hits:
            rows.append((str(path.relative_to(root)), len(lines), hits))
            total += len(lines)
    return rows, total


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default="crates")
    ap.add_argument("--fail", action="store_true", help="出现阻断类标记时退出 1")
    ap.add_argument("--fail-all", action="store_true", help="任何标记都退出 1")
    args = ap.parse_args()

    rows, total = scan(pathlib.Path(args.root))
    print(f"{'文件':52} {'行数':>6}  标记")
    print("-" * 104)
    blocking_files = 0
    for name, lines, hits in rows:
        blocking = any(h[3] for h in hits)
        if blocking:
            blocking_files += 1
        tags = sorted({f"{h[1]}({h[2]})" for h in hits})
        flag = "!" if blocking else " "
        print(f"{flag}{name:51} {lines:6}  {', '.join(tags)}")
        for ln, project, license_name, is_blocking, text in hits[:3]:
            mark = "阻断" if is_blocking else "宽松"
            print(f"    L{ln:<5} [{mark} {project} {license_name}] {text}")
    print("-" * 104)
    print(f"命中文件：{len(rows)}；涉及代码行：{total}；其中阻断类 {blocking_files} 个文件")

    if args.fail_all and rows:
        print("FAIL：仍有来源标记（--fail-all）。", file=sys.stderr)
        return 1
    if args.fail and blocking_files:
        print("FAIL：仍有阻断类来源标记，不能按当前许可证分发。", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

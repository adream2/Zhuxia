#!/usr/bin/env bash
# 桌面回归：把各模块的自检串成一条命令，输出 PASS/FAIL。
#
#   bash tools/regress.sh
#
# 只依赖仓库内已有的自检入口，不引入测试框架：每个模块都有 --xxx-selftest，
# 这里只负责按顺序跑、检查产物和日志里的关键行。
set -u
cd "$(dirname "$0")/.." || exit 1
source tools/env.sh >/dev/null 2>&1 || true

EXE=./target/debug/zhuxia.exe
OUT=target/tmp/regress
LOG=target/debug/logs
mkdir -p "$OUT"
pass=0
fail=0

ok()  { printf 'PASS  %s\n' "$1"; pass=$((pass + 1)); }
bad() { printf 'FAIL  %s\n' "$1"; fail=$((fail + 1)); }

echo "== 构建"
if cargo build -p zhuxia >"$OUT/build.log" 2>&1; then ok "构建 zhuxia"; else bad "构建 zhuxia"; tail -5 "$OUT/build.log"; exit 1; fi

echo "== 单元测试"
if cargo test --workspace --lib >"$OUT/tests.log" 2>&1; then
  ok "workspace 单测（$(grep -c '^test result: ok' "$OUT/tests.log") 个套件）"
else
  bad "workspace 单测"; grep -E "^test result: FAILED|panicked" "$OUT/tests.log" | head -5
fi

echo "== 手势"
"$EXE" --gesture-doctor >"$OUT/gesture-doctor.txt" 2>&1
if grep -q "通用 12 条" "$OUT/gesture-doctor.txt"; then ok "手势诊断：通用 12 条"; else bad "手势诊断"; head -6 "$OUT/gesture-doctor.txt"; fi
if timeout 20 "$EXE" --gesture-selftest >"$OUT/gesture-selftest.txt" 2>&1; then ok "手势自检"; else bad "手势自检"; tail -4 "$OUT/gesture-selftest.txt"; fi

echo "== 截图"
rm -f target/debug/capture-selftest*.png
if timeout 40 "$EXE" --capture-selftest >"$OUT/capture.txt" 2>&1 && [ -s target/debug/capture-selftest.png ]; then
  ok "截图自检（PNG $(stat -c %s target/debug/capture-selftest.png) 字节）"
else
  bad "截图自检"; tail -4 "$OUT/capture.txt"
fi

if ZHUXIA_CAPTURE_STYLE=1 timeout 40 "$EXE" --capture-selftest >"$OUT/capture-style.txt" 2>&1 \
   && [ -s target/debug/capture-selftest-styled.png ]; then
  ok "美化自检（PNG $(stat -c %s target/debug/capture-selftest-styled.png) 字节）"
else
  bad "美化自检"; tail -4 "$OUT/capture-style.txt"
fi

if ZHUXIA_CAPTURE_LONG=1 timeout 40 "$EXE" --capture-selftest >"$OUT/capture-long.txt" 2>&1; then
  ok "长截图自检"
else
  bad "长截图自检"; tail -4 "$OUT/capture-long.txt"
fi

rm -f target/debug/record-selftest.gif
if timeout 60 "$EXE" --record-selftest-gif >"$OUT/record-gif.txt" 2>&1 && [ -s target/debug/record-selftest.gif ]; then
  ok "GIF 录制自检（$(stat -c %s target/debug/record-selftest.gif) 字节）"
else
  bad "GIF 录制自检"; tail -4 "$OUT/record-gif.txt"
fi

echo "== 常驻：剪贴板 + 任务栏"
taskkill //F //IM zhuxia.exe >/dev/null 2>&1
sleep 1
"$EXE" >"$OUT/app.log" 2>&1 &
sleep 10

# 宿主会按需自我提权重启，父进程会退出，所以只能按镜像名判断，不能看 $!。
running() { tasklist //FI "IMAGENAME eq zhuxia.exe" //FO CSV //NH 2>/dev/null | grep -qi zhuxia.exe; }
if running; then ok "宿主常驻启动"; else bad "宿主常驻启动"; fi

TODAY=$(date +%Y-%m-%d)
ALOG="$LOG/zhuxia-$TODAY.log"

# 剪贴板：用 clip.exe 写一条文本，等采集线程落库
MARK="regress-clip-$$"
printf '%s' "$MARK" | clip
sleep 3
if grep -aq "$MARK" "$ALOG" 2>/dev/null; then ok "剪贴板采集（$MARK）"; else bad "剪贴板采集"; tail -5 "$ALOG"; fi

# 任务栏：覆盖层创建成功 + 定时上屏
if grep -aq "覆盖层创建 hwnd" "$ALOG" 2>/dev/null; then ok "任务栏覆盖层创建"; else bad "任务栏覆盖层创建"; fi
if grep -aq "UpdateLayeredWindow ok=true" "$ALOG" 2>/dev/null; then ok "任务栏上屏"; else bad "任务栏上屏"; fi

# 截图热键链路：进程仍在、没有 panic
if grep -aq "panicked" "$ALOG" 2>/dev/null; then bad "运行期 panic"; grep -a -A 3 "panicked" "$ALOG" | head -6; else ok "运行期无 panic"; fi

# 提权实例用 taskkill 会被拒绝访问，先用程序自己的退出命令（它会给实例发消息）。
"$EXE" --quit >/dev/null 2>&1
sleep 3
taskkill //F //IM zhuxia.exe >/dev/null 2>&1
sleep 1
if running; then
  bad "退出后无残留进程"; taskkill //F //IM zhuxia.exe >/dev/null 2>&1
else
  ok "退出后无残留进程"
fi

echo
echo "== 结果：$pass 通过，$fail 失败（日志：$OUT）"
[ "$fail" -eq 0 ] || exit 1

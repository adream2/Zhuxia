# 手势测试清单（rewrite/cleanroom 构建）

程序：`E:\WorkBuddy\Zhuxia-rewrite\target\release\zhuxia.exe`
当前状态：钩子=true，覆盖层=true，通用规则 12 条，分组 1 个，边缘滚动开，按键回显关。

## 安全、建议先试

| 手势 | 预期 |
|---|---|
| 按住右键画 `→←↓` | 打开剪贴管理面板 |
| 按住右键画 `↓→↑` | 打开设置并停在图床管理 |
| 按住右键画 `←↓→` | 打开设置并停在开发工具 |
| 按住右键画 `↙↘↙` | 进入截图选区（Esc 退出） |
| 按住右键画 `↘` | 只显示轨迹/无动作（用于确认识别不误触） |
| 浏览器里 `→` / `←` | 前进 / 后退 |
| 光标贴屏幕左/右边缘滚轮 | 左边缘音量、右边缘亮度 |

## 需要小心

| 手势 | 风险 |
|---|---|
| `↓→` | 关闭当前窗口，别在重要窗口上试 |
| `↓←` | 关闭同进程其它窗口 |
| `↗` / `↙` | 最大化还原 / 最小化 |
| `→↙→` | 排除当前程序；恢复入口在 设置 → 工具 → 鼠标手势 → 排除的程序 |
| `↓→↓` | 打开 hosts，会弹记事本/UAC，建议跳过 |

## 看结果

- 轨迹：按住右键移动时应出现蓝色轨迹；命中规则时底部显示动作名。
- 状态文件：`target/release/gesture-status.txt`
- 日志：`target/release/logs/zhuxia-2026-11-22.log`
- 诊断：运行 `zhuxia.exe --gesture-doctor`，会写 `target/release/gesture-doctor.txt`

## 退出

- 托盘图标右键 → 退出；
- 或运行：

```powershell
E:\WorkBuddy\Zhuxia-rewrite\target\release\zhuxia.exe --quit
```

如果右键完全没反应，直接：

```powershell
taskkill /F /IM zhuxia.exe
```

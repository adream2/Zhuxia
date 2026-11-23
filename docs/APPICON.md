# 图标提取（appicon）

## 功能

- 设置页的「图标提取」有操作页：选文件 → 选形态 → 提取，结果逐条列出（帧数、尺寸、输出文件）。
- 从程序与安装包提取图标：exe / dll / ocx / cpl / scr / sys / mui / lnk / msi / msix / appx / apk / aab / ipa / app / dmg / icns / deb / rpm / AppImage。
- 输出 PNG、ICO（多尺寸）或全尺寸图集。
- 资源管理器右键入口；模块禁用时卸载菜单。
- 批量处理、输出目录、覆盖策略可配置。

## 提取方式

| 来源 | 方式 |
|---|---|
| PE（exe/dll/ocx/cpl/scr/sys/mui） | 解析 PE 资源目录，读取 RT_GROUP_ICON / RT_ICON |
| lnk | 解析 Shell Link，读取目标与图标位置 |
| msi / msix / appx | 解析安装包内资源或图标文件 |
| apk / aab | 解析 zip 与 `resources.arsc`，按密度取图标 |
| ipa / app / dmg / icns | 解析 ICNS、HFS+、DMG 内 .app |
| deb / rpm / AppImage | 解析 ar/cpio、rpm payload、squashfs |

## 输出

```text
<输出目录>/<程序名>/icon-16.png ... icon-256.png
<输出目录>/<程序名>/icon.ico
<输出目录>/<程序名>/all-sizes.png
```

- ICO 默认写入 16/24/32/48/64/128/256；可配置子集。
- 同一程序多个图标时按资源 ID/尺寸命名。
- 不覆盖已有文件，除非开启覆盖。

## 技术路线

- 纯本地解析，不联网、不调用外部工具。
- 解压使用内存/临时文件，限制单包大小与递归深度，避免压缩炸弹。
- 位图统一转 32bpp BGRA；保留 alpha，透明背景输出 PNG。
- 无图标或解析失败时给出明确原因，不生成空文件。

## 边界

- 加壳/加密安装包可能无法解析。
- 部分 MSIX/APK 的图标是自适应图标，需选择前景/背景层。
- 超大图集受内存与输出尺寸限制。

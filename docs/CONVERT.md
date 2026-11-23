# 图片格式转换（imgconv）

## 功能

- 设置页的「图片转换」有操作页：选文件 → 选目标格式 → 转换，结果逐条列出（体积、尺寸、耗时）。
- 资源管理器右键：单张/多张图片就地转格式。
- 目标格式：JPG、PNG、WebP、GIF、BMP、TIFF、ICO（多规格）。
- 输入：常见位图 + SVG（光栅化）+ ICO。
- 处理：EXIF 方向自动转正、透明区压平、长边限制、质量设置、ICO 多尺寸。
- 安全：永不覆盖源文件；同名时自动加序号。

## 技术路线

- 解码：GDI+ 处理常规位图与 ICO；SVG 用 `resvg` 光栅化（长边 256–4096 限制）。
- 方向：读取 EXIF Orientation 并在解码后旋转/翻转。
- 透明：目标格式不支持 alpha 时按背景色压平。
- 编码：GDI+ 写 JPG/PNG/GIF/BMP/TIFF；WebP 走 `libwebp`；ICO 按 16/32/48/256 多尺寸写入。
- 菜单：注册表写入资源管理器右键菜单（`HKCU\Software\Classes\SystemFileAssociations\.<ext>\shell\ZhuxiaImgConv`，
  `ExtendedSubCommandsKey` 级联到公共子菜单）；模块禁用时卸载。
  **不能挂在 `Software\Classes\.<ext>\shell\` 下**：只要扩展名有 ProgID
  （`.png` 被 WPS 抢成 `WPS.PIC.png`、`.exe` 是 `exefile`），外壳只读 ProgID 的动词，
  扩展名键上那棵树被整棵忽略——注册表全对、`installed()` 也 true，右键里就是不出现。
  **父键的默认值必须不设**——设了会被外壳判成"没有 command"，键在、菜单却不出现；
- 队列：右键传入的路径去重后串行处理，避免一次选中大量文件时并发爆内存。

## 输出规则

```text
源文件：photo.jpg
目标：PNG → photo.png
同名已存在 → photo-2.png、photo-3.png
```

- 输出目录默认与源文件相同，可配置。
- 失败不删除源文件，错误写入日志并在结果中汇总。

## 边界

- 受 DRM/损坏文件可能解码失败。
- 动图只取首帧。
- SVG 的字体依赖系统字体，复杂滤镜可能与浏览器渲染有差异。
- ICO 只支持常见尺寸，超大图标会按上限缩放。

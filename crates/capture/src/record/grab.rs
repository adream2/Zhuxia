//! 抓屏后端：DXGI 桌面复制（首选） + GDI `BitBlt`（兜底）。
//!
//! 为什么优先 DXGI（实测见 `examples/dxgi_grab.rs`）：
//!
//! | 后端 | 1280×720 | 1920×1080 | 2560×1440 | 特性 |
//! |---|---|---|---|---|
//! | GDI BitBlt | 16.7ms | 16.7~33.3ms | 33.2ms | 与 DWM present 同步，**会跳** |
//! | DXGI | 3.2ms | 3.9ms | 4.8ms | 稳定，快 4~8 倍 |
//!
//! DXGI 的模型也更合本项目的节拍：**轮询取"最新一帧"**，而不是等事件回调。
//! 代价是同一时刻只允许一个程序复制桌面（OBS 在录时我们会失败）、只支持整屏
//! （我们本来就要按选区裁剪）、不支持旋转显示器。
//!
//! 三种结果必须区分开，否则会把"画面没变"当成"抓屏坏了"：
//! - `New`：拿到新内容
//! - `Stale`：没变化（DXGI 的正常状态，视频要复用上一帧，GIF 可以跳过）
//! - `Failed`：真失败（锁屏、设备丢失、分辨率变化）

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_IDENTITY,
    DXGI_MODE_ROTATION_UNSPECIFIED, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;

use super::source::{grab_screen, FrameBuf, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    New,
    Stale,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Dxgi,
    Gdi,
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Dxgi => "DXGI",
            Backend::Gdi => "GDI",
        }
    }
}

/// 一个抓屏器。优先 DXGI，创建失败（旧系统/虚拟显卡/被占用）自动退 GDI。
pub struct Grabber {
    backend: Backend,
    rect: RECT,
    dxgi: Option<DxgiGrab>,
    gdi: Option<FrameBuf>,
    /// 最后一次成功抓到的内容（Stale 时交给调用方复用）
    last: Option<FrameBuf>,
}

impl Grabber {
    pub fn new(rect: Rect) -> Self {
        let r: RECT = rect.into();
        match DxgiGrab::new(r) {
            Ok(g) => {
                zx_core::log::info(format!(
                    "抓屏后端：DXGI（{}×{}，桌面复制已建立）",
                    rect.w, rect.h
                ));
                Grabber { backend: Backend::Dxgi, rect: r, dxgi: Some(g), gdi: None, last: None }
            }
            Err(e) => {
                zx_core::log::warn(format!(
                    "DXGI 不可用（{e}），退回 GDI —— 帧率上限会明显下降"
                ));
                Grabber {
                    backend: Backend::Gdi,
                    rect: r,
                    dxgi: None,
                    gdi: Some(FrameBuf::new(rect.w, rect.h)),
                    last: None,
                }
            }
        }
    }

    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// 抓一帧。`out` 在 `New` 时被更新；`Stale` 时保持不变。
    pub fn grab(&mut self, out: &mut FrameBuf) -> Grab {
        match self.backend {
            Backend::Dxgi => {
                let Some(g) = self.dxgi.as_mut() else { return Grab::Failed };
                match g.grab(out) {
                    DxgiResult::New => {
                        self.last = Some(out.clone());
                        Grab::New
                    }
                    DxgiResult::Stale => {
                        // 画面没变：把上一帧还给调用方，保证视频每帧都有内容
                        if let Some(prev) = self.last.as_ref() {
                            if prev.w == out.w && prev.h == out.h {
                                out.buf.copy_from_slice(&prev.buf);
                            }
                        }
                        Grab::Stale
                    }
                    DxgiResult::Failed => Grab::Failed,
                }
            }
            Backend::Gdi => {
                let Some(buf) = self.gdi.as_mut() else { return Grab::Failed };
                if grab_screen(self.rect, buf) {
                    if out.w != buf.w || out.h != buf.h {
                        *out = FrameBuf::new(buf.w, buf.h);
                    }
                    out.buf.copy_from_slice(&buf.buf);
                    self.last = Some(out.clone());
                    // GDI 无法区分"没变"和"变了"，一律当 New（GIF 侧自己会做脏矩形）
                    Grab::New
                } else {
                    Grab::Failed
                }
            }
        }
    }
}

enum DxgiResult {
    New,
    Stale,
    Failed,
}

struct DxgiGrab {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    size: (u32, u32),
    rect: RECT,
    origin: (i32, i32),
}

impl DxgiGrab {
    fn new(rect: RECT) -> windows::core::Result<Self> {
        unsafe {
            let mut dev: Option<ID3D11Device> = None;
            let mut ctx: Option<ID3D11DeviceContext> = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut dev),
                None,
                Some(&mut ctx),
            )?;
            let device = dev.ok_or_else(|| err("D3D11 设备创建返回空"))?;
            let context = ctx.ok_or_else(|| err("D3D11 上下文创建返回空"))?;

            let dxgi: IDXGIDevice = device.cast()?;
            let adapter: IDXGIAdapter = dxgi.GetAdapter()?;
            let mut chosen: Option<(IDXGIOutput, RECT, DXGI_MODE_ROTATION)> = None;
            let mut i = 0u32;
            while let Ok(output) = adapter.EnumOutputs(i) {
                if let Ok(desc) = output.GetDesc() {
                    let r = desc.DesktopCoordinates;
                    let hit = rect.left < r.right
                        && rect.right > r.left
                        && rect.top < r.bottom
                        && rect.bottom > r.top;
                    if hit {
                        chosen = Some((output, r, desc.Rotation));
                        break;
                    }
                }
                i += 1;
            }
            let Some((output, bounds, rotation)) = chosen else {
                return Err(err("没有覆盖该区域的显示器"));
            };
            if rotation != DXGI_MODE_ROTATION_IDENTITY && rotation != DXGI_MODE_ROTATION_UNSPECIFIED
            {
                return Err(err("不支持旋转的显示器"));
            }
            let out1: IDXGIOutput1 = output.cast()?;
            let dup = out1.DuplicateOutput(&device)?;
            Ok(DxgiGrab {
                device,
                context,
                dup,
                staging: None,
                size: (0, 0),
                rect,
                origin: (bounds.left, bounds.top),
            })
        }
    }

    fn grab(&mut self, out: &mut FrameBuf) -> DxgiResult {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            if self.dup.AcquireNextFrame(0, &mut info, &mut res).is_err() {
                // 超时 = 画面没变化；其他错误也是在这里报，交给调用方计数
                return if res.is_none() { DxgiResult::Stale } else { DxgiResult::Failed };
            }
            let r = (|| -> DxgiResult {
                let Some(res) = res else { return DxgiResult::Failed };
                let Ok(texture) = res.cast::<ID3D11Texture2D>() else { return DxgiResult::Failed };
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut desc);
                let (fw, fh) = (desc.Width, desc.Height);
                if fw == 0 || fh == 0 {
                    return DxgiResult::Failed;
                }
                if self.staging.is_none() || self.size != (fw, fh) {
                    let sd = D3D11_TEXTURE2D_DESC {
                        Width: fw,
                        Height: fh,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                        Usage: D3D11_USAGE_STAGING,
                        BindFlags: 0,
                        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                        MiscFlags: 0,
                    };
                    let mut st: Option<ID3D11Texture2D> = None;
                    if self.device.CreateTexture2D(&sd, None, Some(&mut st)).is_err() {
                        return DxgiResult::Failed;
                    }
                    self.staging = st;
                    self.size = (fw, fh);
                }
                let Some(staging) = self.staging.as_ref() else { return DxgiResult::Failed };
                self.context.CopyResource(staging, &texture);
                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                if self
                    .context
                    .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .is_err()
                {
                    return DxgiResult::Failed;
                }
                let ox = (self.rect.left - self.origin.0).max(0) as u32;
                let oy = (self.rect.top - self.origin.1).max(0) as u32;
                let w = ((self.rect.right - self.rect.left) as u32)
                    .min(fw.saturating_sub(ox))
                    .max(1);
                let h = ((self.rect.bottom - self.rect.top) as u32)
                    .min(fh.saturating_sub(oy))
                    .max(1);
                if out.w != w as i32 || out.h != h as i32 {
                    *out = FrameBuf::new(w as i32, h as i32);
                }
                let src = mapped.pData as *const u8;
                let pitch = mapped.RowPitch as usize;
                let dstride = out.stride();
                let row_bytes = (w as usize * 4).min(dstride);
                for y in 0..h as usize {
                    let s = (oy as usize + y) * pitch + ox as usize * 4;
                    let d = y * dstride;
                    std::ptr::copy_nonoverlapping(src.add(s), out.buf.as_mut_ptr().add(d), row_bytes);
                    for x in (d + 3..d + row_bytes).step_by(4) {
                        out.buf[x] = 255;
                    }
                }
                self.context.Unmap(staging, 0);
                DxgiResult::New
            })();
            // 必须还帧，否则下一次 AcquireNextFrame 会一直失败
            let _ = self.dup.ReleaseFrame();
            r
        }
    }
}

fn err(msg: &str) -> windows::core::Error {
    windows::core::Error::new(windows::Win32::Foundation::E_FAIL, msg)
}

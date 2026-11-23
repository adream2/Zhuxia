//! DXGI 桌面复制取帧探针。
//!
//! 为什么是 DXGI 而不是 WGC：WGC 是**变化驱动 + 事件回调**的模型（静止画面不产帧，
//! 且回调有线程亲和性问题）；DXGI 是**轮询 + 取最新帧**，和本项目的录制节拍天然对齐。
//! 代价是只能抓整块显示器（我们本来就要按选区裁剪），且同一时刻只允许一个程序复制桌面。
//!
//! ```text
//! cargo run --release --example dxgi_grab
//! ```

use std::time::Instant;

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::*;

use capture::record::source::FrameBuf;

pub struct DxgiGrab {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: Option<ID3D11Texture2D>,
    size: (u32, u32),
    rect: RECT,
    origin: (i32, i32),
}

impl DxgiGrab {
    pub fn new(rect: RECT) -> windows::core::Result<Self> {
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
            let device = dev.expect("d3d11 device");
            let context = ctx.expect("d3d11 context");

            // 找到覆盖选中区域的显示器
            let dxgi: IDXGIDevice = device.cast()?;
            let adapter: IDXGIAdapter = dxgi.GetAdapter()?;
            let mut chosen: Option<(IDXGIOutput, RECT)> = None;
            let mut i = 0u32;
            while let Ok(output) = adapter.EnumOutputs(i) {
                if let Ok(desc) = output.GetDesc() {
                    let r = desc.DesktopCoordinates;
                    let hit = rect.left < r.right && rect.right > r.left
                        && rect.top < r.bottom && rect.bottom > r.top;
                    if hit {
                        chosen = Some((output, r));
                        break;
                    }
                }
                i += 1;
            }
            let Some((output, bounds)) = chosen else {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "没有覆盖该区域的显示器",
                ));
            };
            if output.GetDesc()?.Rotation != DXGI_MODE_ROTATION_IDENTITY
                && output.GetDesc()?.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED
            {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "暂不支持旋转显示器",
                ));
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

    /// 取最新一帧，裁到构造时的 `rect`。false = 画面没变化（复用上一帧）。
    pub fn grab(&mut self, out: &mut FrameBuf) -> bool {
        unsafe {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            if self.dup.AcquireNextFrame(0, &mut info, &mut res).is_err() {
                return false;
            }
            let result = (|| -> bool {
                let Some(res) = res else { return false };
                let Ok(texture) = res.cast::<ID3D11Texture2D>() else { return false };
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                texture.GetDesc(&mut desc);
                let (fw, fh) = (desc.Width, desc.Height);
                if fw == 0 || fh == 0 {
                    return false;
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
                        return false;
                    }
                    self.staging = st;
                    self.size = (fw, fh);
                }
                let Some(staging) = self.staging.as_ref() else { return false };
                self.context.CopyResource(staging, &texture);

                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                if self
                    .context
                    .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                    .is_err()
                {
                    return false;
                }
                let ox = (self.rect.left - self.origin.0).max(0) as u32;
                let oy = (self.rect.top - self.origin.1).max(0) as u32;
                let w = ((self.rect.right - self.rect.left) as u32).min(fw.saturating_sub(ox)).max(1);
                let h = ((self.rect.bottom - self.rect.top) as u32).min(fh.saturating_sub(oy)).max(1);
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
                    // DXGI 的 alpha 常为 0，统一刷不透明
                    for x in (d + 3..d + row_bytes).step_by(4) {
                        out.buf[x] = 255;
                    }
                }
                self.context.Unmap(staging, 0);
                true
            })();
            // 无论成功与否都必须还帧，否则下一次 AcquireNextFrame 会卡住
            let _ = self.dup.ReleaseFrame();
            result
        }
    }
}

fn main() {
    let _ = zx_core::cfg::load();
    zx_ui::window::init_dpi();

    let (vx, vy) = unsafe {
        use windows::Win32::UI::WindowsAndMessaging::*;
        (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN))
    };

    for (w, h) in [(1280, 720), (1920, 1080), (2560, 1440)] {
        let rect = RECT { left: vx + 100, top: vy + 100, right: vx + 100 + w, bottom: vy + 100 + h };
        let mut g = match DxgiGrab::new(rect) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("{w}×{h}：创建失败 {e}");
                continue;
            }
        };
        let mut out = FrameBuf::new(w, h);
        // 等第一帧（桌面复制启动后要有内容变化才会给帧）
        let t_warm = Instant::now();
        let mut got = false;
        while t_warm.elapsed().as_millis() < 2000 {
            if g.grab(&mut out) {
                got = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        if !got {
            eprintln!("{w}×{h}：2 秒内没拿到帧（桌面完全静止？）");
            continue;
        }
        println!("\n── {w}×{h} ── 首帧 {:.0}ms", t_warm.elapsed().as_secs_f64() * 1000.0);

        let mut times = Vec::new();
        let mut miss = 0;
        let t0 = Instant::now();
        while t0.elapsed().as_millis() < 1500 {
            let t = Instant::now();
            if g.grab(&mut out) {
                times.push(t.elapsed().as_secs_f64() * 1000.0);
            } else {
                miss += 1;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if times.is_empty() {
            println!("  没测到带内容的帧（画面静止）");
            continue;
        }
        let avg = times.iter().sum::<f64>() / times.len() as f64;
        let max = times.iter().cloned().fold(0.0, f64::max);
        println!(
            "  1.5 秒：{} 帧有更新 / {} 次无变化，单帧 avg {avg:.2}ms / max {max:.2}ms → 上限 {:.0} fps",
            times.len(),
            miss,
            1000.0 / avg
        );

        let mut cv = zx_ui::Canvas::new(out.w, out.h, 96);
        {
            let dst = cv.pixels_mut();
            let n = dst.len().min(out.buf.len());
            dst[..n].copy_from_slice(&out.buf[..n]);
        }
        let p = zx_core::cfg::run_dir().join(format!("dxgi-probe-{w}x{h}.png"));
        if let Some(png) = cv.png_bytes() {
            let _ = std::fs::write(&p, png);
            println!("  样本：{}", p.display());
        }
    }
}

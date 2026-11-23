//! WinML 探针：验证「系统自带的 Windows ML 能不能直接吃 PaddleOCR 的 ONNX 模型」。
//!
//! `cargo run --release --example winml_probe -- <model.onnx> [宽] [高]`
//! 会打印模型的输入/输出特征，并用全零张量跑一次推理，报告输出长度与耗时。
//!
//! 用途：用户反馈"PaddleOCR 加载失败"时，先用它确认这台机器有没有 Windows ML
//! （Win10 1809 之前没有，精简版系统也可能被裁掉）。

use windows::core::{Interface, HSTRING};
use windows::AI::MachineLearning::{LearningModel, LearningModelBinding, LearningModelSession, TensorFloat};

fn shape_of(d: &windows::AI::MachineLearning::ILearningModelFeatureDescriptor) -> Vec<i64> {
    d.cast::<windows::AI::MachineLearning::TensorFeatureDescriptor>()
        .and_then(|t| t.Shape())
        .map(|s| (0..s.Size().unwrap_or(0)).filter_map(|i| s.GetAt(i).ok()).collect())
        .unwrap_or_default()
}

fn main() {
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_MULTITHREADED,
        );
    }
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "det.onnx".into());
    let w: i64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(960);
    let h: i64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(960);

    let model = match LearningModel::LoadFromFilePath(&HSTRING::from(path.as_str())) {
        Ok(m) => m,
        Err(e) => {
            println!("加载失败：{e}");
            return;
        }
    };

    if let Ok(features) = model.InputFeatures() {
        for i in 0..features.Size().unwrap_or(0) {
            if let Ok(d) = features.GetAt(i) {
                println!("输入 {i}: {}  kind={:?}  shape={:?}",
                         d.Name().map(|s| s.to_string()).unwrap_or_default(),
                         d.Kind().map(|k| format!("{k:?}")).unwrap_or_default(),
                         shape_of(&d));
            }
        }
    }
    if let Ok(features) = model.OutputFeatures() {
        for i in 0..features.Size().unwrap_or(0) {
            if let Ok(d) = features.GetAt(i) {
                println!("输出 {i}: {}  shape={:?}",
                         d.Name().map(|s| s.to_string()).unwrap_or_default(),
                         shape_of(&d));
            }
        }
    }

    let session = match LearningModelSession::CreateFromModel(&model) {
        Ok(s) => s,
        Err(e) => {
            println!("建会话失败：{e}");
            return;
        }
    };
    let binding = match LearningModelBinding::CreateFromSession(&session) {
        Ok(b) => b,
        Err(e) => {
            println!("绑定失败：{e}");
            return;
        }
    };
    let name = model
        .InputFeatures()
        .ok()
        .and_then(|f| f.GetAt(0).ok())
        .and_then(|d| d.Name().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "x".into());
    let data = vec![0f32; (3 * w * h) as usize];
    let tensor = match TensorFloat::CreateFromShapeArrayAndDataArray(&[1, 3, h, w], &data) {
        Ok(t) => t,
        Err(e) => {
            println!("建张量失败：{e}");
            return;
        }
    };
    if let Err(e) = binding.Bind(&HSTRING::from(name.as_str()), &tensor) {
        println!("绑输入失败：{e}");
        return;
    }
    let started = std::time::Instant::now();
    match session.Evaluate(&binding, &HSTRING::new()) {
        Ok(result) => {
            let ms = started.elapsed().as_millis();
            let len = result
                .Outputs()
                .ok()
                .and_then(|o| o.First().ok())
                .and_then(|it| it.Current().ok())
                .and_then(|kv| kv.Value().ok())
                .and_then(|v| v.cast::<TensorFloat>().ok())
                .and_then(|t| t.GetAsVectorView().ok())
                .map(|v| v.Size().unwrap_or(0))
                .unwrap_or(0);
            println!("推理成功：输出 {len} 个 float，{ms} ms");
        }
        Err(e) => println!("推理失败：{e}"),
    }
}

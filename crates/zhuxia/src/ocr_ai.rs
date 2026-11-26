//! OCR 结果的翻译与 AI 整理（外壳实现）。
//!
//! `capture` 那边只留了两个函数指针，真正的网络请求在这里——因为 `imgbed::http`
//! （WinHTTP）在这个 crate 的依赖里，没有必要为了两个接口把网络栈塞进截图库。
//!
//! 两个能力都允许用户自己配：
//!
//! | 能力 | 配置 | 不配时 |
//! | --- | --- | --- |
//! | 翻译 | `translateUrl`（POST JSON `{"text":…}` → `{"text":…}`） | 走内置免费接口（Google gtx，无需 key） |
//! | AI | `aiBaseUrl` / `aiModel` + 加密存储的 `ai.key`（OpenAI 兼容） | 报错提示去配置 |
//!
//! AI 的用途定死是**清理 OCR 结果**，不是聊天。OCR 输出的典型毛病（错字、断行全乱、
//! 表格串成一行、代码缩进丢了）恰好是大模型最擅长的活，而"识别→整理成可粘贴的文本"
//! 也正是截图工具用户唯一想要的 AI。

use serde_json::{json, Value};

use zx_core::cfg;

const AI_PROMPT: &str = "\
你是 OCR 结果清理器。用户会给你一段从截图里识别出来的文字，请把它整理成可以直接使用的形式：
1. 修正常见的 OCR 错字（形近字、全半角、多余空格）；
2. 按内容恢复换行与段落：标题单独成行，列表保持列表，代码保持原有缩进；
3. 如果内容明显是表格，输出 Markdown 表格；
4. 不要增删信息，不要解释，不要加任何前缀，只输出整理后的正文。";

/// 翻译目标语言。
///
/// 有道和 Google 的语言码不一样（有道是 `zh-CHS`，Google 是 `zh-CN`），这里统一成
/// 自己的枚举，各自映射一套，免得把某一家的码硬编码到调用点。
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lang {
    Zh,
    En,
    Ja,
    Ko,
    Fr,
    De,
    Es,
    Ru,
}

impl Lang {
    fn youdao(self) -> &'static str {
        match self {
            Lang::Zh => "zh-CHS",
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::Ko => "ko",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::Es => "es",
            Lang::Ru => "ru",
        }
    }

    fn google(self) -> &'static str {
        match self {
            Lang::Zh => "zh-CN",
            Lang::En => "en",
            Lang::Ja => "ja",
            Lang::Ko => "ko",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::Es => "es",
            Lang::Ru => "ru",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "英文",
            Lang::Ja => "日语",
            Lang::Ko => "韩语",
            Lang::Fr => "法语",
            Lang::De => "德语",
            Lang::Es => "西班牙语",
            Lang::Ru => "俄语",
        }
    }
}

/// 配置里的目标语言 → 实际语言。`auto` 的语义是"翻到**另一种**语言"：
/// 输入里有中文就译成英文，没有中文就译成中文。
///
/// 不用有道自己的 `to=Auto`：那个在输入中文时会译成英文，输入英文时译成中文，
/// 看着"聪明"，实际是**方向不可控**——用户选定了目标语言就该按目标走。
fn resolve_lang(cfg_value: &str, text: &str) -> Lang {
    match cfg_value.trim().to_ascii_lowercase().as_str() {
        "zh" | "zh-chs" | "zh-cn" | "chinese" => Lang::Zh,
        "en" | "english" => Lang::En,
        "ja" | "jp" | "japanese" => Lang::Ja,
        "ko" | "kr" | "korean" => Lang::Ko,
        "fr" => Lang::Fr,
        "de" => Lang::De,
        "es" => Lang::Es,
        "ru" => Lang::Ru,
        // auto 或没配
        _ => {
            if has_cjk(text) {
                Lang::En
            } else {
                Lang::Zh
            }
        }
    }
}

/// 粗略判断有没有中日韩字符（够用了：只用来决定 auto 的方向）。
fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        let u = c as u32;
        (0x4E00..=0x9FFF).contains(&u)      // 基本汉字
            || (0x3040..=0x30FF).contains(&u) // 假名
            || (0xAC00..=0xD7AF).contains(&u) // 谚文
    })
}

/// `to` 由调用方每次指定（结果窗口的下拉）；传 `auto` 走自动判方向。
pub fn translate(text: &str, to: &str) -> Result<(String, Option<String>), String> {
    let c = cfg::cfg().capture();
    let lang = if to.trim().is_empty() { "auto" } else { to };
    let to = resolve_lang(lang, text);
    let url = c.translate_url;
    if url.trim().is_empty() {
        // 内置接口 = 把正文发给第三方。没同意就一个字节都不发。
        if !c.translate_consent {
            return Err(
                "内置翻译需要先同意：正文会发送到第三方免费接口。\
                 可在设置 → 截图 → 翻译里允许，或在结果窗口里点「翻译」并确认。"
                    .into(),
            );
        }
        return translate_builtin(text, to);
    }
    // 自定义接口：把目标语言也带上，用不上的服务端会忽略
    let body = json!({ "text": text, "to": to.google(), "from": "auto" }).to_string();
    let v = post_json(url.trim(), &body, &[])?;
    let t = v
        .get("text")
        .and_then(|t| t.as_str())
        .map(|t| t.to_string())
        .ok_or_else(|| "翻译接口返回里没有 text 字段".to_string())?;
    // 自建接口是整段一次请求，没有"部分失败"这回事
    Ok((t, None))
}

/// 内置免费翻译。**默认走国内能直连的接口**。
///
/// 实测（2026-11-25）：`aidemo.youdao.com/trans` 可用，返回 `{"translation":["…"]}`；
/// `fanyi.youdao.com/translate` 已经返回空（老接口废了，别再放第一位）；Google gtx
/// 国内不通，只留作有代理时的兜底。
///
/// 长文本按行切成不超过 [`CHUNK`] 字符的块分别翻——GET 查询串太长会被服务端截断，
/// 而 OCR 结果动辄几千字。
const CHUNK: usize = 1000;

/// 内置接口的**总**时间预算。
///
/// 免费接口不可用时，每块要串行试两条（8s + 8s）。5000 字的正文就是 5 块 × 16s = 80s，
/// 用户只能对着"翻译中…"干等，而且中途没有任何反馈。到点就带着已有结果收工。
const BUILTIN_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// 上次通了哪条内置接口（1 = 有道，2 = Google，0 = 不知道）。
///
/// 有道是主力（国内直连），但它说挂就挂。一旦发现某条不通，后面每一块就不必再等它
/// 8 秒超时——这是"接口挂了"从 16s/块 降到 8s/块 的唯一办法。
static PREFERRED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn translate_builtin(text: &str, to: Lang) -> Result<(String, Option<String>), String> {
    let parts = chunks(text, CHUNK);
    let start = std::time::Instant::now();
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    let mut first_err = String::new();
    let mut failed = 0usize;
    let mut skipped = 0usize;
    for (i, chunk) in parts.iter().enumerate() {
        if start.elapsed() > BUILTIN_BUDGET {
            // 预算用尽：剩下的原样留着，至少结果还能用
            skipped = parts.len() - i;
            out.extend(parts[i..].iter().cloned());
            break;
        }
        match translate_chunk(chunk, to) {
            Ok(t) => out.push(t),
            Err(e) => {
                // 单块失败不再整段失败：保留原文继续翻后面的块。
                // 以前遇到第一个错误就 break，前面的译文全丢，等于白等。
                failed += 1;
                if first_err.is_empty() {
                    first_err = e;
                }
                out.push(chunk.clone());
            }
        }
    }
    // 一块都没成，才叫失败——此时给出真实原因
    if failed == parts.len() && !parts.is_empty() {
        return Err(format!("内置翻译失败 —— {first_err}"));
    }
    let mut joined = String::new();
    for (i, t) in out.iter().enumerate() {
        if i > 0 {
            joined.push(char::from(10));
        }
        joined.push_str(t);
    }
    // 部分成功必须让用户知道：结果里那块是**原文**，不是漏翻的 bug。
    // 提示要短，它要挤进结果窗口那一行副标题。
    let warn = if failed > 0 || skipped > 0 {
        zx_core::log::warn(format!(
            "内置翻译：{failed}/{} 块失败，{skipped} 块超时未翻。首个错误：{first_err}",
            parts.len()
        ));
        let mut bits: Vec<String> = Vec::new();
        if failed > 0 {
            bits.push(format!("{failed} 段未翻译"));
        }
        if skipped > 0 {
            bits.push(format!("{skipped} 段超时跳过"));
        }
        Some(format!("{}，保留原文", bits.join("、")))
    } else {
        None
    };
    Ok((joined, warn))
}

fn translate_chunk(text: &str, to: Lang) -> Result<String, String> {
    let q = pct(text);
    // 上一次通了哪条就先试哪条，避免每次都在已知死掉的接口上白等 8 秒
    let prefer = PREFERRED.load(std::sync::atomic::Ordering::Relaxed);
    let order: [u8; 2] = if prefer == 2 { [2, 1] } else { [1, 2] };
    let mut errs: Vec<String> = Vec::new();
    for which in order {
        let res = if which == 1 {
            youdao(&q, to)
        } else {
            google(&q, to)
        };
        match res {
            Ok(t) if !t.trim().is_empty() => {
                PREFERRED.store(which, std::sync::atomic::Ordering::Relaxed);
                return Ok(t);
            }
            Ok(_) => errs.push(format!("{}返回里没有译文", name_of(which))),
            Err(e) => errs.push(format!("{}：{e}", name_of(which))),
        }
    }
    // 两条都不通：清掉偏好，下块重新按默认顺序试
    PREFERRED.store(0, std::sync::atomic::Ordering::Relaxed);
    Err(errs.join("；"))
}

fn name_of(which: u8) -> &'static str {
    if which == 1 { "有道" } else { "Google" }
}

fn youdao(q: &str, to: Lang) -> Result<String, String> {
    let url = format!("https://aidemo.youdao.com/trans?q={q}&from=Auto&to={}", to.youdao());
    let v = get_json(&url)?;
    let arr = v.get("translation").and_then(|x| x.as_array());
    Ok(arr
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default())
}

fn google(q: &str, to: Lang) -> Result<String, String> {
    let url = format!(
        "https://translate.googleapis.com/translate_a/single?client=gtx&sl=auto&tl={}&dt=t&q={q}",
        to.google()
    );
    let v = get_json(&url)?;
    let mut t = String::new();
    if let Some(segs) = v.get(0).and_then(|x| x.as_array()) {
        for seg in segs {
            if let Some(x) = seg.get(0).and_then(|x| x.as_str()) {
                t.push_str(x);
            }
        }
    }
    Ok(t)
}

/// 按行切块，尽量不把一行劈开；单行超长时才硬切。
///
/// 两条硬约束（旧实现都破坏了）：
/// 1. 每块 `chars().count() <= max`——旧版切分时先塞了个换行再算空间，块会超长 1 个字符；
/// 2. 不产出空块——旧版在 `room == 0` 时会 push 一个空串，空地进翻译接口只会换来一次
///    白跑（有的接口直接报错，反而让整段翻译失败）。
fn chunks(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut len = 0usize;

    /// 出一块。纯空白的块没有翻译价值，直接丢掉。
    ///
    /// 尾部的空行也剪掉：它只是"块边界"的副产物，送给接口既浪费字节，
    /// 也会让译文末尾多出几个换行。
    fn flush(out: &mut Vec<String>, cur: &mut String, len: &mut usize) {
        while cur.ends_with('\n') {
            cur.pop();
        }
        if !cur.trim().is_empty() {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
        *len = 0;
    }

    for line in text.lines() {
        // 块内行间分隔：放不下就先出块，换行本身也算一个字符
        if len > 0 {
            if len >= max {
                flush(&mut out, &mut cur, &mut len);
            } else {
                cur.push('\n');
                len += 1;
            }
        }
        for ch in line.chars() {
            if len >= max {
                flush(&mut out, &mut cur, &mut len);
            }
            cur.push(ch);
            len += 1;
        }
    }
    flush(&mut out, &mut cur, &mut len);
    if out.is_empty() {
        out.push(text.to_string());
    }
    out
}

fn get_json(url: &str) -> Result<Value, String> {
    // UA 里带上自己的名字：不假装是浏览器，对端被滥用时能定位到是谁。
    // 保留 Mozilla 前缀是因为部分免费端点对非浏览器 UA 直接 403。
    let ua = format!(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Zhuxia/{}",
        env!("CARGO_PKG_VERSION")
    );
    let resp = imgbed::http::request(
        "GET",
        url,
        &[("Accept", "application/json"), ("User-Agent", ua.as_str())],
        None,
        "",
        8000,
    )?;

    if resp.status >= 400 {
        return Err(format!("HTTP {}", resp.status));
    }
    serde_json::from_str(&resp.text()).map_err(|e| format!("响应不是 JSON（{e}）"))
}

/// AI Key。存 DPAPI 加密的 `imgbed/secrets.dat`（`ai.key`），**不写 settings.yaml**：
/// 那份文件经常被同步、备份、贴进聊天，而 key 等价于账号额度。
///
/// 老版本把 key 明文写在 `settings.yaml` 的 `aiKey` 里；第一次读到非空值就迁过来
/// 并清空原字段，用户无感。
fn ai_key() -> String {
    if let Some(k) = imgbed::secrets::get("ai.key") {
        return k;
    }
    let legacy = cfg::cfg()
        .capture
        .clone()
        .unwrap_or_default()
        .ai_key
        .unwrap_or_default()
        .trim()
        .to_string();
    if legacy.is_empty() {
        return String::new();
    }
    if imgbed::secrets::set("ai.key", &legacy).is_ok() {
        cfg::update_cfg(|s| {
            if let Some(c) = s.capture.as_mut() {
                c.ai_key = None;
            }
        });
        zx_core::log::info("OCR AI：已把 aiKey 从 settings.yaml 迁到 DPAPI 加密存储");
    }
    legacy
}

pub fn polish(text: &str) -> Result<String, String> {
    let c = cfg::cfg().capture();
    let base = c.ai_base_url;
    let key = ai_key();
    let model = c.ai_model;
    if base.trim().is_empty() || model.trim().is_empty() {
        return Err("AI 还没配置：设置里填 aiBaseUrl / aiKey / aiModel（OpenAI 兼容接口）".into());
    }
    let prompt = if c.ai_prompt.trim().is_empty() {
        AI_PROMPT.to_string()
    } else {
        c.ai_prompt.clone()
    };
    let url = format!("{}/chat/completions", base.trim().trim_end_matches('/'));
    let body = json!({
        "model": model.trim(),
        "temperature": 0.2,
        "messages": [
            { "role": "system", "content": prompt },
            { "role": "user", "content": text },
        ],
    })
    .to_string();
    let auth = format!("Bearer {}", key.trim());
    let v = post_json(&url, &body, &[("Authorization", auth.as_str())])?;
    v.pointer("/choices/0/message/content")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "AI 返回里没有内容（检查模型名和额度）".to_string())
}

fn post_json(url: &str, body: &str, extra: &[(&str, &str)]) -> Result<Value, String> {
    let mut headers: Vec<(&str, &str)> = vec![
        ("Content-Type", "application/json"),
        ("Accept", "application/json"),
    ];
    headers.extend_from_slice(extra);
    let resp = imgbed::http::request("POST", url, &headers, Some(body.as_bytes()), "", 30000)
        .map_err(|e| format!("请求失败：{e}"))?;
    let text = resp.text();
    if resp.status >= 400 {
        let brief: String = text.chars().take(200).collect();
        return Err(format!("HTTP {}：{brief}", resp.status));
    }
    serde_json::from_str(&text).map_err(|e| format!("响应不是 JSON：{e}"))
}

/// URL 查询串的百分号编码（只保留非保留字符）。
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 当前配置下的目标语言（探活/设置页用来显示"翻成什么"）。
pub fn target_label(sample: &str, to: &str) -> &'static str {
    let code = if to.trim().is_empty() {
        cfg::cfg().capture().translate_to
    } else {
        to.to_string()
    };
    resolve_lang(&code, sample).label()
}

/// 由 modules::install 调用，把能力注入截图库。
pub fn install() {
    capture::ocr_ai::install(translate, polish);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_keeps_lines_whole_when_possible() {
        let text = "第一行
第二行
第三行";
        let c = chunks(text, 1000);
        assert_eq!(c.len(), 1);
        assert!(c[0].contains("第二行"));
    }

    #[test]
    fn chunks_splits_long_text() {
        let text: String = (0..200).map(|i| format!("第{i}行
")).collect();
        let c = chunks(&text, 100);
        assert!(c.len() > 3, "100 字符的块应该切出多段");
        // 上限是硬约束：超一个字符都会被服务端截断（旧实现正好超 1 个）
        assert!(
            c.iter().all(|s| s.chars().count() <= 100),
            "块长必须 <= 100，实际有 {}",
            c.iter().map(|s| s.chars().count()).max().unwrap_or(0)
        );
        assert!(c.iter().all(|s| !s.trim().is_empty()), "不能产出空块");
        // 内容不能丢：去掉空白后字符数应对得上
        let joined: String = c.join("");
        let want: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let got: String = joined.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(got, want, "切块不能吞字符");
    }

    #[test]
    fn chunks_never_returns_blank_pieces() {
        // 大量空行 + 短内容：空块会被送进接口白跑一次，甚至让整段失败
        let text = "\n\n\n\nabc\n\n\n\n";
        let c = chunks(text, 10);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0], "abc");
    }

    #[test]
    fn chunks_hard_splits_a_single_long_line() {
        let text = "x".repeat(25);
        let c = chunks(&text, 10);
        assert_eq!(c.len(), 3);
        assert!(c.iter().all(|s| s.chars().count() <= 10));
        assert_eq!(c.concat(), text);
    }

    #[test]
    fn pct_encodes_chinese_and_space() {
        assert_eq!(pct("a b"), "a%20b");
        assert_eq!(pct("你"), "%E4%BD%A0");
    }
}

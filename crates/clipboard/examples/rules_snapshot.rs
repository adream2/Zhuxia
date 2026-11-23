//! 规则与模型的行为快照。
//!
//! 重写 `model.rs` / `rules.rs` 之前先跑一遍存基线，重写后再跑一遍逐字节对比：
//! 这些函数是纯函数，输出必须完全一致，否则列表预览、去重、脱敏都会悄悄变样。
//!
//! ```text
//! cargo run -q -p clipboard --example rules_snapshot > target/snapshot-old.txt
//! ```

use clipboard::model::{self, Entry, Kind};
use clipboard::rules;
use zx_core::ClipAppPolicy;

fn show<T: std::fmt::Debug>(label: &str, value: T) {
    println!("{label}\t{value:?}");
}

fn main() {
    // ---------------- Kind 元数据
    let kinds = [
        Kind::Text,
        Kind::Code,
        Kind::Url,
        Kind::RichText,
        Kind::Image,
        Kind::File,
        Kind::Video,
    ];
    for k in kinds {
        show(
            &format!("kind/{k:?}"),
            (k.as_str(), k.is_text(), k.label(), k.badge()),
        );
    }

    // ---------------- 内容分类
    let texts = [
        "https://example.com/a",
        "www.example.com",
        "http://a",
        "ftp://files/x.zip",
        "not-a/url://x",
        "://no-scheme",
        "今天天气不错",
        "fn main() { let x = 1; }",
        "const x = 1",
        "import os\nprint(1)",
        "#include <stdio.h>\nint main(){return 0;}",
        "{\"a\": 1, \"b\": 2}",
        "{not json}",
        "<div>hi</div>",
        "a; b; c",
        "SELECT * FROM t;",
        "",
        "   ",
        "https://example.com 后面还有中文",
        "See https://x.y/z for details",
        "class Foo { void bar() {} }",
    ];
    for t in texts {
        show(&format!("classify/{t:?}"), rules::detect_content_type(t));
    }

    // ---------------- 路径分类
    for p in [
        "C:/a/b.png",
        "C:/a/b.JPEG",
        "C:/a/b.gif",
        "C:/a/b.webp",
        "C:/a/b.ico",
        "C:/a/b.tiff",
        "C:/a/b.mp4",
        "C:/a/b.MKV",
        "C:/a/b.webm",
        "C:/a/b.txt",
        "C:/a/b",
        "C:/a/b.tar.gz",
    ] {
        show(
            &format!("path/{p}"),
            (rules::classify_path(p), rules::is_image_file(p)),
        );
    }

    // ---------------- HTML
    for h in [
        "<p>第一段</p><p>第二&nbsp;段</p>",
        "<div>a</div><div>b</div>",
        "<table><tr><td>a</td><td>b</td></tr></table>",
        "<h1>标题</h1><br>正文",
        "<img src=\"x.png\">",
        "<img src=\"x.png\"><table></table>",
        "<p></p>",
        "纯文本",
        "第一段\n\n\n\n第二段",
        "<b>bold</b> 与 <i>italic</i>",
    ] {
        show(
            &format!("html/{h:?}"),
            (
                rules::html_to_text(h),
                rules::html_is_image_only(h),
                rules::derive_rich_text("原 content", Some(h)),
            ),
        );
    }

    for e in [
        "&nbsp;&lt;&gt;&quot;&#39;&apos;&amp;",
        "&#65;&#x42;&#x4e2d;",
        "&#123456;",
        "&#xZZ;",
        "没有实体",
    ] {
        show(&format!("entity/{e:?}"), rules::decode_entities(e));
    }

    // ---------------- CF_HTML
    for frag in ["中文片段 <b>加粗</b>", "", "<p>x</p>", "😀 emoji"] {
        let encoded = rules::encode_cf_html(frag);
        show(&format!("cfhtml/len/{frag:?}"), encoded.len());
        show(&format!("cfhtml/head/{frag:?}"), encoded.lines().take(5).collect::<Vec<_>>());
        show(&format!("cfhtml/round/{frag:?}"), rules::parse_cf_html(encoded.as_bytes()));
    }
    show(
        "cfhtml/broken",
        rules::parse_cf_html(b"Version:0.9\r\nStartHTML:0000000105\r\nEndHTML:9999\r\n"),
    );
    show("cfhtml/garbage", rules::parse_cf_html(b""));
    show("cfhtml/plain", rules::parse_cf_html(b"just text"));

    // ---------------- 预览
    for (kind, content, html) in [
        (Kind::Text, "  hi  ", None),
        (Kind::Text, "第一段\n\n第二段", None),
        (Kind::Text, "   ", None),
        (Kind::Text, "a\tb", None),
        (Kind::Code, "fn main() {\n    1\n}", None),
        (Kind::Url, "https://example.com/a", None),
        (Kind::RichText, "原 content", Some("<p>富文本<b>正文</b></p>")),
        (Kind::RichText, "只有内容", None),
        (Kind::RichText, "", Some("<p></p>")),
        (Kind::Image, "/tmp/a.png", None),
        (Kind::Image, "data:image/png;base64,xx", None),
        (Kind::File, "C:/a/报告.pdf", None),
        (Kind::File, "C:/a/1.pdf\nC:/a/2.pdf\nC:/a/3.pdf", None),
        (Kind::Video, "C:/a/v.mp4", None),
        (Kind::Text, &"很长".repeat(200), None),
        (Kind::Text, "前导空格\n中间\n尾随空格  ", None),
    ] {
        show(
            &format!("preview/{kind:?}/{content:.20?}"),
            rules::build_preview(kind, content, html),
        );
    }

    // ---------------- 指纹
    for t in ["a\r\nb", "a\nb", " a", "中文", ""] {
        show(&format!("hash/{t:?}"), rules::text_hash(t));
    }
    show("hash/bytes", rules::bytes_hash(&[0u8, 1, 2, 255]));
    show("hash/empty", rules::bytes_hash(&[]));

    // ---------------- 隐私
    let all = ["url", "phone", "idcard", "email", "secret", "password"]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    for t in [
        "我的手机 13812345678",
        "a.b@example.com",
        "https://x.y/z",
        "110101199003071234",
        "sk-abcdefghijklmnopqrstuvwx",
        "password: 0123456789abcdef",
        "Abcdef1!",
        "abc",
        "普通文字",
        "",
        &"x".repeat(5001),
        "data:image/png;base64,AAAA",
    ] {
        let per = all
            .iter()
            .map(|k| rules::contains_sensitive(t, std::slice::from_ref(k), &[]))
            .collect::<Vec<_>>();
        show(&format!("sensitive/{t:.16?}"), (per, rules::contains_sensitive(t, &all, &[])));
    }
    show(
        "sensitive/custom",
        (
            rules::contains_sensitive("订单 A123", &[], &["A\\d{3}".to_string()]),
            rules::contains_sensitive("订单 B123", &[], &["A\\d{3}".to_string()]),
            rules::contains_sensitive("x", &[], &["(".to_string()]),
        ),
    );

    for t in [
        "手机 13812345678",
        "ab@example.com",
        "110101199003071234",
        "+86 138-1234-5678",
        "没有敏感信息",
    ] {
        show(&format!("mask/{t:?}"), rules::mask_preview(t));
    }

    // ---------------- 清理规则
    for raw in [
        "secret => ***\n> 机密",
        "# action: ignore\n密码\\d+\n手机 => [手机]",
        "# 普通注释\nfoo => bar",
        "   \n\n=> 空模式\n有效 => 替换",
        "a=>1\\nb",
    ] {
        let parsed = rules::parse_cleanup_rules(raw);
        let mut outs = Vec::new();
        for t in ["a secret b", "这是机密", "密码12345", "foo bar", "手机 138", "x\na\nb"] {
            outs.push(rules::apply_cleanup_rules(t, &parsed));
        }
        show(&format!("cleanup/{raw:.14?}"), (parsed.len(), outs));
    }

    // ---------------- 来源策略
    let mut policy = ClipAppPolicy {
        app: "keepass".into(),
        action: "ignore".into(),
        rules: String::new(),
    };
    for (app, path, kind) in [
        ("KeePass.exe", Some("C:/Program Files/KeePass/KeePass.exe"), Kind::Text),
        ("notepad.exe", None, Kind::Text),
        ("keepass", None, Kind::Image),
    ] {
        show(
            &format!("policy/ignore/{app}/{kind:?}"),
            rules::policy_matches(&policy, app, path, kind),
        );
    }
    policy.action = "cleanup".into();
    for kind in [Kind::Text, Kind::Image] {
        show(
            &format!("policy/cleanup/{kind:?}"),
            rules::policy_matches(&policy, "keepass.exe", None, kind),
        );
    }
    policy.app = "  ".into();
    show("policy/blank", rules::policy_matches(&policy, "keepass.exe", None, Kind::Text));

    // ---------------- 记录模型
    let mut e = Entry {
        kind: Kind::File,
        content: "C:/a/报告.pdf\nC:/a/2.pdf\n".into(),
        preview: "preview 兜底".into(),
        ..Default::default()
    };
    show("entry/paths", e.file_paths());
    show("entry/line", e.display_line());
    show("entry/external", e.is_external());
    e.kind = Kind::Image;
    e.content = "data:image/png;base64,AA".into();
    show("entry/image-line", (e.display_line(), e.is_external()));
    e.content = "C:/a/pic.png".into();
    show("entry/image-path", (e.display_line(), e.is_external()));
    e.kind = Kind::Url;
    e.preview = "https://x.y".into();
    show("entry/url-line", e.display_line());

    e.title = "  ".into();
    show("entry/secret-title-empty", e.secret_title());
    e.title = "GitHub Token".into();
    show("entry/secret-title", e.secret_title());
    e.content = "abc".into();
    show("entry/masked-short", e.masked_content());
    e.content = "x".repeat(40);
    show("entry/masked-long", e.masked_content());

    // 含闰年 2 月 29、年初/年末、UTC+8 跨日边界
    for ms in [
        0,
        951_782_400_000,      // 2000-02-29 00:00 UTC+8
        1_709_164_800_000,    // 2024-02-29 00:00 UTC+8
        946_656_000_000,      // 1999-12-31 00:00 UTC+8
        1_764_000_000_000,    // 2025-11-23 前后
        1_700_000_000_000,
        1_600_000_000_000,
        1_750_000_000_000,
        1_764_460_799_000,    // 2025-11-23 23:59:59 UTC+8
    ] {
        show(&format!("civil/{ms}"), model::civil_from_ms(ms));
    }
    show("now/positive", model::now_ms() > 1_700_000_000_000);
}

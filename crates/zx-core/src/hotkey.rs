//! 全局热键的**文本 ↔ 虚拟键码**解析。
//!
//! 各模块都要读 `settings.yaml` 里的 `"Alt+C"` 这种写法，解析规则只该有一份：
//! 语法是 `修饰键+修饰键+主键`（`Ctrl`/`Alt`/`Shift`/`Win`，主键可以是字母、数字、
//! `F1`–`F24` 或功能键名）。没有修饰键的裸键**不给注册**——那会把整台机器的输入抢走。
//!
//! 这里只做解析，不注册：`RegisterHotKey` 绑定线程，注册/注销由各模块在自己的
//! 热键线程里做（见 `crates/clipboard/src/hotkeys.rs`、`crates/capture/src/hotkeys.rs`）。

use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

/// `Ctrl+Alt+1` → (MOD_CONTROL|MOD_ALT, VK_1)。解析失败返回 None。
pub fn parse(text: &str) -> Option<(HOT_KEY_MODIFIERS, u16)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut key = None;
    for part in text.split('+') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" | "meta" => mods |= MOD_WIN,
            other => key = vk_of(other),
        }
    }
    let vk = key?;
    if mods.0 == 0 {
        return None;
    }
    Some((mods, vk))
}

/// 单个键名 → 虚拟键码。允许裸键解析（界面回显用），注册前由 `parse` 把关。
pub fn vk_of(k: &str) -> Option<u16> {
    let upper = k.to_ascii_uppercase();
    if upper.len() == 1 {
        let c = upper.chars().next()?;
        if c.is_ascii_alphanumeric() {
            return Some(c as u16);
        }
    }
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u16>() {
            if (1..=24).contains(&n) {
                return Some(0x6F + n);
            }
        }
    }
    Some(match upper.as_str() {
        "INSERT" | "INS" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "SPACE" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "ESC" | "ESCAPE" => 0x1B,
        "BACKSPACE" => 0x08,
        "UP" => 0x26,
        "DOWN" => 0x28,
        "LEFT" => 0x25,
        "RIGHT" => 0x27,
        "`" | "BACKQUOTE" => 0xC0,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_combinations() {
        assert_eq!(parse("Alt+C").map(|(m, v)| (m.0, v)), Some((1, b'C' as u16)));
        assert_eq!(parse("Ctrl+Shift+V").map(|(m, v)| (m.0, v)), Some((2 | 4, b'V' as u16)));
        assert_eq!(parse("Ctrl+Alt+1").map(|(m, v)| (m.0, v)), Some((1 | 2, b'1' as u16)));
        assert!(parse("").is_none());
        assert!(parse("F5").is_none());
        assert!(parse("Ctrl+Alt+F5").is_some());
    }
}

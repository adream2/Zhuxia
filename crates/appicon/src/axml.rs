//! Android 二进制 XML（AXML）解析。
//!
//! `AndroidManifest.xml` 在 APK 里不是文本，而是带字符串池的二进制格式；自适应图标
//! 与矢量图标的 XML 也是同一套。这里只做"读元素 + 读属性"，够定位 `android:icon`
//! 与 `<foreground>`，不实现 Android 的资源编译/布局。

use crate::error::IconError;

pub const ATTR_REFERENCE: u8 = 0x01;
pub const ATTR_STRING: u8 = 0x03;
pub const ATTR_INT: u8 = 0x10;
pub const ATTR_INT_BOOLEAN: u8 = 0x12;

#[derive(Clone, Debug)]
pub struct Attr {
    pub name: String,
    pub ns: String,
    pub raw: u32,
    pub dtype: u8,
    pub data: u32,
}

#[derive(Clone, Debug)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<Attr>,
}

#[derive(Clone, Debug)]
pub enum Value {
    Str(String),
    Ref(u32),
    Int(i32),
}

#[derive(Clone, Debug, Default)]
pub struct Doc {
    pub strings: Vec<String>,
    pub elements: Vec<Element>,
}

impl Doc {
    pub fn attr(&self, name: &str) -> Option<Value> {
        let el = self.elements.iter().find(|e| e.name == "application")?;
        el.attrs.iter().find(|a| a.name == name).and_then(|a| self.value(a))
    }

    pub fn value(&self, a: &Attr) -> Option<Value> {
        match a.dtype {
            ATTR_STRING => self.strings.get(a.data as usize).cloned().map(Value::Str),
            ATTR_REFERENCE => Some(Value::Ref(a.data)),
            ATTR_INT | ATTR_INT_BOOLEAN => Some(Value::Int(a.data as i32)),
            _ => {
                if a.raw != u32::MAX {
                    self.strings.get(a.raw as usize).cloned().map(Value::Str)
                } else {
                    None
                }
            }
        }
    }

    /// 找第一个名字匹配的元素（自适应图标的 `<adaptive-icon>` / `<foreground>`）。
    pub fn find(&self, name: &str) -> Option<&Element> {
        self.elements.iter().find(|e| e.name == name)
    }

    /// 指定元素上的 drawable/src 引用（自适应图标优先取 `<foreground>`，
    /// 因为它排在 `<background>` 后面时，按文档顺序会先拿到背景层）。
    pub fn refs_of(&self, name: &str) -> Vec<u32> {
        self.elements
            .iter()
            .filter(|e| e.name.eq_ignore_ascii_case(name))
            .flat_map(|e| {
                e.attrs
                    .iter()
                    .filter(|a| (a.name == "drawable" || a.name == "src") && a.dtype == ATTR_REFERENCE)
                    .map(|a| a.data)
            })
            .collect()
    }

    /// 某元素上所有 `android:drawable` / `android:src` 引用，按出现顺序。
    pub fn drawable_refs(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for e in &self.elements {
            for a in &e.attrs {
                if (a.name == "drawable" || a.name == "src" || a.name == "pathData") && a.dtype == ATTR_REFERENCE {
                    out.push(a.data);
                }
            }
        }
        out
    }
}

pub fn is_axml(data: &[u8]) -> bool {
    data.len() >= 8 && data[0] == 0x03 && data[1] == 0x00
}

pub fn parse(data: &[u8]) -> Result<Doc, IconError> {
    if !is_axml(data) {
        return Err(IconError::Corrupt("不是编译后的二进制 XML".into()));
    }
    let total = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let end = total.min(data.len());
    let mut doc = Doc::default();
    let mut pos = 8usize;
    while pos + 8 <= end {
        let ty = u16::from_le_bytes([data[pos], data[pos + 1]]);
        let hdr = u16::from_le_bytes([data[pos + 2], data[pos + 3]]) as usize;
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]]) as usize;
        if size < 8 || pos + size > end {
            break;
        }
        match ty {
            0x0001 => {
                if let Some(pool) = parse_pool(data, pos, size) {
                    doc.strings = pool;
                }
            }
            0x0102 => {
                if let Some(el) = parse_element(data, pos, hdr, size, &doc.strings) {
                    doc.elements.push(el);
                }
            }
            _ => {}
        }
        pos += size;
    }
    if doc.strings.is_empty() && doc.elements.is_empty() {
        return Err(IconError::Corrupt("AXML 里没有字符串池/元素".into()));
    }
    Ok(doc)
}

fn parse_pool(data: &[u8], base: usize, size: usize) -> Option<Vec<String>> {
    let count = rd32(data, base + 8)? as usize;
    let flags = rd32(data, base + 16)?;
    let strings_start = rd32(data, base + 20)? as usize;
    let utf8 = flags & 0x100 != 0;
    let offsets = base + 28;
    if count > 200_000 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off = rd32(data, offsets + i * 4)? as usize;
        let at = base + strings_start + off;
        if at >= base + size {
            out.push(String::new());
            continue;
        }
        let s = if utf8 {
            read_utf8(data, at, base + size)
        } else {
            read_utf16(data, at, base + size)
        };
        out.push(s.unwrap_or_default());
    }
    Some(out)
}

/// UTF-8 池：先 1–2 字节的 UTF-16 长度，再 1–2 字节的字节长度，然后字节。
fn read_utf8(data: &[u8], at: usize, end: usize) -> Option<String> {
    let mut p = at;
    let (_u16len, n) = read_len8(data, p)?;
    p += n;
    let (blen, n) = read_len8(data, p)?;
    p += n;
    if p + blen > end || p + blen > data.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&data[p..p + blen]).to_string())
}

fn read_len8(data: &[u8], p: usize) -> Option<(usize, usize)> {
    let b = *data.get(p)?;
    if b & 0x80 != 0 {
        Some((((b as usize & 0x7F) << 8) | *data.get(p + 1)? as usize, 2))
    } else {
        Some((b as usize, 1))
    }
}

fn read_utf16(data: &[u8], at: usize, end: usize) -> Option<String> {
    let mut p = at;
    let first = rd16(data, p)? as usize;
    p += 2;
    let len = if first & 0x8000 != 0 {
        let second = rd16(data, p)? as usize;
        p += 2;
        ((first & 0x7FFF) << 16) | second
    } else {
        first
    };
    if len > 100_000 || p + len * 2 > end || p + len * 2 > data.len() {
        return None;
    }
    let s: Vec<u16> = (0..len)
        .map(|i| u16::from_le_bytes([data[p + i * 2], data[p + i * 2 + 1]]))
        .collect();
    Some(String::from_utf16_lossy(&s))
}

fn parse_element(data: &[u8], base: usize, hdr: usize, size: usize, strings: &[String]) -> Option<Element> {
    let ext = base + hdr;
    let ns = rd32(data, ext)? as usize;
    let name = rd32(data, ext + 4)? as usize;
    let attr_start = rd16(data, ext + 8)? as usize;
    let attr_size = rd16(data, ext + 10)? as usize;
    let attr_count = rd16(data, ext + 12)? as usize;
    let _ = ns;
    let name = strings.get(name).cloned().unwrap_or_default();
    let mut attrs = Vec::new();
    let abase = ext + attr_start;
    for i in 0..attr_count.min(256) {
        let a = abase + i * attr_size.max(20);
        if a + 20 > base + size || a + 20 > data.len() {
            break;
        }
        let ans = rd32(data, a)? as usize;
        let an = rd32(data, a + 4)? as usize;
        let raw = rd32(data, a + 8)?;
        let dtype = *data.get(a + 15)?;
        let adata = rd32(data, a + 16)?;
        attrs.push(Attr {
            name: strings.get(an).cloned().unwrap_or_default(),
            ns: strings.get(ans).cloned().unwrap_or_default(),
            raw,
            dtype,
            data: adata,
        });
    }
    Some(Element { name, attrs })
}

fn rd16(d: &[u8], o: usize) -> Option<u16> {
    let s = d.get(o..o + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}
fn rd32(d: &[u8], o: usize) -> Option<u32> {
    let s = d.get(o..o + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

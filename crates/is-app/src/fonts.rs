use std::sync::Arc;

use eframe::egui;

pub fn install(ctx: &egui::Context) {
    let Some((path, index)) = find_cjk() else {
        return;
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };

    let index = clamp_face_index(&bytes, index);
    let mut fonts = egui::FontDefinitions::default();
    let mut data = egui::FontData::from_owned(bytes);
    data.index = index;
    fonts.font_data.insert("cjk".to_owned(), Arc::new(data));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}

#[cfg(unix)]
fn find_cjk() -> Option<(String, u32)> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}\t%{index}", "Noto Sans CJK SC"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let (file, index) = s.split_once('\t')?;
    if file.is_empty() || !std::path::Path::new(file).exists() {
        return None;
    }
    // fontconfig 把可变字体的命名实例号编在高 16 位，ab_glyph 只认低 16 位
    let raw: u32 = index.trim().parse().unwrap_or(0);
    Some((file.to_string(), raw & 0xFFFF))
}

// Windows 没有 fontconfig，只能按名字在系统字体目录里挨个试。微软雅黑是简中
// 界面的默认字体、Vista 起随所有 SKU 分发，找不到它才轮到后面两个老字体。
// TTC 里第 0 个 face 就是 Regular，不需要 fontconfig 那套索引换算。
#[cfg(windows)]
fn find_cjk() -> Option<(String, u32)> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let dir = std::path::Path::new(&root).join("Fonts");
    ["msyh.ttc", "msyh.ttf", "simhei.ttf", "simsun.ttc"]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
        .map(|p| (p.to_string_lossy().into_owned(), 0))
}

// 索引越界 egui 会 panic，先按 TTC 头里的 numFonts 夹一下
fn clamp_face_index(bytes: &[u8], index: u32) -> u32 {
    if bytes.len() < 12 || &bytes[0..4] != b"ttcf" {
        return 0;
    }
    let num = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    if index < num { index } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_font_index_is_masked() {
        assert_eq!(0x40002u32 & 0xFFFF, 2);
    }

    #[test]
    fn face_index_clamped_to_collection_size() {
        let mut ttc = b"ttcf".to_vec();
        ttc.extend_from_slice(&[0, 2, 0, 0]);
        ttc.extend_from_slice(&5u32.to_be_bytes());
        assert_eq!(clamp_face_index(&ttc, 2), 2);
        assert_eq!(clamp_face_index(&ttc, 9), 0);
        assert_eq!(clamp_face_index(b"\x00\x01\x00\x00abcdefgh", 3), 0);
        assert_eq!(clamp_face_index(b"tt", 1), 0);
    }

    // 找不到就跳过，但要在日志里说出来：找不到中文字体的后果是界面全是豆腐块，
    // 而这条测试静默通过时看不出发生了哪一种。
    #[test]
    fn real_system_font_parses() {
        let Some((path, index)) = find_cjk() else {
            eprintln!("跳过：本机找不到中文字体，界面会退回英文字形");
            return;
        };
        eprintln!("命中字体 {path}（face {index}）");
        let bytes = std::fs::read(&path).expect("读字体");
        let idx = clamp_face_index(&bytes, index);
        assert!(ab_glyph::FontVec::try_from_vec_and_index(bytes, idx).is_ok());
    }
}

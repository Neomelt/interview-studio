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

    #[test]
    fn real_system_font_parses() {
        let Some((path, index)) = find_cjk() else {
            return;
        };
        let bytes = std::fs::read(&path).expect("读字体");
        let idx = clamp_face_index(&bytes, index);
        assert!(ab_glyph::FontVec::try_from_vec_and_index(bytes, idx).is_ok());
    }
}

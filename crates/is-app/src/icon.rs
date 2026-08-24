use std::sync::Arc;

use eframe::egui;

const PNG: &[u8] = include_bytes!("../../../packaging/icons/icon-256.png");

pub fn load() -> Option<Arc<egui::IconData>> {
    let img = image::load_from_memory_with_format(PNG, image::ImageFormat::Png)
        .ok()?
        .to_rgba8();
    let (width, height) = img.dimensions();
    Some(Arc::new(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_decodes() {
        let icon = load().expect("图标解不出来");
        assert_eq!(icon.width, 256);
        assert_eq!(icon.height, 256);
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        // 全透明说明贴错了文件
        assert!(icon.rgba.chunks(4).any(|p| p[3] > 200));
    }
}

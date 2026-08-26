// 不要控制台。默认的 console 子系统会在双击运行时额外弹一个黑框，关掉黑框
// 程序也跟着没了。这个属性在非 Windows 目标上会被忽略。
#![windows_subsystem = "windows"]

mod app;
mod fonts;
mod icon;
mod meters;
mod paths;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: {
            let mut vb = egui::ViewportBuilder::default()
                .with_inner_size([560.0, 640.0])
                .with_min_inner_size([460.0, 520.0])
                .with_app_id("interview-studio")
                .with_title("面试录音");
            if let Some(i) = icon::load() {
                vb = vb.with_icon(i);
            }
            vb
        },
        ..Default::default()
    };
    eframe::run_native(
        "interview-studio",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)) as Box<dyn eframe::App>)),
    )
}

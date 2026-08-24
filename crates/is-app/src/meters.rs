use eframe::egui;

const MIN_DB: f32 = -60.0;
const MAX_DB: f32 = 0.0;

// 低于这个值认为没信号。比 Meter 的 -90 底噪高一些，留出环境噪声的余量。
pub const SILENT_DB: f32 = -70.0;

pub struct MeterBar<'a> {
    label: &'a str,
    db: f32,
    /// 连续静默时长，超过阈值时整条变红并给出提示
    silent_for: Option<std::time::Duration>,
}

impl<'a> MeterBar<'a> {
    pub fn new(label: &'a str, db: f32) -> Self {
        Self {
            label,
            db,
            silent_for: None,
        }
    }

    pub fn silent_for(mut self, d: Option<std::time::Duration>) -> Self {
        self.silent_for = d;
        self
    }
}

impl egui::Widget for MeterBar<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let alarming = self.silent_for.is_some_and(|d| d.as_secs() >= 10);

        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(self.label).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let txt = if self.db <= SILENT_DB {
                        "静音".to_string()
                    } else {
                        format!("{:.0} dB", self.db)
                    };
                    let color = if alarming {
                        egui::Color32::from_rgb(255, 120, 110)
                    } else {
                        egui::Color32::from_gray(150)
                    };
                    ui.label(egui::RichText::new(txt).monospace().color(color));
                });
            });

            let (rect, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
            paint(ui, rect, self.db, alarming);

            if let Some(d) = self.silent_for.filter(|_| alarming) {
                ui.label(
                    egui::RichText::new(format!(
                        "已静音 {} 秒 —— 这条轨可能录不到东西",
                        d.as_secs()
                    ))
                    .small()
                    .color(egui::Color32::from_rgb(255, 140, 130)),
                );
            }
            resp
        })
        .inner
    }
}

fn paint(ui: &egui::Ui, rect: egui::Rect, db: f32, alarming: bool) {
    let p = ui.painter();
    let r = 3.0;
    p.rect_filled(rect, r, egui::Color32::from_rgb(24, 26, 31));

    let frac = ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0);
    if frac > 0.001 {
        let mut filled = rect;
        filled.max.x = rect.min.x + rect.width() * frac;
        p.rect_filled(filled, r, level_color(db, alarming));
    }

    // -12 和 -3 两条刻度，用来判断电平是否合适
    for mark in [-12.0f32, -3.0] {
        let f = (mark - MIN_DB) / (MAX_DB - MIN_DB);
        let x = rect.min.x + rect.width() * f;
        p.line_segment(
            [egui::pos2(x, rect.min.y), egui::pos2(x, rect.max.y)],
            egui::Stroke::new(1.0, egui::Color32::from_black_alpha(90)),
        );
    }

    p.rect_stroke(
        rect,
        r,
        egui::Stroke::new(1.0, egui::Color32::from_gray(52)),
        egui::StrokeKind::Inside,
    );
}

fn level_color(db: f32, alarming: bool) -> egui::Color32 {
    if alarming {
        egui::Color32::from_rgb(150, 55, 55)
    } else if db >= -3.0 {
        egui::Color32::from_rgb(224, 108, 92)
    } else if db >= -12.0 {
        egui::Color32::from_rgb(224, 186, 96)
    } else {
        egui::Color32::from_rgb(104, 186, 128)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_move_through_the_zones() {
        let green = level_color(-30.0, false);
        let amber = level_color(-8.0, false);
        let red = level_color(-1.0, false);
        assert_ne!(green, amber);
        assert_ne!(amber, red);
        assert_ne!(green, red);
    }

    #[test]
    fn alarm_overrides_every_zone() {
        for db in [-40.0, -8.0, -1.0] {
            assert_eq!(level_color(db, true), level_color(-40.0, true));
        }
    }
}

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use eframe::egui;
use is_audio::{Backend, Device, LoopbackSource, Routing};
use is_pipeline::{
    Levels, Meter, RecordConfig, Recording, disk, mix_in_place, probe, track_levels,
};

use crate::meters::{MeterBar, SILENT_DB};

const TICK: Duration = Duration::from_millis(100);

struct Devices {
    mic: Device,
    sink: Device,
    loopback: LoopbackSource,
}

struct Meters {
    mic: Meter,
    sys: Meter,
    mic_silent_since: Option<Instant>,
    sys_silent_since: Option<Instant>,
}

impl Meters {
    fn tick(&mut self) {
        let now = Instant::now();
        for (db, since) in [
            (self.mic.level_db(), &mut self.mic_silent_since),
            (self.sys.level_db(), &mut self.sys_silent_since),
        ] {
            if db <= SILENT_DB {
                since.get_or_insert(now);
            } else {
                *since = None;
            }
        }
    }

    fn mic_silent_for(&self) -> Option<Duration> {
        self.mic_silent_since.map(|t| t.elapsed())
    }

    fn sys_silent_for(&self) -> Option<Duration> {
        self.sys_silent_since.map(|t| t.elapsed())
    }
}

struct Finished {
    path: PathBuf,
    duration: f64,
    mic: Levels,
    sys: Levels,
    mix_peak: f32,
}

enum Stage {
    Idle,
    Recording {
        rec: Recording,
        started: Instant,
    },
    Mixing {
        rx: Receiver<Result<Finished, String>>,
    },
    Done(Finished),
}

pub struct App {
    // 后端留着不只是为了枚举设备：路由不对时要拿它去切默认输出，
    // 而能不能切是平台能力，得问后端而不是在这里写死。
    backend: Option<Box<dyn Backend>>,
    devices: Option<Devices>,
    routing: Option<Routing>,
    meters: Option<Meters>,
    stage: Stage,
    out_dir: PathBuf,
    error: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(visuals());

        let out_dir = crate::paths::recordings_dir();

        let mut app = Self {
            backend: None,
            devices: None,
            routing: None,
            meters: None,
            stage: Stage::Idle,
            out_dir,
            error: None,
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.error = None;
        self.meters = None;

        let backend = match is_audio::default_backend() {
            Ok(b) => b,
            Err(e) => {
                self.backend = None;
                self.error = Some(e.to_string());
                return;
            }
        };

        let resolved = (|| -> is_audio::Result<(Devices, Routing)> {
            let mic = backend.default_source()?;
            let sink = backend.default_sink()?;
            let loopback = backend.loopback_source(&sink)?;
            let routing = backend.check_routing()?;
            Ok((
                Devices {
                    mic,
                    sink,
                    loopback,
                },
                routing,
            ))
        })();

        self.backend = Some(backend);

        match resolved {
            Ok((devices, routing)) => {
                // 设备与路由检查在两个平台上都成立，先落盘；电平表另说。
                match &devices.loopback {
                    LoopbackSource::PulseMonitor(sys_src) => {
                        match (Meter::start(&devices.mic.id), Meter::start(sys_src)) {
                            (Ok(mic), Ok(sys)) => {
                                self.meters = Some(Meters {
                                    mic,
                                    sys,
                                    mic_silent_since: None,
                                    sys_silent_since: None,
                                });
                            }
                            (Err(e), _) | (_, Err(e)) => {
                                self.error = Some(format!("电平表起不来: {e}"))
                            }
                        }
                    }
                    // parec 是 PulseAudio 的工具，Windows 上没有对应物，电平要
                    // 由原生采集顺带算出来。那部分还没实现。
                    LoopbackSource::WasapiLoopback(_) => {
                        self.error =
                            Some("Windows 的电平表还没实现，设备与路由检查照常可用".into());
                    }
                }
                self.devices = Some(devices);
                self.routing = Some(routing);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn start(&mut self) {
        let Some(d) = &self.devices else { return };
        let cfg = RecordConfig {
            mic: d.mic.id.clone(),
            loopback: d.loopback.clone(),
            output: self.out_dir.join(crate::paths::filename_for_now()),
        };
        match Recording::start(&cfg) {
            Ok(rec) => {
                self.error = None;
                self.stage = Stage::Recording {
                    rec,
                    started: Instant::now(),
                };
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn stop(&mut self) {
        let Stage::Recording { rec, .. } = std::mem::replace(&mut self.stage, Stage::Idle) else {
            return;
        };
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(finish(rec));
        });
        self.stage = Stage::Mixing { rx };
    }
}

// 电平在原始双轨上量（轨 0/1），量完再混音——混音后轨号会整体后移一位
fn finish(rec: Recording) -> Result<Finished, String> {
    let path = rec.stop().map_err(|e| e.to_string())?;
    let mic = track_levels(&path, 0).map_err(|e| e.to_string())?;
    let sys = track_levels(&path, 1).map_err(|e| e.to_string())?;
    let report = mix_in_place(&path).map_err(|e| e.to_string())?;
    let duration = probe::duration_secs(&path).unwrap_or(0.0);
    Ok(Finished {
        path,
        duration,
        mic,
        sys,
        mix_peak: report.mix_peak_db,
    })
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.request_repaint_after(TICK);

        if let Some(m) = &mut self.meters {
            m.tick();
        }
        // ffmpeg 中途死掉时界面会一直走秒表，用户毫不知情。每帧探一次。
        if let Stage::Recording { rec, .. } = &mut self.stage
            && !rec.is_alive()
        {
            self.error = Some("录音进程已退出，录音中断了".into());
            self.stop();
        }
        if let Stage::Mixing { rx } = &self.stage
            && let Ok(res) = rx.try_recv()
        {
            self.stage = match res {
                Ok(f) => Stage::Done(f),
                Err(e) => {
                    self.error = Some(e);
                    Stage::Idle
                }
            };
        }

        egui::Panel::top("head").show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading("面试录音");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("重新检查").clicked() {
                        self.refresh();
                    }
                });
            });
            ui.add_space(8.0);
        });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            if let Some(e) = self.error.clone() {
                ui.colored_label(egui::Color32::from_rgb(255, 120, 110), format!("✖ {e}"));
                ui.add_space(8.0);
            }
            self.preflight_card(ui);
            ui.add_space(14.0);
            self.meter_section(ui);
            ui.add_space(18.0);
            self.controls(ui);
            ui.add_space(10.0);
            self.status(ui);
        });
    }
}

impl App {
    fn preflight_card(&mut self, ui: &mut egui::Ui) {
        // refresh() 会重建 backend，不能在还借着它画界面的时候调用，
        // 所以先记下来，出了闭包再执行。
        let mut switched = false;
        let mut err = None;

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("开始前检查").strong());
            ui.add_space(6.0);

            match &self.devices {
                Some(d) => {
                    row(ui, true, "麦克风", &d.mic.to_string());
                    row(ui, true, "系统输出", &d.sink.to_string());
                }
                None => row(ui, false, "设备", "解析不出来"),
            }

            match &self.routing {
                Some(r) => {
                    let ok = r.will_capture_system_audio();
                    row(ui, ok, "路由", &r.summary());
                    if let Routing::AllElsewhere { elsewhere, .. } = r
                        && let Some(target) = elsewhere.first().cloned()
                    {
                        ui.add_space(4.0);
                        // 能不能替用户切是平台能力：Linux 上 pactl 就能改，
                        // Windows 上没有受支持的 API，只能说清楚该去哪儿改。
                        match self.backend.as_deref() {
                            Some(b) if b.can_set_default_sink() => {
                                if ui
                                    .button(format!("改用 {target}"))
                                    .on_hover_text("把系统默认输出切到这台设备，然后重新检查")
                                    .clicked()
                                {
                                    match b.set_default_sink(&target) {
                                        Ok(()) => switched = true,
                                        Err(e) => err = Some(e.to_string()),
                                    }
                                }
                            }
                            _ => {
                                ui.colored_label(
                                    egui::Color32::from_gray(150),
                                    format!(
                                        "到系统的声音设置里把默认输出改成「{target}」，再点「重新检查」"
                                    ),
                                );
                            }
                        }
                    }
                }
                None => row(ui, false, "路由", "还没检查"),
            }
        });

        if switched {
            self.refresh();
        } else if err.is_some() {
            self.error = err;
        }
    }

    fn meter_section(&mut self, ui: &mut egui::Ui) {
        let recording = matches!(self.stage, Stage::Recording { .. });
        match &self.meters {
            Some(m) => {
                let (mic_silent, sys_silent) = if recording {
                    (m.mic_silent_for(), m.sys_silent_for())
                } else {
                    (None, None)
                };
                ui.add(MeterBar::new("我（麦克风）", m.mic.level_db()).silent_for(mic_silent));
                ui.add_space(10.0);
                ui.add(MeterBar::new("对方（系统输出）", m.sys.level_db()).silent_for(sys_silent));
            }
            None => {
                ui.colored_label(egui::Color32::from_gray(120), "电平表未运行");
            }
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let ready = self.devices.is_some();
        ui.vertical_centered(|ui| match &self.stage {
            Stage::Idle | Stage::Done(_) => {
                let btn = egui::Button::new(egui::RichText::new("⏺  开始录音").size(17.0))
                    .min_size(egui::vec2(200.0, 44.0));
                if ui.add_enabled(ready, btn).clicked() {
                    self.start();
                }
            }
            Stage::Recording { started, .. } => {
                let btn = egui::Button::new(
                    egui::RichText::new(format!("⏹  停止　{}", hms(started.elapsed()))).size(17.0),
                )
                .min_size(egui::vec2(200.0, 44.0))
                .fill(egui::Color32::from_rgb(120, 48, 48));
                if ui.add(btn).clicked() {
                    self.stop();
                }
            }
            Stage::Mixing { .. } => {
                ui.add_space(10.0);
                ui.spinner();
                ui.label("正在合成混音轨，原两轨会无损保留…");
            }
        });
    }

    fn status(&mut self, ui: &mut egui::Ui) {
        let dim = egui::Color32::from_gray(130);
        match &self.stage {
            Stage::Recording { rec, .. } => {
                let size = std::fs::metadata(rec.path()).map(|m| m.len()).unwrap_or(0);
                let free = disk::free_bytes(&self.out_dir).unwrap_or(0);
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(dim, disk::human_bytes(size));
                    ui.colored_label(dim, "·");
                    ui.colored_label(dim, format!("剩余 {}", disk::human_bytes(free)));
                });
            }
            Stage::Done(f) => {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new("已保存").strong());
                    ui.add_space(4.0);
                    ui.colored_label(dim, f.path.display().to_string());
                    ui.colored_label(
                        dim,
                        format!(
                            "时长 {}　混音峰值 {:.1} dB",
                            hms(Duration::from_secs_f64(f.duration)),
                            f.mix_peak
                        ),
                    );
                    verdict(ui, "我", f.mic);
                    verdict(ui, "对方", f.sys);
                });
            }
            _ => {
                let free = disk::free_bytes(&self.out_dir).unwrap_or(0);
                ui.colored_label(
                    dim,
                    format!(
                        "{}　剩余 {}",
                        self.out_dir.display(),
                        disk::human_bytes(free)
                    ),
                );
            }
        }
    }
}

fn row(ui: &mut egui::Ui, ok: bool, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        let (mark, color) = if ok {
            ("✔", egui::Color32::from_rgb(120, 200, 140))
        } else {
            ("✖", egui::Color32::from_rgb(255, 140, 130))
        };
        ui.colored_label(color, mark);
        ui.add_sized([76.0, 18.0], egui::Label::new(label));
        ui.colored_label(egui::Color32::from_gray(170), value);
    });
}

fn verdict(ui: &mut egui::Ui, who: &str, l: Levels) {
    let (text, color) = if l.is_silent() {
        (
            "全程静音".to_string(),
            egui::Color32::from_rgb(255, 130, 120),
        )
    } else if l.is_weak() {
        (
            format!("{:.0} dB 偏弱", l.mean_db),
            egui::Color32::from_rgb(224, 186, 96),
        )
    } else {
        (
            format!("{:.0} dB", l.mean_db),
            egui::Color32::from_rgb(120, 200, 140),
        )
    };
    ui.colored_label(color, format!("{who}　{text}"));
}

fn hms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
}

fn visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = egui::Color32::from_rgb(21, 23, 27);
    v.window_fill = egui::Color32::from_rgb(26, 29, 34);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hms_formats_hours_minutes_seconds() {
        assert_eq!(hms(Duration::from_secs(0)), "00:00:00");
        assert_eq!(hms(Duration::from_secs(59)), "00:00:59");
        assert_eq!(hms(Duration::from_secs(3661)), "01:01:01");
        assert_eq!(hms(Duration::from_secs(39 * 60 + 2)), "00:39:02");
    }
}

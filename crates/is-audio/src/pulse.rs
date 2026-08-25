// 用 pactl 而不是 cpal：cpal 默认走 ALSA，看不到 monitor 源。

use std::process::Command;

use crate::{Backend, Device, Error, LoopbackSource, Result};

pub struct PulseBackend;

impl PulseBackend {
    pub fn new() -> Result<Self> {
        pactl(&["info"])?;
        Ok(Self)
    }
}

fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl")
        .args(args)
        .output()
        .map_err(|e| Error::Unavailable(format!("跑不了 pactl: {e}")))?;
    if !out.status.success() {
        return Err(Error::Unavailable(format!(
            "pactl {} 失败: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn info_field(field: &str) -> Result<String> {
    let text = pactl(&["info"])?;
    text.lines()
        .find_map(|l| l.strip_prefix(field).map(|v| v.trim().to_string()))
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Parse(format!("pactl info 里没有 {field}")))
}

fn descriptions(kind: &str) -> Result<Vec<(String, String)>> {
    let text = pactl(&["list", kind])?;
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        let t = line.trim();
        if let Some(v) = t.strip_prefix("Name: ") {
            name = Some(v.trim().to_string());
        } else if let Some(v) = t.strip_prefix("Description: ")
            && let Some(n) = name.take()
        {
            out.push((n, v.trim().to_string()));
        }
    }
    Ok(out)
}

// index \t name \t driver \t format \t state
fn short_index_name(kind: &str) -> Result<Vec<(String, String)>> {
    let text = pactl(&["list", "short", kind])?;
    Ok(text
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?.trim().to_string(), f.next()?.trim().to_string()))
        })
        .filter(|(i, n)| !i.is_empty() && !n.is_empty())
        .collect())
}

fn devices(kind: &str) -> Result<Vec<Device>> {
    let desc = descriptions(kind)?;
    Ok(short_index_name(kind)?
        .into_iter()
        .map(|(_, id)| {
            let description = desc
                .iter()
                .find(|(n, _)| *n == id)
                .map(|(_, d)| d.clone())
                .unwrap_or_default();
            Device { id, description }
        })
        .collect())
}

fn device_by_id(kind: &str, id: &str) -> Result<Device> {
    devices(kind)?
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| Error::Parse(format!("{kind} 里找不到 {id}")))
}

impl Backend for PulseBackend {
    fn default_sink(&self) -> Result<Device> {
        device_by_id("sinks", &info_field("Default Sink:")?)
    }

    fn default_source(&self) -> Result<Device> {
        device_by_id("sources", &info_field("Default Source:")?)
    }

    fn sinks(&self) -> Result<Vec<Device>> {
        devices("sinks")
    }

    fn sources(&self) -> Result<Vec<Device>> {
        devices("sources")
    }

    // 第 2 列是流所在的 sink 编号：input-index \t sink-index \t client \t ...
    fn active_sinks(&self) -> Result<Vec<Device>> {
        let text = pactl(&["list", "short", "sink-inputs"])?;
        let mut sink_indices: Vec<String> = text
            .lines()
            .filter_map(|l| l.split('\t').nth(1).map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect();
        sink_indices.sort();
        sink_indices.dedup();

        let idx_to_name = short_index_name("sinks")?;
        let all = self.sinks()?;
        Ok(sink_indices
            .iter()
            .filter_map(|i| idx_to_name.iter().find(|(idx, _)| idx == i).map(|(_, n)| n))
            .filter_map(|name| all.iter().find(|d| d.id == *name).cloned())
            .collect())
    }

    fn can_set_default_sink(&self) -> bool {
        true
    }

    fn set_default_sink(&self, sink: &Device) -> Result<()> {
        pactl(&["set-default-sink", &sink.id]).map(|_| ())
    }

    fn loopback_source(&self, sink: &Device) -> Result<LoopbackSource> {
        // 有些虚拟 sink 不带 monitor，拼出来的名字打不开，必须确认存在
        let monitor = format!("{}.monitor", sink.id);
        let exists = short_index_name("sources")?
            .iter()
            .any(|(_, n)| *n == monitor);
        if exists {
            Ok(LoopbackSource::PulseMonitor(monitor))
        } else {
            Err(Error::Unavailable(format!(
                "{sink} 没有 monitor 源（{monitor}），录不到它的声音"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> Option<PulseBackend> {
        PulseBackend::new().ok()
    }

    macro_rules! require {
        ($b:ident) => {
            let Some($b) = backend() else {
                eprintln!("跳过：本机没有可用的 PulseAudio/PipeWire");
                return;
            };
        };
    }

    #[test]
    fn enumerates_real_devices() {
        require!(b);
        let sinks = b.sinks().expect("列输出设备");
        assert!(!sinks.is_empty(), "至少该有一个输出设备");
        for s in &sinks {
            assert!(!s.id.is_empty());
            assert!(!s.description.is_empty(), "{} 没有 Description", s.id);
        }
    }

    #[test]
    fn default_devices_are_in_the_list() {
        require!(b);
        let d = b.default_sink().expect("默认输出");
        assert!(b.sinks().unwrap().iter().any(|s| s.id == d.id));
        let s = b.default_source().expect("默认输入");
        assert!(b.sources().unwrap().iter().any(|x| x.id == s.id));
    }

    #[test]
    fn every_sink_resolves_a_monitor() {
        require!(b);
        for sink in b.sinks().unwrap() {
            match b.loopback_source(&sink) {
                Ok(LoopbackSource::PulseMonitor(m)) => {
                    assert_eq!(m, format!("{}.monitor", sink.id))
                }
                Ok(other) => panic!("Linux 上不该返回 {other:?}"),
                Err(e) => panic!("{sink} 解析 monitor 失败: {e}"),
            }
        }
    }

    #[test]
    fn active_sinks_are_a_subset_of_all_sinks() {
        require!(b);
        let all = b.sinks().unwrap();
        for a in b.active_sinks().unwrap() {
            assert!(all.iter().any(|s| s.id == a.id), "{a} 不在设备列表里");
        }
    }

    #[test]
    fn routing_check_runs_and_explains_itself() {
        require!(b);
        let r = b.check_routing().expect("路由检查");
        let s = r.summary();
        assert!(!s.is_empty());
        eprintln!("本机路由结论: {s}");
    }
}

// 用 MMDevice + AudioSession 而不是 cpal：cpal 只给得出设备列表，看不到「哪台
// 设备上真的有流在播」——而那恰好是预检唯一要回答的问题。

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::Audio::{
    AudioSessionStateActive, DEVICE_STATE_ACTIVE, EDataFlow, IAudioSessionControl,
    IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    STGM_READ,
};
use windows::core::{Interface, PCWSTR};

use crate::{Backend, Device, Error, LoopbackSource, Result};

pub struct WasapiBackend {
    enumerator: IMMDeviceEnumerator,
}

impl WasapiBackend {
    pub fn new() -> Result<Self> {
        // eframe/winit 已经在主线程上初始化过 COM。重复调用返回 S_FALSE，用另一种
        // 套间模式调用返回 RPC_E_CHANGED_MODE——两者都说明 COM 可用，都不是错误，
        // 所以这里不看返回值。真正的失败会在 CoCreateInstance 上暴露出来。
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|e| Error::Unavailable(format!("拿不到音频设备枚举器: {e}")))?;
        Ok(Self { enumerator })
    }

    fn endpoints(&self, flow: EDataFlow) -> Result<Vec<Device>> {
        let coll = unsafe {
            self.enumerator
                .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
        }
        .map_err(failed("枚举音频端点"))?;
        let n = unsafe { coll.GetCount() }.map_err(failed("取端点数量"))?;
        (0..n)
            .map(|i| {
                let d = unsafe { coll.Item(i) }.map_err(failed("取端点"))?;
                describe(&d)
            })
            .collect()
    }

    fn default_endpoint(&self, flow: EDataFlow) -> Result<Device> {
        let d = unsafe { self.enumerator.GetDefaultAudioEndpoint(flow, eConsole) }
            .map_err(|e| Error::Unavailable(format!("没有默认{}设备: {e}", flow_name(flow))))?;
        describe(&d)
    }

    fn device_by_id(&self, id: &str) -> Result<IMMDevice> {
        let wide = wide(id);
        unsafe { self.enumerator.GetDevice(PCWSTR(wide.as_ptr())) }
            .map_err(|e| Error::Parse(format!("找不到设备 {id}: {e}")))
    }

    // 一台渲染端点上只要有一个会话处于 Active，就算它在出声。对应 Linux 那边的
    // pactl list short sink-inputs。
    fn has_active_session(&self, id: &str) -> Result<bool> {
        let dev = self.device_by_id(id)?;
        let mgr: IAudioSessionManager2 =
            unsafe { dev.Activate(CLSCTX_ALL, None) }.map_err(failed("打开会话管理器"))?;
        let sessions = unsafe { mgr.GetSessionEnumerator() }.map_err(failed("枚举音频会话"))?;
        let n = unsafe { sessions.GetCount() }.map_err(failed("取会话数量"))?;

        for i in 0..n {
            // 单个会话读不出来就跳过：会话随时可能在枚举过程中消失，
            // 不该让整次预检失败。
            let Ok(ctl) = (unsafe { sessions.GetSession(i) }) else {
                continue;
            };
            // AudioSessionStateActive 是常量不是枚举变体，写进 matches! 的模式
            // 位置会被当成绑定名，那个分支就永远命中了。必须用等号比。
            if unsafe { ctl.GetState() }.is_ok_and(|s| s == AudioSessionStateActive)
                && !is_system_sounds(&ctl)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

// 系统音效（通知音）不算「有人在这台设备上放东西」。它是一个常驻会话，
// 响一声就会短暂变成 Active，会让预检在错误的时刻给出绿灯。
fn is_system_sounds(ctl: &IAudioSessionControl) -> bool {
    let Ok(c2) = ctl.cast::<IAudioSessionControl2>() else {
        return false;
    };
    // 这个方法用返回码区分是与否：S_OK 表示是，S_FALSE 表示不是。两者都算
    // 「成功」，所以必须比 HRESULT 本身，不能用 is_ok()。
    (unsafe { c2.IsSystemSoundsSession() }) == S_OK
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn failed(what: &'static str) -> impl Fn(windows::core::Error) -> Error {
    move |e| Error::Unavailable(format!("{what}失败: {e}"))
}

fn flow_name(flow: EDataFlow) -> &'static str {
    if flow == eRender { "输出" } else { "输入" }
}

// 端点 ID 形如 {0.0.0.00000000}.{guid}，是跨重启稳定的标识，用来做 id；
// 友好名给人看。取不到友好名不致命，Display 会退回显示 id。
fn describe(d: &IMMDevice) -> Result<Device> {
    Ok(Device {
        id: endpoint_id(d)?,
        description: friendly_name(d),
    })
}

fn endpoint_id(d: &IMMDevice) -> Result<String> {
    let pw = unsafe { d.GetId() }.map_err(failed("取设备 ID"))?;
    // Safety: GetId 成功时返回一枚 COM 分配的 NUL 结尾宽字符串，
    // 读完由调用方释放，正是下面这一次。
    let s = unsafe { pw.to_string() }
        .map_err(|e| Error::Parse(format!("设备 ID 不是合法 UTF-16: {e}")))?;
    unsafe { CoTaskMemFree(Some(pw.0.cast())) };
    Ok(s)
}

fn friendly_name(d: &IMMDevice) -> String {
    unsafe {
        d.OpenPropertyStore(STGM_READ)
            .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
            .map(|v| v.to_string())
            .unwrap_or_default()
    }
}

impl Backend for WasapiBackend {
    fn default_sink(&self) -> Result<Device> {
        self.default_endpoint(eRender)
    }

    fn default_source(&self) -> Result<Device> {
        self.default_endpoint(eCapture)
    }

    fn sinks(&self) -> Result<Vec<Device>> {
        self.endpoints(eRender)
    }

    fn sources(&self) -> Result<Vec<Device>> {
        self.endpoints(eCapture)
    }

    fn active_sinks(&self) -> Result<Vec<Device>> {
        let mut out = Vec::new();
        for dev in self.sinks()? {
            if self.has_active_session(&dev.id)? {
                out.push(dev);
            }
        }
        Ok(out)
    }

    // 不像 PulseAudio 需要一个单独存在的 .monitor 源，Windows 上任何渲染端点都能
    // 用 AUDCLNT_STREAMFLAGS_LOOPBACK 打开。这里只确认 ID 还解析得到设备——
    // 设备可能在预检之后被拔掉。
    fn loopback_source(&self, sink: &Device) -> Result<LoopbackSource> {
        self.device_by_id(&sink.id)?;
        Ok(LoopbackSource::WasapiLoopback(sink.id.clone()))
    }

    // Windows 没有受支持的「设置默认音频设备」API。IPolicyConfig 是未文档化的
    // 内部接口，其 IID 与方法顺序在各版本间变过，不能拿去赌用户的录音。
    fn can_set_default_sink(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 这条在 CI 那台没有声卡的 windows runner 上也会真跑：枚举器本身不依赖
    // 端点存在，没有设备时要给出干净的错误而不是 panic 或空 id。
    #[test]
    fn enumerator_works_even_with_no_devices() {
        let b = WasapiBackend::new().expect("COM 枚举器应当创建成功");
        let sinks = b.sinks().expect("列端点不该失败，哪怕一个都没有");
        let sources = b.sources().expect("列端点不该失败，哪怕一个都没有");
        eprintln!("输出端点 {} 个，输入端点 {} 个", sinks.len(), sources.len());

        match b.default_sink() {
            Ok(d) => assert!(!d.id.is_empty(), "默认输出的 id 不该为空"),
            Err(Error::Unavailable(m)) => eprintln!("没有默认输出（无声卡环境的预期结果）: {m}"),
            Err(e) => panic!("错误类型不对，应当是 Unavailable: {e}"),
        }
    }

    #[test]
    fn cannot_switch_default_sink_on_windows() {
        let b = WasapiBackend::new().expect("COM 枚举器应当创建成功");
        assert!(!b.can_set_default_sink());
        let dev = Device {
            id: "whatever".into(),
            description: String::new(),
        };
        assert!(b.set_default_sink(&dev).is_err(), "不该假装切成功了");
    }

    macro_rules! require_devices {
        ($b:ident) => {
            let Ok($b) = WasapiBackend::new() else {
                eprintln!("跳过：拿不到 WASAPI 设备枚举器");
                return;
            };
            if $b.sinks().map(|s| s.is_empty()).unwrap_or(true) {
                eprintln!("跳过：本机没有可用的音频输出端点");
                return;
            }
        };
    }

    #[test]
    fn enumerates_real_devices() {
        require_devices!(b);
        for s in b.sinks().unwrap() {
            assert!(!s.id.is_empty());
            assert!(!s.description.is_empty(), "{} 没有友好名", s.id);
        }
    }

    #[test]
    fn default_devices_are_in_the_list() {
        require_devices!(b);
        let d = b.default_sink().expect("默认输出");
        assert!(b.sinks().unwrap().iter().any(|s| s.id == d.id));
        let s = b.default_source().expect("默认输入");
        assert!(b.sources().unwrap().iter().any(|x| x.id == s.id));
    }

    #[test]
    fn every_sink_can_be_loopback_captured() {
        require_devices!(b);
        for sink in b.sinks().unwrap() {
            match b.loopback_source(&sink) {
                Ok(LoopbackSource::WasapiLoopback(id)) => assert_eq!(id, sink.id),
                Ok(other) => panic!("Windows 上不该返回 {other:?}"),
                Err(e) => panic!("{sink} 解析 loopback 失败: {e}"),
            }
        }
    }

    #[test]
    fn active_sinks_are_a_subset_of_all_sinks() {
        require_devices!(b);
        let all = b.sinks().unwrap();
        for a in b.active_sinks().unwrap() {
            assert!(all.iter().any(|s| s.id == a.id), "{a} 不在设备列表里");
        }
    }

    #[test]
    fn routing_check_runs_and_explains_itself() {
        require_devices!(b);
        let r = b.check_routing().expect("路由检查");
        assert!(!r.summary().is_empty());
        eprintln!("本机路由结论: {}", r.summary());
    }
}

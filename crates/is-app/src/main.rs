use is_audio::{Backend, pulse::PulseBackend};

fn main() -> std::process::ExitCode {
    match preflight() {
        Ok(ok) => {
            if ok {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("检查失败: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn preflight() -> is_audio::Result<bool> {
    let backend = PulseBackend::new()?;

    let mic = backend.default_source()?;
    let sink = backend.default_sink()?;
    println!("麦克风    {mic}");
    println!("系统输出  {sink}");

    match backend.loopback_source(&sink) {
        Ok(src) => println!("采集源    {src:?}"),
        Err(e) => {
            println!("采集源    ✗ {e}");
            return Ok(false);
        }
    }

    let routing = backend.check_routing()?;
    println!("路由      {}", routing.summary());
    Ok(routing.will_capture_system_audio())
}

## Windows support

Recording, live meters, device enumeration and the routing preflight now work on
Windows, via WASAPI. Capture is native — FFmpeg has no loopback capture device on
Windows — but encoding and muxing still go through the same `ffmpeg`, so the
output file has the same shape on both platforms.

`.msi` and portable `.zip` both bundle `ffmpeg.exe` / `ffprobe.exe`
([LGPL builds](https://github.com/BtbN/FFmpeg-Builds), licence included), so
there is nothing else to install.

### What is verified, and what is not

CI builds, lints and tests both platforms, and on Windows it checks that the
bundled FFmpeg actually runs and produces a dual-track MKV from two raw PCM
streams — exactly what stopping a recording does.

CI runners have **no sound card**, so on Windows the following have not been
exercised against real audio hardware yet:

- device enumeration and the routing preflight
- WASAPI capture (microphone and system loopback)
- record → stop → mix, end to end

Those paths are covered by unit tests for their pure logic (silence
reconstruction, sample conversion, argument construction) and by tests that skip
themselves when no device is present. Please report what happens on your machine.

Linux is unchanged and its end-to-end recording test still passes.

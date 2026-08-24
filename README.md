# Interview Studio

Dual-track recorder for online interviews and meetings. Your microphone and the
other side's audio are captured as **two separate tracks**, plus a mixed track so
that double-clicking the file plays both voices.

Two separate tracks make speaker separation free — no diarization needed.
Transcribe each track independently and you get a role-labelled dialogue.

Works with any conferencing software (Zoom, Teams, Meet, Tencent Meeting, or a
browser tab), because it captures the system audio device rather than hooking
into a particular application.

> Status: early development. Linux only for now.

## Install

Grab a package from the [releases page](../../releases).

```sh
# Fedora / RHEL
sudo dnf install ./interview-studio-*.x86_64.rpm

# Debian / Ubuntu
sudo apt install ./interview-studio_*_amd64.deb

# Or run the binary directly
tar xzf interview-studio-*-x86_64-linux.tar.gz
./interview-studio
```

Verify the download against `SHA256SUMS` published with the release:

```sh
sha256sum -c SHA256SUMS --ignore-missing
```

### Requirements

| | |
| --- | --- |
| `ffmpeg` | recording, mixing, level measurement |
| `pactl`, `parec` | device enumeration and live meters (`pulseaudio-utils`) |
| PipeWire or PulseAudio | the audio server itself |

The packages declare these; the tarball does not, so install them yourself.

## Using it

Recordings land in `<your music folder>/InterviewStudio/`, named by timestamp.

1. Open the app **before** the call and let the other side make some sound
2. Check the preflight card — all three rows must be green
3. Watch both meters move; that is your proof both sides are being captured
4. Record, then stop; the mixed track is added automatically

## Why two tracks

Most "record the meeting" setups capture only the microphone, so the other
person is missing. Capturing system output as well is possible on every desktop
OS, but the mechanism differs:

| Platform | System audio capture | Status |
| --- | --- | --- |
| Linux | PulseAudio / PipeWire `.monitor` source | implemented |
| Windows | WASAPI loopback | planned |
| macOS | CoreAudio tap (14.6+) | planned |

`is-audio::Backend` abstracts over this.

## The failure mode this guards against

PipeWire remembers an output device *per application*. The default output can be
your headset while the meeting app is pinned to HDMI — the recording then
captures the default device's monitor and the other side's track ends up
**silent**, while everything sounds fine in your ears.

The preflight check compares the default sink against the sinks that actually
carry audio streams, and refuses to give a green light when they disagree. The
live meters keep answering the same question for the whole call.

## Build from source

```sh
cargo build --release
cargo test --workspace

# tests that record audio or make sound are opt-in
cargo test --workspace -- --ignored
```

Packaging:

```sh
cargo build --release --bin interview-studio
cargo deb -p is-app --no-build
cargo generate-rpm -p crates/is-app
```

## Layout

```
crates/
├── is-audio/     device enumeration, routing detection, backend trait
├── is-pipeline/  recording, mixing, level metering, probing
└── is-app/       egui front end
```

## License

MIT — see [LICENSE](LICENSE).

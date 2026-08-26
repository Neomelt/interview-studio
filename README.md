# Interview Studio

Dual-track recorder for online interviews and meetings. Your microphone and the
other side's audio are captured as **two separate tracks**, plus a mixed track so
that double-clicking the file plays both voices.

Two separate tracks make speaker separation free — no diarization needed.
Transcribe each track independently and you get a role-labelled dialogue.

Works with any conferencing software (Zoom, Teams, Meet, Tencent Meeting, or a
browser tab), because it captures the system audio device rather than hooking
into a particular application.

> Status: early development. Linux and Windows.
>
> The Windows build has not yet been exercised on real audio hardware — CI
> runners have no sound card, so device enumeration, routing detection and
> capture are verified only by construction and by tests that skip without
> devices. Treat 0.2.0 on Windows as needing a first real run. See
> [Verification status](#verification-status).

## Install

Grab a package from the [releases page](../../releases).

### Linux

```sh
# Fedora / RHEL
sudo dnf install ./interview-studio-*.x86_64.rpm

# Debian / Ubuntu
sudo apt install ./interview-studio_*_amd64.deb

# Or run the binary directly
tar xzf interview-studio-*-x86_64-linux.tar.gz
./interview-studio
```

### Windows

Run the `.msi`, or unzip the portable `.zip` and run `interview-studio.exe`.
Both bundle `ffmpeg.exe`/`ffprobe.exe`, so there is nothing else to install.

Verify the download against `SHA256SUMS` published with the release:

```sh
sha256sum -c SHA256SUMS --ignore-missing
```

### Requirements

| | Linux | Windows |
| --- | --- | --- |
| Audio server | PipeWire or PulseAudio | WASAPI (built in) |
| Device enumeration | `pactl` (`pulseaudio-utils`) | built in |
| Live meters | `parec` (`pulseaudio-utils`) | built in |
| Encoding, mixing, probing | `ffmpeg` | bundled |

The Linux packages declare these dependencies; the tarball does not, so install
them yourself. The Windows packages bundle everything.

## Using it

Recordings land in `<your music folder>/InterviewStudio/`, named by timestamp.

1. Open the app **before** the call and let the other side make some sound
2. Check the preflight card — all three rows must be green
3. Watch both meters move; that is your proof both sides are being captured
4. Record, then stop; the mixed track is added automatically

**Use headphones.** On speakers, your microphone also picks up the other side,
so the two tracks stop being independent and the whole point is lost. This is
the one thing that silently degrades the recording without any error showing up.

The mixed track (track 1) is a convenience for double-clicking. Track 2 and
track 3 are the untouched microphone and system recordings — those are what you
feed to a transcriber, and what to listen to if the mix sounds off.

## Why two tracks

Most "record the meeting" setups capture only the microphone, so the other
person is missing. Capturing system output as well is possible on every desktop
OS, but the mechanism differs:

| Platform | System audio capture | Status |
| --- | --- | --- |
| Linux | PulseAudio / PipeWire `.monitor` source | implemented |
| Windows | WASAPI loopback | implemented |
| macOS | CoreAudio tap (14.6+) | planned |

`is-audio::Backend` abstracts over this.

## The failure mode this guards against

PipeWire remembers an output device *per application*. The default output can be
your headset while the meeting app is pinned to HDMI — the recording then
captures the default device's monitor and the other side's track ends up
**silent**, while everything sounds fine in your ears. Windows has the same trap
under Settings → System → Sound → Volume mixer.

The preflight check compares the default output against the outputs that
actually carry audio streams, and refuses to give a green light when they
disagree. The live meters keep answering the same question for the whole call.

## How Windows differs under the hood

FFmpeg has no loopback capture device on Windows — only `dshow`, which needs the
driver to expose a "Stereo Mix" endpoint that most modern machines do not have.
So capture is done natively against WASAPI, written to raw PCM, and handed to
the same `ffmpeg` at stop time for encoding and muxing. The output file is
therefore byte-for-byte the same *shape* on both platforms, and everything
downstream (mixing, probing, level measurement) is shared.

One WASAPI detail worth knowing: loopback capture emits **no packets at all**
while the render endpoint is idle, rather than emitting silence. Silence has to
be reconstructed from the capture timestamps, otherwise quiet stretches are
dropped, the track gets shorter, and the two tracks drift apart.

## How the mixed track is balanced

Summing both sides at full gain means a loud side buries a quiet one — music at
-12 dBFS against speech at -30 dBFS leaves the speech inaudible, which defeats
the only reason that track exists. So before mixing, both tracks are measured
and the louder one is turned down until the gap is within 6 dB (never more than
18 dB of cut), then a shared make-up gain (at most 12 dB) brings the level back
up. Turning the loud side down rather than the quiet side up avoids amplifying
the noise floor that usually made it quiet in the first place.

The two original tracks are copied without re-encoding and are verified
sample-for-sample against the source, so nothing here touches them.

## Verification status

| | Linux | Windows |
| --- | --- | --- |
| Build, lint, unit tests | CI | CI |
| Package builds | CI | CI |
| Bundled ffmpeg runs, produces a dual-track MKV | n/a | CI |
| Device enumeration and routing on real hardware | yes | **not yet** |
| Record → stop → mix end to end | yes (`--ignored` test) | **not yet** |

The tests that need audio devices skip themselves when none are present, and CI
runs with `--nocapture` so a skip is visible in the log rather than looking like
a pass.

## Build from source

```sh
cargo build --release
cargo test --workspace

# tests that record audio or make sound are opt-in
cargo test --workspace -- --ignored
```

Packaging:

```sh
# Linux
cargo build --release --bin interview-studio
cargo deb -p is-app --no-build
cargo generate-rpm -p crates/is-app

# Windows (needs the WiX v3 toolset)
cargo build --release --bin interview-studio
./scripts/find-wix.ps1
./scripts/windows-package.ps1 -Version 0.2.1
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

The Windows packages redistribute unmodified FFmpeg binaries built by
[BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) under the LGPL v2.1
or later; the licence text ships alongside them as `LICENSE-ffmpeg.txt`.

# Interview Studio

Dual-track recorder for online interviews and meetings: your microphone and the
other side's audio are captured as **two separate tracks**, plus a mixed track so
that simply double-clicking the file plays both voices.

Two separate tracks mean speaker separation is free — no diarization needed.
Transcribing each track independently yields a role-labelled dialogue.

> Status: early development.

## Why two tracks

Most "record the meeting" setups capture only the microphone, so the other
person is missing. Capturing system output as well is possible on every desktop
OS, but the mechanism differs:

| Platform | System audio capture |
| --- | --- |
| Linux | PulseAudio / PipeWire `.monitor` source |
| Windows | WASAPI loopback |
| macOS | CoreAudio tap (14.6+) |

`is-audio::Backend` abstracts over this. Only the Linux backend exists today.

## The failure mode this guards against

PipeWire remembers an output device *per application*. So the default output can
be your headset while the meeting app is pinned to HDMI — the recording captures
the default device's monitor and the other side's track ends up **silent**, while
everything sounds fine in your ears.

The preflight check catches exactly this: it compares the default sink against
the sinks that actually have audio streams, and refuses to give a green light
when they disagree.

## Build

```sh
cargo build --release
cargo test --workspace
```

Runtime dependencies: `ffmpeg`, and `pactl` (from `pulseaudio-utils`).

## Layout

```
crates/
├── is-audio/   device enumeration, routing detection, backend trait
└── is-app/     application entry point
```

## License

MIT

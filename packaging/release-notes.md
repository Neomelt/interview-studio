Fixes for everything that turned up on the first real Windows run.

### Windows

- **No more console window.** The app was built for the console subsystem, so
  double-clicking it opened a black box alongside the window. Its FFmpeg
  subprocesses would have flashed their own console windows too — a dozen per
  recording — so those are now created without one.
- **Real application icon**, embedded in the executable, so the taskbar, Alt-Tab
  and Explorer show it. The runtime icon only ever applied to the window itself.
- **The installer now asks.** You can choose the install location, and start
  menu and desktop shortcuts are separate options you can decline. Previously
  there was no shortcut at all and the app could only be launched by searching
  for it. The installer UI is also in Chinese now.

### Both platforms

- **The mixed track is balanced.** It used to be a straight full-gain sum of both
  sides, so a loud side buried a quiet one — playing music while talking left the
  speech inaudible in the default track. The louder side is now turned down until
  the two are within 6 dB, followed by a shared make-up gain. The two original
  tracks are untouched and still verified sample-for-sample.

If a recording still sounds like one side is missing, check the two original
tracks (2 and 3) rather than the mix, and use headphones — on speakers the
microphone picks up the other side as well, which is a recording-setup problem
no amount of mixing can undo.

### Still not verified on real hardware

WASAPI capture and the routing preflight are exercised by unit tests and by CI,
but CI runners have no sound card, so a real end-to-end recording on Windows is
still down to you. The 0.2.0 notes have the details.

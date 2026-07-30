# Claude Code session prompts

Paste one per session. Do not combine them — each is scoped to roughly one
context window, and merging two means the second gets built badly.

Put `CLAUDE.md` in the repo root first. Claude Code loads it automatically, so
these prompts stay short.

---

## Session 0 — Scaffold and adb handshake

```
Read CLAUDE.md.

Set up the repo skeleton exactly as laid out in the "Repo layout" section:
directories, Cargo.toml, .gitignore, Apache 2.0 LICENSE, and stub README.md.

Then build the adb layer in host/src/adb.rs:
- locate the adb binary (PATH, then ANDROID_HOME/platform-tools)
- list connected devices, error clearly if zero or if more than one and no
  --serial was given
- push an arbitrary file to /data/local/tmp
- open a reverse tunnel (adb reverse localabstract:<name> tcp:<port>)
- tear the tunnel down on drop, including on panic and Ctrl-C

Wire up clap with: --serial, --port, --verbose.

Acceptance: `cargo run -- devices` prints my connected phone. `cargo run --
test-tunnel` opens a tunnel, prints confirmation, and removes it on exit —
verified by `adb reverse --list` being empty afterwards.

Do not write any video code this session. Stop when acceptance passes and
report.
```

---

## Session 1 — Naive mirror (throwaway, on purpose)

```
Read CLAUDE.md. M0 is done.

Build the throwaway M1 mirror. Use the device's built-in screenrecord as the
source so we can validate decode and display before writing any Java:

  adb exec-out screenrecord --output-format=h264 --size 1280x720 -

Spawn that as a child process, read its stdout, feed the H.264 bytes to
ffmpeg-next, and render decoded frames in an SDL window.

Expect 1-3 seconds of lag. That is screenrecord's internal buffering, not our
bug. Do not try to fix it — M2 replaces this source entirely.

Acceptance: my phone screen appears in a window and tracks what I do, however
laggy. Window closes cleanly, child process is reaped, no zombie adb.

Tag the commit v0.1-naive. Stop and report.
```

---

## Session 2 — The real device server

```
Read CLAUDE.md. This is the core technical milestone.

Build device-server/ — a Java 17 Gradle project that compiles against the
Android framework, dexes to a jar, and is launched by the host with:

  app_process / com.<pkg>.Server <version> key=value key=value ...

Implement:
- argument parsing (key=value pairs, order-independent)
- version handshake: refuse to start if the client version does not match
- connect back to the host's listening socket over the reverse tunnel
- ScreenCapture: bind a Surface to the default display, handle rotation by
  resetting the encode session
- MediaCodec H.264 encoder configured for low latency per the constraints in
  CLAUDE.md — verify no B-frames, no lookahead, and set
  KEY_REPEAT_PREVIOUS_FRAME_AFTER so a static screen still emits frames
- 12-byte packet header: flags, PTS, payload size. Document it in
  docs/protocol.md in this same commit.

Then update the host to push and launch this jar instead of screenrecord, and
to parse the new framed protocol.

Acceptance: mirror runs off our own server. Glass-to-glass is visibly far
better than v0.1-naive and under 100ms by the still-frame method. A rotation
does not crash or wedge the stream.

Hidden-API access goes through explicit wrapper classes with reflection, never
inline reflection scattered through logic. Stop and report.
```

---

## Session 3 — Pipeline hardening

```
Read CLAUDE.md, section "Hard constraints".

Audit the host pipeline against constraints 1-3 and fix every violation:
- TCP_NODELAY on every socket, both ends
- every inter-thread handoff bounded to one frame, dropping the old frame
  rather than queueing
- no jitter buffer anywhere in the video path

Split into proper threads: socket reader, demuxer, decoder, renderer. Use
bounded channels of capacity 1. Add a dropped-frame counter exposed under
--verbose.

Then run a 10-minute soak with the phone playing video and log glass-to-glass
drift. If latency at minute 10 exceeds minute 1, something is still queueing —
find it.

Acceptance: dropped-frame counter is non-zero under load (proving we drop
rather than queue), and latency at minute 10 matches minute 1 within noise.

Stop and report with the soak numbers.
```

---

## Session 4 — Benchmark harness

```
Read CLAUDE.md, section "Measurement".

Two deliverables.

1. benchmarks/methodology.md — write up the still-frame dual-timer protocol:
   millisecond stopwatch app on the phone, phone held beside the monitor, one
   still photo capturing both timers, difference is glass-to-glass, repeated
   n=20 and averaged. State the error bound honestly (roughly +/-16ms per
   sample, a few ms after averaging).

2. Stage instrumentation in the host behind a --trace flag: timestamp each
   packet at socket-read, demux-complete, decode-complete, and present. Log
   per-stage deltas and a rolling p50/p95. Make clear in the output that this
   is pipeline latency, not glass-to-glass.

Then create benchmarks/RESULTS.md as a table with columns: configuration,
glass-to-glass mean, n, pipeline p50, notes. Leave the rows for me to fill
after I take the photos. Add a scrcpy 4.1 reference row, also blank.

Acceptance: --trace produces per-stage numbers. Both markdown files read
clearly to someone who has never seen the repo.

Do not invent or estimate any measured value. Blank cells only.
```

---

## Session 5 — Input injection

```
Read CLAUDE.md. Video path is done and measured.

Add the control socket, bidirectional.

Host side (host/src/control.rs):
- translate SDL events to control messages: key down/up, text input, mouse
  motion, mouse button, scroll
- serialize to a compact binary format, documented in docs/protocol.md
- send from a dedicated thread so serialization never blocks the render loop

Device side:
- Controller thread reads and deserializes control messages
- inject via InputManager.injectInputEvent() through the existing reflection
  wrappers
- send clipboard changes back to the host on the same socket

Map right-click to BACK and middle-click to HOME.

Add round-trip instrumentation: timestamp at SDL event, and log the delta to
the first frame that could reflect it. Report this as "input round-trip",
distinct from the video spans.

Acceptance: I can click, type, and scroll on the phone from the PC. Copy on
the phone appears in the PC clipboard.

Stop and report the round-trip number.
```

---

## Session 6 — Extend mode

```
Read CLAUDE.md. This is the hardest milestone and it is upside, not core.

Before writing code, write docs/extend-mode.md covering the virtual display
prerequisite: the PC OS must believe a second display exists or there is
nothing to capture. Document setup for an off-the-shelf virtual display driver
(IddSampleDriver on Windows, or a dummy plug / virtual CRTC on Linux). We are
not writing a display driver — say so explicitly in the doc.

Then:
- host: capture the virtual display (Desktop Duplication API on Windows,
  PipeWire on Linux), encode with hardware encoding where available, push over
  a fourth socket
- device-app/: a Kotlin app that receives, decodes with MediaCodec, and renders
  to a full-screen SurfaceView with the system bars hidden

Same latency constraints as the mirror path. Same measurement discipline.

Acceptance: my PC's display settings show a second monitor. Dragging a window
onto it makes that window appear on the phone.

If the virtual display driver setup proves environment-specific enough that it
cannot be automated, document the manual steps clearly rather than shipping a
fragile installer.
```

---

## Session 7 — Portfolio polish

```
Read CLAUDE.md.

Rewrite README.md for someone evaluating this in ninety seconds:

1. One-line description, then a demo GIF placeholder at the very top
2. Measured results table pulled from benchmarks/RESULTS.md, with spans named
3. "How it works" — five sentences and the architecture diagram
4. Build and run instructions that actually work from a clean machine
5. "Prior art" section crediting scrcpy, stating clearly what I learned from
   its design docs and what I implemented independently
6. "Known limitations" — be specific and honest. Input round-trip, codec
   support, tested devices only.

Also write ARCHITECTURE.md properly: the three-socket design, the role
inversion and why, the threading model, and the wire format.

No superlatives, no "blazing fast", no unqualified latency numbers anywhere.

Acceptance: a reviewer who has never seen this repo can tell within ninety
seconds what it does, how fast it is, and how it was measured.
```

---

## Notes on running these

**Start each session fresh.** Claude Code carries context within a session; a
new milestone in a stale context inherits confusion from the last one.

**When it drifts, point at the file.** "Check CLAUDE.md, hard constraint 2" is
more effective than re-explaining.

**Let M1 be ugly.** The instinct to fix screenrecord's lag is strong and it
wastes a session. That source gets deleted in M2.

**Update CLAUDE.md as you go.** When a decision changes — stack swap, protocol
change, dropped feature — edit the file rather than only telling Claude Code in
chat. The file is what persists.

**Commit at every milestone.** The tags are part of the portfolio story: a
reviewer can see v0.1-naive at 2 seconds of lag and the M4 tag at 44ms, and
that progression is more convincing than the final number alone.

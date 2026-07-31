# USB Display Bridge

claude --resume 6807339f-d0d4-46ef-a7a6-2e75e380c41c


A wired, USB-only bidirectional display bridge between an Android phone and a
desktop PC.

- **Mirror mode** — phone screen and audio → window on the PC
- **Extend mode** — a PC virtual display → the phone, so the phone acts as a
  second monitor

Read this file before starting any task. It is the source of truth for
architecture and constraints.

---

## Non-goals

Do not build these. If a task seems to require one, stop and ask.

- Wireless / Wi-Fi / network transport of any kind. USB only.
- Any cloud service, account system, telemetry, or auto-update.
- Root access, Magisk modules, or a persistent app installed on the phone.
- An Electron or web-based host UI.
- Feature parity with scrcpy. This project is a focused subset, done well and
  measured honestly.

---

## Stack decision

**Host (PC side): Rust.**

| Concern | Crate |
| --- | --- |
| Video decode | `ffmpeg-next` |
| Window, input, render | `sdl2` |
| USB (later, OTG mode) | `rusb` |
| CLI | `clap` |
| Logging | `tracing` |

**Device server: Java 17, built with Gradle, dexed, launched via `app_process`.**
Kotlin is not used for the server — the Android framework reflection code is
cleaner in Java and matches available reference material.

**Extend-mode phone app: Kotlin, `MediaCodec` + `SurfaceView`.**

> **Windows toolchain (resolved during M1, one session, no stack switch
> needed):** `ffmpeg-next` and `sdl2` build and link against MSYS2. Working
> recipe on this machine:
> - In an MSYS2 shell: `pacman -S mingw-w64-x86_64-ffmpeg
>   mingw-w64-x86_64-SDL2 mingw-w64-x86_64-pkg-config
>   mingw-w64-x86_64-clang`. The `clang` package is required for
>   `libclang.dll`, which `ffmpeg-sys-next`'s bindgen step needs even
>   though nothing else in the project touches clang directly.
> - The active `rustup` toolchain must be `stable-x86_64-pc-windows-gnu`,
>   not `-msvc` — it has to match MSYS2 mingw64's ABI.
> - `C:\msys64\mingw64\bin` must be on `PATH` at both build time (pkg-config,
>   gcc/ld) and run time (`SDL2.dll`, `avcodec-*.dll`, etc. are linked
>   dynamically).
> - Symptom of a *different*, incompatible mingw-w64 toolchain shadowing
>   MSYS2's on `PATH`: a linker error mentioning `undefined reference to
>   _gnu_exception_handler` rather than a missing-tool error. Check
>   `where.exe x86_64-w64-mingw32-gcc.exe` for a stale entry ahead of
>   `C:\msys64\mingw64\bin`.
>
> The C++/Meson fallback this section used to threaten is not needed —
> leaving the note here only so nobody re-litigates the stack decision.

---

## Architecture

Two programs. The client on the PC pushes the server to the phone and starts it.

```
PC (Rust client)                        Android (Java server)
  decoder (FFmpeg)      <-- video --      ScreenCapture + MediaCodec
  SDL window            <-- audio --      AudioRecord + MediaCodec
  controller            <-> control ->    Controller / injectInputEvent
        \______ adb tunnel over USB ______/
```

Three sockets, opened in this order: video, audio, control. Any may be
disabled, but not all. Video and audio are device→PC only. Control is
bidirectional (input events out, clipboard changes back).

**Role inversion.** By default the PC opens a listening socket and the phone
connects back via `adb reverse`. This avoids a connect race without polling.
Support `--force-adb-forward` as a fallback.

**Version lockstep.** The client passes its version as the first argument. The
server refuses to start on mismatch. There is no backward compatibility and
none is wanted — the wire protocol is internal and may change freely.

**Why the server is Java.** Screen capture and input injection need privileges
granted to the `shell` user. Pushing a dexed jar to `/data/local/tmp` and
running it under `app_process` gets those privileges with no root and nothing
installed. `/data/local/tmp` is chosen because it is writable by `shell` but
not world-writable, so another app cannot swap the jar out before launch.

---

## Hard constraints

These are not preferences. Violating them silently breaks the project's whole
reason to exist.

### Latency

1. **`TCP_NODELAY` on every socket.** Nagle's algorithm will silently add 40ms.
2. **Every inter-thread queue holds at most one frame.** New frame arrives
   while the old one is unconsumed → drop the old one. A growing queue is a
   bug, never backpressure.
3. **No jitter buffer for video.** Audio gets one; video does not.
4. **Encoder: no B-frames, no lookahead.** B-frames reference future frames and
   cost a full frame of delay. Baseline or main profile only.
5. **Never send raw frames.** 1080p60 raw is ~3 Gbps; USB 2.0 delivers ~280
   Mbps in practice. Encode on the device that owns the pixels.
6. **Default codec is H.264.** H.265 encodes ~1.5x slower, AV1 slower still.
   Offer them behind a flag; never default to them.

### Measurement

Any latency number in code comments, commit messages, the README, or
conversation must state what it spans. Use these terms exactly:

- **glass-to-glass** — phone pixels changing to PC pixels changing. The only
  number that goes in the README headline.
- **pipeline** — capture callback to present call. Excludes both vsync waits
  and the panel.
- **stage** — a single named stage, e.g. "decode 6ms".

An unqualified "10ms latency" is not a valid claim anywhere in this repo.

The floor for glass-to-glass at 60Hz is roughly 25ms (16.7ms frame interval +
panel response). Any measurement below that is measuring something else. Say
so rather than reporting it.

---

## Milestones

Work one at a time. Do not start the next before the current one's acceptance
criteria pass.

| ID | Deliverable | Done when |
| --- | --- | --- |
| M0 | Scaffold + adb handshake | Host lists devices, pushes a file, opens and closes a reverse tunnel cleanly |
| M1 | Naive mirror (throwaway) | Video from `screenrecord` renders in an SDL window. Lag is bad; that's expected |
| M2 | Custom Java server | Own `MediaCodec` server streams; glass-to-glass under 100ms |
| M3 | Pipeline hardening | Latency stable across a 10-minute run, no drift |
| M4 | Benchmark harness | `benchmarks/RESULTS.md` with n=20 vs scrcpy on same hardware |
| M5 | Input injection | Mouse and keyboard control the device |
| M6 | Extend mode | PC sees a second display; it renders on the phone |

M1 is deliberately throwaway. It de-risks decode and display before any Java
is written. Tag it `v0.1-naive` and keep the tag.

Ship-worthy point is M4. M5 and M6 are upside.

**Re-scope (2026-07-30):** M6 is being built ahead of M2-M5 at the project
owner's explicit request. M2's custom device-server and M5's control socket
don't exist yet, so extend mode uses its own standalone video socket instead
of the three-socket scheme described above, documented separately in
`docs/protocol.md` and `docs/extend-mode.md`. M2-M5 remain the plan for the
mirror path once extend mode is working.

---

## Repo layout

```
usb-display-bridge/
├── CLAUDE.md
├── README.md              demo GIF at top, then measured numbers
├── ARCHITECTURE.md        protocol spec, wire format, threading model
├── host/                  Rust client
│   ├── src/
│   │   ├── main.rs
│   │   ├── adb.rs         tunnel setup, device discovery, server push
│   │   ├── demux.rs       byte stream → packets
│   │   ├── decode.rs      FFmpeg wrapper
│   │   ├── render.rs      SDL window
│   │   └── control.rs     input event serialization
│   └── Cargo.toml
├── device-server/         Java, Gradle, dexed to a jar
├── device-app/            Kotlin, extend-mode receiver
├── benchmarks/
│   ├── RESULTS.md
│   └── methodology.md
└── docs/protocol.md
```

---

## Conventions

- Conventional commits: `feat:`, `fix:`, `perf:`, `docs:`, `bench:`.
- Every `perf:` commit body states before and after numbers with their span.
- Rust: `cargo fmt` and `cargo clippy -- -D warnings` pass before commit.
- No `unwrap()` outside tests. Use `anyhow::Result` at boundaries.
- Wire format changes require updating `docs/protocol.md` in the same commit.
- Comments explain why, not what. Skip the ones that restate the code.

---

## Reference material

scrcpy (Apache 2.0) solves this problem well and its `doc/develop.md` documents
the architecture and protocol clearly.

**Read it for understanding. Do not copy code into this repo.** The point of
this project is to have built the thing. Where a design decision is taken
because scrcpy demonstrated it works, note that in a comment and credit scrcpy
in the README's prior-art section. Copying source defeats the purpose and is
obvious to anyone reviewing.

---

## Working agreement

- Ask before any refactor touching more than three files.
- Ask before adding a dependency not listed in the stack table.
- When a milestone's acceptance criteria pass, stop and report. Do not roll on.
- If a constraint above is blocking something, say so and propose a change to
  this file. Do not work around it quietly.

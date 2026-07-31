# Wire protocol

This currently documents extend mode's video stream only. The mirror
path's own wire format doesn't exist yet -- M1 uses `screenrecord` piped
directly over `adb exec-out`, no socket or framing of our own, and M2 (the
custom Java device-server, with its own video/audio/control three-socket
scheme) hasn't been built. See CLAUDE.md's Milestones section for the
re-scope note explaining why extend mode came first.

## Extend mode: video stream

One TCP socket, PC listening, device connecting back (role inversion, same
default as the rest of this project) over an `adb reverse tcp:<port>
tcp:<port>` tunnel. `TCP_NODELAY` is set on both ends.

This is a **standalone socket**, separate from the mirror path's future
three-socket scheme -- unifying them is deferred, not forgotten.

### Handshake

The host writes a single version byte immediately after accepting the
connection, before any frame data:

```
[u8 version]
```

`device-app` reads and checks this before doing anything else. A mismatch
means a stale installed build talking to a newer host (or vice versa) --
device-app should show an error and close the connection rather than try to
decode. Current version: `1` (see `EXTEND_PROTOCOL_VERSION` in
`host/src/main.rs`).

Unlike the mirror path's Java server -- pushed fresh to `/data/local/tmp` on
every run, so it can never be stale relative to the host that just pushed
it -- device-app is an installed APK that can persist across host rebuilds.
This byte is the cheap guard against that.

### Frames

After the handshake, one message per encoded access unit:

```
[u32 BE length][Annex-B H.264 bytes]
```

- `length` is the byte length of the payload that follows, big-endian.
- The payload is raw Annex-B (start-code-prefixed NAL units), exactly what
  the encoder (`host/src/screencap.rs`) produces -- no container, no extra
  framing inside it.
- No PTS field. Nothing on the wire currently needs to synchronize against
  it; if stage-latency instrumentation is added later (in the spirit of
  CLAUDE.md's `--trace` idea for the mirror path), it timestamps locally on
  each side rather than carrying PTS over the wire.

### In-band SPS/PPS

The encoder is configured with `x264-params=repeat-headers=1`, so SPS/PPS
NAL units (types 7 and 8) precede every IDR frame, not just the first one.
This is a wire-format decision, not just an encoder implementation detail:
it means `device-app` never needs a separate config message to build its
`MediaCodec` format -- it scans the first access unit it receives for NAL
types 7/8, uses them as `csd-0`/`csd-1`, and feeds everything after that as
ordinary input buffers. A later in-band SPS/PPS (e.g. after a display-mode
change forces the host to rebuild its encoder, see `screencap.rs`) is just
additional bytes in that access unit -- Android's AVC decoders handle
in-band parameter sets as a matter of course.

### Known rough edge: port

Device-app connects to `127.0.0.1:<port>` on-device using a hardcoded
constant that matches the host's `--port` default (27183). It is not passed
via an `am start` intent extra. Overriding `--port` on the host currently
requires updating and rebuilding device-app to match. Worth fixing before
this goes beyond dev-mode use, not fixed now to keep Stage B's scope
bounded.

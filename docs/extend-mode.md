# Extend mode: prerequisites

Extend mode makes a PC virtual display appear on the phone. Before any of
this project's code can capture anything, Windows has to believe a second
monitor exists. **This project does not write a display driver.** Writing
and signing a Windows indirect display driver is a large undertaking on its
own; the sane move is to point at an existing one.

## Recommended driver

[`IddSampleDriver`](https://github.com/microsoft/Windows-Driver-Samples/tree/main/video/IndirectDisplay),
part of Microsoft's own `Windows-Driver-Samples` repo. It's the reference
implementation of the IddCx (Indirect Display Driver) class, MIT-licensed,
maintained by Microsoft. Start here.

If building and signing the sample proves too much friction, third-party
forks exist that ship a prebuilt, pre-signed driver package built on the
same sample (search "virtual display driver" / "IddSampleDriver fork" on
GitHub). These trade auditability for convenience. Re-check what's actively
maintained at the time you actually do this — the driver landscape moves,
and nothing here is a permanent recommendation.

## Manual steps — you run these, not the assistant

These are irreversible-ish system changes (driver signing policy, a
kernel-mode driver) on your own machine. The coding agent does not run any
of this — no `bcdedit`, no WDK/driver install, no Device Manager changes.

**Chosen path on this machine (decided 2026-07-30): build Microsoft's own
sample rather than trust a prebuilt third-party driver.** Visual Studio 2026
Community is already installed here; the Windows Driver Kit (WDK) component
is not (checked: no kernel-mode headers under `Windows Kits\10\Include\*\km`).
The sample source is cloned locally at `..\Windows-Driver-Samples` (sibling
to this repo, not inside it — same reasoning as CLAUDE.md's scrcpy
prior-art note: reference material, not something to fold into this repo).

1. **Install the WDK.** Easiest: Visual Studio Installer → Modify → find
   "Windows Driver Kit" under Individual Components (or install the
   standalone WDK installer from Microsoft's WDK download page, which
   integrates with an existing VS install automatically).
2. **Enable test signing** (required unless you have a properly signed
   driver): open an elevated command prompt and run
   ```
   bcdedit /set testsigning on
   ```
   then reboot. Windows will show a "Test Mode" watermark on the desktop
   afterward — that's expected and reversible by setting `testsigning off`
   and rebooting again.
3. **Build the driver.** Open `..\Windows-Driver-Samples\video\IndirectDisplay`
   in Visual Studio, build the `IddSampleDriver` project (Release, matching
   your architecture — x64), which produces `.inf`/`.sys`/`.dll` files
   under its output directory.
4. **Install it.** Easiest path: Device Manager → Action → "Add legacy
   hardware" → "Install the hardware that I manually select" → "Display
   adapters" → "Have Disk" → point at the built `.inf`. Alternatively,
   `pnputil /add-driver <path-to-inf> /install` from an elevated prompt.
5. **Confirm it worked.** Windows Settings → System → Display should now
   show a second monitor. Note its number in the Display Settings layout —
   that number is a hint, not a guarantee (see below).

## Finding the right `output_idx`

The host's `--output-idx` flag (see `screencap.rs` / the `extend`
subcommand) selects a display by its DXGI adapter/output enumeration index,
which `ddagrab` (FFmpeg's Desktop Duplication API capture filter) uses
directly. **This enumeration order is not guaranteed to match the "1" / "2"
labels Display Settings shows you**, especially on a laptop with hybrid
graphics (integrated + discrete GPU) — the virtual driver's output can
enumerate under either adapter depending on how it attached.

Don't guess from Display Settings. Instead, use the `extend` subcommand's
`--preview` flag, which decodes and renders the captured display locally in
an SDL window with no phone or adb involved:

```
cargo run --release -- extend --preview --output-idx 0
cargo run --release -- extend --preview --output-idx 1
...
```

Try indices in order until the preview window shows the virtual display's
content (e.g. a blank desktop background, distinct from your real desktop)
rather than your real primary monitor. That index is what you pass to the
non-preview run once the driver is confirmed working.

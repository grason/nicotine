# Nicotine — agent notes

High-performance **EVE Online multiboxing** tool (Linux X11/Wayland + Windows).
Rust 2021, crate `nicotine`, version in `Cargo.toml` (`0.6.0` as of this file).
Public repo: https://github.com/isomerc/nicotine. License: MIT.

Grok does **not** load `GROK.md`. This file (`AGENTS.md`) is the project
instruction file Grok injects at session start. `CLAUDE.md` is also
recognized for compatibility; do not duplicate this file under another name.

User-facing docs live in `README.md`. Do not copy README into this file.
Keep this file short and operational.

---

## What the binary is

- Cargo bin name is **`Nicotine`** (`[[bin]]` in `Cargo.toml`) so Windows
  Explorer shows `Nicotine.exe`. Linux installers drop a lowercase
  `nicotine` symlink. `pkill -i nicotine` is case-insensitive on purpose
  (`stop` used to no-op after the rename).
- Second bin `fake-eve-stub` is a Windows-only test helper; on Linux it
  is a no-op stub so the crate still builds. Releases copy only `Nicotine`.
- Release Windows builds use `windows_subsystem = "windows"` (no console
  on double-click). Debug stays console so `cargo run` prints.

---

## Commands

```bash
cargo build --release          # Linux native; binary at target/release/Nicotine
cargo test                     # unit tests only (CI-safe; skips #[ignore])
cargo fmt --check              # CI gate
cargo clippy -- -D warnings    # CI gate; treat warnings as errors
./run-tests.sh                 # unit + integration + wayland
./run-tests.sh unit
./run-tests.sh integration     # needs $DISPLAY (X11 or XWayland)
./run-tests.sh wayland         # needs $WAYLAND_DISPLAY + xdg_activation_v1
./install-local.sh             # release build → ~/.local/bin/{Nicotine,nicotine}
```

Windows target from Linux (mirrors CI):

```bash
cargo xwin clippy --release --target x86_64-pc-windows-msvc -- -D warnings
cargo xwin build --release --target x86_64-pc-windows-msvc
```

Needs `clang` (for `clang-cl`) + `llvm` (for `llvm-rc`, used by `build.rs`
to embed the `.exe` icon). First `cargo xwin` downloads the MSVC SDK into
`~/.cache/cargo-xwin`. Nix: `nix develop` or `nix-shell` (see `flake.nix`,
`shell.nix`). `rodio`/`cpal` need ALSA headers on Linux (`libasound2-dev`).

Do not use `cargo test` as a stand-in for integration. Those tests are
`#[ignore]` so headless CI unit jobs stay green.

---

## Runtime shape

```
nicotine start / no-args
  ├─ daemon thread     cycling, input listeners, preview manager, IPC
  └─ main thread       iced 0.14 config panel (the visible app)

nicotine daemon        headless daemon only
nicotine forward|backward|N
                       IPC to daemon if up, else one-shot cycle under lock
nicotine list|active   diagnostics; integration tests assert on this stdout
nicotine stack         restack/center EVE clients
nicotine stop          pkill/taskkill + drop socket/lock
```

- **IPC**: Unix socket `/tmp/nicotine.sock` or Windows named pipe
  `nicotine.sock`. Override with `NICOTINE_SOCKET_PATH` (tests).
- **Lock**: `nicotine-cycle.lock` via `fd-lock`. Override dir with
  `NICOTINE_RUNTIME_DIR`.
- **Config**: `~/.config/nicotine/config.toml` (XDG) / Roaming APPDATA.
  Override with `NICOTINE_CONFIG_DIR` (full dir, no extra `nicotine/`
  suffix). Daemon re-reads it every ~500 ms (hot-reload).
- **Character order** lives in `config.characters` (TOML array), **not**
  `characters.txt`. README still mentions the old file; do not revive it.
  Read `CycleState::character_order()` per operation — do not cache a
  snapshot across ticks (that bug shipped: panel edits were ignored).
- **Preview positions**: `preview_positions.toml` next to config.
- Root `config.toml` in this repo is **not** a template; ignore it.

CLI cycle commands prefer the daemon. If it is down they take
`lock::with_cycle_lock` and run once. Drop the command if the lock is held.

---

## Module map

| Path | Role |
|---|---|
| `src/main.rs` | CLI, display-server detect, start/stop, one-shot cycle |
| `src/daemon.rs` | IPC loop, 500 ms rescan + config hot-reload, input spawn |
| `src/cycle_state.rs` | Cycle/switch planner. `ACTIVATION_GRACE` = 300 ms |
| `src/window_manager.rs` | `WindowManager` trait + compositor detect |
| `src/x11_manager.rs` | Native X11 (x11rb, EWMH) |
| `src/wayland_backends.rs` | KWin / Sway / Hyprland / GNOME |
| `src/windows_manager.rs` | Win32 EnumWindows / SetForegroundWindow |
| `src/windows_helpers.rs` | **Pure** Win32-shaped logic, compiled on Linux for unit tests |
| `src/windows_input.rs` | Windows hooks + `RegisterHotKey` |
| `src/mouse_listener.rs` / `keyboard_listener.rs` | Linux evdev |
| `src/pointer_nudge.rs` | Wayland uinput +1/−1 px after activate (pointer focus) |
| `src/eve_match.rs` | `exefile.exe` process gate (title-only is not enough) |
| `src/eve_logs/` | Chatlog/gamelog tailer: system names + inactive-client alerts |
| `src/preview_common.rs` | Shared drag/snap geometry |
| `src/preview_x11/` | XComposite + XRender previews + list window |
| `src/preview_windows/` | DWM thumbnails |
| `src/config_panel/` | iced 0.14 GUI (`mod.rs` logic, `ui.rs` view) |
| `src/config.rs` | TOML config + `LiveSettings` (panel → preview, no disk round-trip) |
| `src/ipc.rs` / `paths.rs` / `lock.rs` | Socket, runtime paths, cycle lock |
| `src/telemetry.rs` | Launch ping; **compile-time** gated on `NICOTINE_TELEMETRY_TOKEN` |
| `src/version_check.rs` | GitHub latest-release check for the panel footer |
| `src/audio.rs` | Logo easter-egg jingle (`assets/nicotinecountry.mp3`) |
| `tests/fake_eve.rs` | End-to-end against real OS windows |
| `tests/common/` | `FakeEveHarness` + `TestDaemon` (unix/windows) |

Platform modules are `#[cfg(unix)]` / `#[cfg(windows)]` from `main.rs`.
`windows_helpers.rs` and `eve_match.rs` stay unconditional so Linux CI
can unit-test Windows-shaped logic.

---

## Hard constraints (do not regress)

1. **EVE detection** = title `starts_with("EVE - ")` **and** process
   basename `exefile.exe` (Wine/Proton `comm` on Linux, image path on
   Windows). Title-only matches Discord/browser tabs. Stored
   `EveWindow.title` has the `EVE - ` prefix stripped.

2. **Config panel is iced 0.14**, not egui/eframe. Comments still say
   “egui” in places; do not reintroduce egui. On KWin Plasma Wayland the
   panel is a native Wayland window; **never force the winit X11
   backend** (panics `"Invalid surface"` under XWayland).

3. **GNOME Wayland**: cycling + previews work (XWayland EWMH).
   `stack_windows` / Restack is disabled — Mutter drops client moves.
   `restack_supported()` is the single gate. GNOME-on-Xorg still stacks.

4. **KWin/GNOME activate** is a pager-sourced `_NET_ACTIVE_WINDOW`
   ClientMessage (`source = 2`) plus `set_input_focus`. Do **not** add
   xdg-activation tokens onto `_NET_STARTUP_ID` for cycling: KWin denies
   serial-less requests and the round-trip used to back the compositor
   up. `xdg-activation` is still tested (`--ignored` bin test) because
   the protocol crate is a dep; it is not on the cycle hot path.

5. **Wayland pointer focus** does not follow raise. After activate,
   `pointer_nudge` emits a net-zero uinput motion (+1 then −1 px) from a
   delayed thread. Best-effort: missing `/dev/uinput` logs once and
   cycling still works. Do not warp via Wayland (`wp_pointer_warp_v1`
   only works on our own surface).

6. **After we activate**, skip `sync_with_active` for 300 ms
   (`ACTIVATION_GRACE`). Compositor focus is async; trusting a stale
   `_NET_ACTIVE_WINDOW` made rapid cycles rewind. After grace, honor
   the compositor so alt-tab still works.

7. **Linux input** is evdev (`/dev/input/event*`, user in `input`
   group). Mouse on by default; keyboard off (Tab fights the game).
   **Windows** mouse cycling is **off** by default (XBUTTON1/2 steal
   browser/game back/forward); keyboard F11/F10 on by default. Codes
   are **platform-native** (evdev vs Win32 VK) — do not share numeric
   defaults across `cfg`.

8. **LiveSettings** is the live panel→preview channel (size, opacity,
   display mode, lock, show/hide). Disk save is debounced 300 ms. Do
   not make the preview manager wait on a full config reload for slider
   drags.

9. **Telemetry** must stay anonymous (install UUID + version + OS, no
   character names / hotkeys / cycle counts). `option_env!("NICOTINE_TELEMETRY_TOKEN")`
   — unset token means the ping is compiled out. Failures never affect UX.
   `Cross.toml` must keep `passthrough = ["NICOTINE_TELEMETRY_TOKEN"]`.

10. **EVE logs** (`src/eve_logs/`): read-only poll of Chatlogs (UTF-16 LE
    `Local_*.txt`) and Gamelogs (UTF-8). Identity is the `Listener:` header.
    Off by default (`[logs] enabled`). Do not parse general Local chat —
    keep the `EVE System` / `(notify)` fast-reject. Preview chrome for
    systems/alerts is separate; the tailer writes `LogLiveState` only.

11. **Brand palette** is duplicated as RGB in iced, X11 chrome, and
    Win32 GDI (red `196,30,58`, gold `180,155,105`, cream `252,250,242`,
    black `30,30,30`). Change all three.

12. **Windows DPI**: `SetProcessDpiAwarenessContext(SYSTEM_AWARE)`
    before any window. Preview sizes go through the DPI helper; do not
    assume 96 DPI.

13. Integration tests must isolate via `NICOTINE_SOCKET_PATH`,
    `NICOTINE_RUNTIME_DIR`, `NICOTINE_CONFIG_DIR` so they never touch a
    user’s live daemon. Run `--test-threads=1`. Linux fake-EVE needs a
    WM that publishes `_NET_CLIENT_LIST` (openbox in CI). Fake clients
    are titled `EVE - <name>` with `comm`/image `exefile.exe`.

---

## Platform backends

| Session | Manager | Notes |
|---|---|---|
| X11 | `X11Manager` | Full: cycle, stack, previews |
| KDE Wayland | `KWinManager` | EVE is XWayland; EWMH activate + pointer nudge |
| Sway | `SwayManager` | `swaymsg` |
| Hyprland | `HyprlandManager` | `hyprctl` |
| GNOME Wayland | `GnomeManager` | Cycle + previews; no restack |
| Unknown Wayland | bail | |
| Windows | `WindowsManager` | DWM thumbnails, `RegisterHotKey`, low-level mouse hook |

Linux list-mode / preview chrome: XComposite redirect + XRender, fonts
via `ab_glyph` (`JetBrainsMono-Regular.ttf`, `Marlboro.ttf`). Windows:
DWM thumbnails + GDI text. Shared UX (drag, snap, click-vs-drag) lives
in `preview_common`.

---

## Tests and CI

- **Unit**: pure planners, config parse, eve_match, key classify,
  `windows_helpers`. No display.
- **Integration** (`tests/fake_eve.rs`, `#[ignore]`): real windows +
  daemon subprocess. Assert via `nicotine list` / `nicotine active`
  (`<id>\t<title>`). Windows `EnumWindows` is Z-order; re-read list
  after any activate. Linux `_NET_CLIENT_LIST` is map-time stable.
- **Wayland**: `cargo test --bin Nicotine xdg_activation -- --ignored`.

CI (`.github/workflows/ci.yml`): Linux `cargo test` + fmt + clippy;
`cargo xwin clippy` for Windows; Xvfb+openbox integration; windows-latest
integration; sway-headless xdg-activation.

Release (tag `v*`): tag **must** equal `Cargo.toml` `version` (the v0.4.2
self-update loop). Linux via `cross` (`Cross.toml` installs
`libasound2-dev:$CROSS_DEB_ARCH`). Windows via `cargo xwin`. Artifacts:
`nicotine-linux-{x86_64,aarch64}`, `Nicotine.exe`.

---

## Style

- rustfmt default; clippy `-D warnings`.
- Prefer extracting **pure helpers** (no Win32/X11) so they unit-test on
  Linux. That is why `windows_helpers.rs` and `eve_match.rs` exist.
- Comments explain non-obvious constraints (compositor quirks, grace
  windows, why a protocol was *removed*). Do not add narration of the
  change you just made.
- `anyhow::Result` at the edges. `Arc<Mutex<T>>` for live shared state;
  preview code uses poison-recover (`lock_recover`) so a panel panic
  does not kill the preview thread.
- Platform `cfg` at module boundaries. Do not sprinkle `cfg` through
  shared cycle logic.
- Do not “fix” the binary name back to `nicotine`. Do not add a
  `GROK.md`. Do not reintroduce `characters.txt`.

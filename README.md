<div align="center">
  <img src="assets/ghlogo.png" alt="Nicotine Logo" width="600">
</div>

# Nicotine 🚬

High-performance EVE Online multiboxing tool for Linux (X11 & Wayland) and Windows.

<div align="center">

### 💬 [**Join the Nicotine Discord**](https://discord.gg/N82KJcS47f) 💬

</div>

[Illuminated is recruiting!](https://illuminatedcorp.com)

## Features

- **Instant client cycling** with mouse buttons (forward/backward) or targeted switching (jump to client N)
- **Live preview windows** per EVE client — XComposite + XRender on Linux, DWM thumbnails on Windows
- **List display mode** — compact roster window as an alternative to per-client previews
- **Compact grid** — one window of live thumbnails, for a lot of accounts
- **Log alerts** — marks for fleet, tackle, mining, and the rest, plus crossed swords when a client takes damage
- **Config panel** for character order, per-character jump hotkeys, preview sizing, and display mode
- **Daemon architecture** for near-zero-latency window switching
- **Auto-stack windows** to perfectly center multiple EVE clients
- **Drag-to-position with snap-to-dock** on previews and the list window; lockable layout for play
- **Multi-compositor support** — X11, KDE Plasma (Wayland), Sway, Hyprland, GNOME (Wayland; auto-stack excepted)
- **Minimize inactive clients** — optional, reduces resource usage when cycling away

## Roadmap
- Comprehensive documentation
- More configuration options

## Quick Install

### One-Line Installer (Recommended)

```bash
curl -sSL https://raw.githubusercontent.com/grason/nicotine/master/install-github.sh | bash
```

Then restart your terminal and run:
```bash
nicotine start    # Automatically runs in background
```

### From Source

```bash
git clone https://github.com/grason/nicotine
cd nicotine
./install-local.sh
```

Then restart your terminal and run:
```bash
nicotine start    # Automatically runs in background
```

## Usage

### Basic Commands

```bash
nicotine start          # Start everything (daemon + overlay)
nicotine stop           # Stop all Nicotine processes
nicotine stack          # Stack all EVE windows
nicotine forward        # Cycle to next client
nicotine backward       # Cycle to previous client
nicotine 1              # Jump to client 1
nicotine 2              # Jump to client 2
```

### Targeted Cycling

`nicotine 1`, `nicotine 2`, and so on follow the cycle order. Set that order in the config panel (Cycle Order). It is saved as `characters` in `~/.config/nicotine/config.toml`. With no order saved, the commands follow window detection order. Bind the commands to hotkeys in your desktop environment for quick access.

### Mouse Bindings

**Native Support (Works on X11 & Wayland):**

Nicotine has built-in mouse button detection that works universally across all display servers and compositors

**Quick Setup:**
1. Add your user to the `input` group:
   ```bash
   sudo usermod -a -G input $USER
   ```
2. **Log out and log back in** (required for group membership to take effect)
3. Start Nicotine - mouse buttons work automatically!

**Configuration:**
Edit `~/.config/nicotine/config.toml` to customize:
```toml
enable_mouse_buttons = true
forward_button = 276   # Button 9 (forward/side button)
backward_button = 275  # Button 8 (backward button)
mouse_device_name = "" # Optional takes priority over mouse_device_path, use evtest to find name of the device
mouse_device_path = "/dev/input/event3" # Optional and not created on first run, find the correct device with evtest
```
If using mouse_device_name the string must be na exact match, you can list your devices with evtest.

```
$ sudo evtest
...
/dev/input/event16:     Logitech PRO X
...
```
In `~/.config/nicotine/config.toml` set

```
mouse_device_name = "Logitech PRO X"
```

Device is configuration priority order is:
1. Name
2. Path
3. Autodetect

**Common button codes:**
- `275` = BTN_EXTRA (button 8, backward)
- `276` = BTN_SIDE (button 9, forward)
- `277` = BTN_FORWARD
- `278` = BTN_BACK

**Find your button codes:**
```bash
sudo evtest  # Select your mouse, then click buttons to see their codes
```

**Troubleshooting:**
- Verify group membership: `groups | grep input`
- Check permissions: `ls -l /dev/input/event*`
- Disable if needed: `enable_mouse_buttons = false` in config

### Keyboard Bindings

**Quick Setup:**
1. Add your user to the `input` group:
   ```bash
   sudo usermod -a -G input $USER
   ```
2. **Log out and log back in** (required for group membership to take effect)

**Configuration:**
Edit `~/.config/nicotine/config.toml` to customize:
```toml
enable_keyboard_buttons = true
forward_key = 15  # TAB Key
backward_key = 15  # TAB Key - modifier_key applied if set in config
keyboard_device_path = None # Device path /dev/input/eventX (OPTIONAL but you may need to set this if keybinds don't work)
modifier_key = None # You will have to add this if you want a modifier key for backward cycling
```

**Common button codes:**
- `15` = KEY_TAB (TAB Key)
- `42` = LEFT_SHIFT

**Find your button codes:**
```bash
sudo evtest  # Select your keyboard, then click buttons to see their codes
```

**Troubleshooting:**
- Make sure you've enabled keyboard buttons: `enable_keyboard_buttons = true` in config
- Check for other device events, sometimes keyboards will have multiple events but only one is handling inputs
```bash
cat /proc/bus/input/devices | grep -B 5 "kbd" | grep -E "Name|Handlers"
sudo evtest /dev/input/eventX # Replace X with the correct event number i.e event11
```

### Config Panel & Previews

`nicotine start` opens the config panel and spawns one preview window per running EVE client. The panel has five sections: **Display Mode** (Previews, Grid, or List), **Cycle Order** (character list + per-character jump hotkeys), **Keyboard Hotkeys**, **Preview Windows** (size sliders, show/hide toggle), and **Alerts** (EVE log monitoring). Changes apply live; saves debounce to disk.

Previews and the list window:
- **Left-click-drag** to reposition; edges snap to adjacent windows
- **Click without drag** on a preview to foreground that EVE client
- **Lock positions** in the panel to freeze the layout during play

### Alerts (opt-in)

The Alerts tab tails EVE's Chatlogs and Gamelogs (read-only). Enable it there.

- Preview title strips show `Character - Jita` once a system is known. The compact grid shows that system on the row above the character name
- Incoming (↓) / outgoing (↑) DPS from the combat log sit on the left and right of the preview title. They hide when idle
- Taking damage shows crossed swords and flashes that client's frame red for a moment (coalesced so a volley is one pulse). The swords use the icon slot for the whole flash, including on the client you are flying
- An event icon sits in the middle of that client's thumbnail, and at the right of its list row. The frame turns gold on an inactive client while that icon is up. The character name stays

| | Shown for | Default |
| --- | --- | --- |
| <img src="assets/alerts/damage.png" width="40" alt="Crossed swords"> | Taking damage | On while monitoring is enabled |
| <img src="assets/alerts/fleet_invite.png" width="40" alt="Envelope"> | Fleet invite | On |
| <img src="assets/alerts/follow_warp.png" width="40" alt="Rocket"> | Follow / warp | On |
| <img src="assets/alerts/regroup.png" width="40" alt="Pin"> | Regroup | On |
| <img src="assets/alerts/compression.png" width="40" alt="Package"> | Compression cycle | On |
| <img src="assets/alerts/scramble.png" width="40" alt="Swirl"> | Warp scramble from a player | On, under Player tackle |
| <img src="assets/alerts/disrupt.png" width="40" alt="Bolt"> | Warp disruption from a player | On, under Player tackle |
| <img src="assets/alerts/jam.png" width="40" alt="Dish"> | Jammed by a player | On, under Player tackle |
| <img src="assets/alerts/bubble.png" width="40" alt="Bubbles"> | Warp disruption bubble | On, under Player tackle |
| <img src="assets/alerts/out_of_charges.png" width="40" alt="Low battery"> | Module out of charges | On |
| <img src="assets/alerts/cargo_full.png" width="40" alt="Suitcase"> | Cargo or ore hold full | On, under Mining |
| <img src="assets/alerts/mining_range.png" width="40" alt="Ruler"> | Miner beyond range | On, under Mining |
| <img src="assets/alerts/decloak.png" width="40" alt="Eye"> | Decloak | Off |
| <img src="assets/alerts/convo_request.png" width="40" alt="Balloon"> | Conversation request | Off |
| <img src="assets/alerts/crystal_broke.png" width="40" alt="Gem"> | Mining crystal broke | Off |

The icons are Twemoji graphics by Twitter, Inc., [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/), via [jdecked/twemoji](https://github.com/jdecked/twemoji).

Scramble, point, and jam only count when the other pilot's name has a corp ticker. A bubble always counts. Sleepers and other rats do not light the tackle icons.
- Keep the icon until you switch to that client, or clear it after a timeout. The Alerts tab chooses
- On Linux, Nicotine looks for logs in the running EVE client's Wine prefix; override the directories if auto-detect misses

## Configuration

Config file: `~/.config/nicotine/config.toml`

Auto-generated on first run. Key settings:

```toml
display_width = 1920
display_height = 1080
panel_height = 0           # Set this if you have a taskbar/panel
eve_width = 1037           # ~54% of display width
eve_height = 1080
show_previews = true       # Spawn preview windows (false = daemon-only)
preview_width = 320        # Preview window width in px
preview_height = 180       # Preview window height in px
display_mode = "Previews"  # "Previews", "Grid", or "List"
positions_locked = false   # Disable drag on previews + list
enable_mouse_buttons = true
forward_button = 276       # Button 9
backward_button = 275      # Button 8
minimize_inactive = false  # Minimize clients when cycling away (saves resources)
```

Per-character jump hotkeys live under `[character_hotkeys."Name"]` with `vk` and optional `modifier`; the config panel writes them for you.

## Architecture

- **Daemon mode**: Maintains window manager connection and state in memory for instant cycling
- **Unix socket IPC**: ~2ms command latency (vs ~50-100ms process spawning)
- **Non-blocking activation**: Fire-and-forget window switching
- **Native mouse support**: Direct evdev access for universal mouse button detection

## Requirements

### Display Server Support

Nicotine supports both **X11** and **Wayland** (compositor-dependent):

- **X11** - Full support (all features)
- **Wayland - KDE Plasma** - Full support via wmctrl (XWayland)
- **Wayland - Sway** - Full support via swaymsg
- **Wayland - Hyprland** - Full support via hyprctl
- **Wayland - GNOME** - Cycling + preview windows work (via XWayland/EWMH); auto-stack is disabled because Mutter blocks applications from positioning windows. GNOME-on-Xorg supports everything.

### Dependencies

**Required:**
- **wmctrl** - Window management on X11 and KDE Plasma Wayland

**Wayland-specific (compositor tools):**
- **KDE Plasma:** wmctrl (uses XWayland compatibility)
- **Sway:** swaymsg (included with sway)
- **Hyprland:** hyprctl (included with hyprland)

**Install:**
```bash
# Arch
sudo pacman -S wmctrl

# Ubuntu/Debian
sudo apt install wmctrl

# Fedora
sudo dnf install wmctrl
```

For **mouse button support**, add yourself to the `input` group (see Mouse Bindings section).

## Wayland Support & Known Limitations

**What works:**
- Mouse buttons (native evdev support, no external tools needed)
- Window detection and cycling (all supported compositors)
- Window stacking (KDE/Sway/Hyprland; not GNOME Wayland)
- Preview windows (XComposite + XRender via XWayland)
- Auto-detection of display server and compositor

**Limitations:**
- GNOME (Wayland): auto-stacking is unavailable. Mutter does not let applications position windows, and there is no extension-free way around it. Client cycling and preview windows work normally via XWayland, and the Restack button is hidden on this session. GNOME-on-Xorg supports everything (including stacking), but note the GNOME Xorg session is being removed upstream (GNOME 49+/Fedora 43).

## Building from Source

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build
cargo build --release

# Binary at: target/release/Nicotine
```

## License

See [LICENSE](LICENSE.md)

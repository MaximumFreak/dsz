# DSZ - DualSense Zen

**DSZ - DualSense Zen** is a fast, native Windows companion for the PlayStation DualSense and DualSense Edge controllers, written in Rust: lighting, adaptive triggers, Bluetooth haptics, gyro and touchpad mouse, button mapping, virtual Xbox 360 / DualSense / DualShock 4 controllers, and per-game profiles.

## Why use it

- **Game haptics and adaptive triggers over Bluetooth.** Games with native DualSense support normally only do this over a USB cable.
- **Automatic DualSense mode.** When you launch a game, DSZ detects whether it supports the DualSense natively. If it does, DSZ switches to a virtual DualSense, so the game's own haptics and adaptive triggers work. Every other game gets a virtual Xbox 360 controller. It switches back when the game closes. ([How it works](#automatic-dualsense-switching))
- **The rest of the controller:** lighting, adaptive triggers in any game, system audio as haptics, touchpad and gyro mouse, button mapping, and per-game profiles.
- **Extremely fast and lightweight:** a single `.exe` with nothing to install. It starts in about 0.2 s and uses under 1% CPU in the tray. ([Performance](#performance))
- **Free and open source**, with no account or license check.

### What to know first

- **Windows 10 or 11 only**, for the DualSense and DualSense Edge. It doesn't drive a physical DualShock 4.
- **Virtual controllers need two free, open-source drivers:** usbip-win2 (0.9.7.7) and HidHide. The app installs either in one click (see [Drivers](#drivers)). Without them it still does lighting, triggers, haptics from system audio, mouse modes, and mappings, but games see the real controller.
- **Close other controller apps while it runs.** Only one app can drive the controller at a time. If buttons double up or lights flicker in a Steam game, turn Steam Input off for that game.
- **This is an early release (0.1)**, tested on one PC with a DualSense Edge over Bluetooth. Expect rough edges, and please report them.

## Performance

Measured on one PC with a DualSense Edge over Bluetooth (about 670 input reports a second), launched cold with no game running, next to another popular DualSense app with the same setup. Figures are rounded; results on other PCs will differ.

| | DSZ | Another DualSense app |
|---|---|---|
| **Startup** | | |
| Window visible after launch | about 0.2 s | a few seconds |
| Virtual controller ready | about 0.2 s | several seconds |
| **Input lag** (controller to virtual controller) | | |
| Typical | a few hundredths of a ms | about the same |
| Worst case | under 1 ms | occasional hitches of tens of ms |
| **Game trigger feedback** (game sends an effect, until the trigger motor moves) | | |
| Typical | about 30 ms | about 40 ms |
| **Window open, controller in use** | | |
| CPU (% of one core) | about 7% | about 3× more |
| Memory | about 100 MB | about 4× more |
| **In the tray, game rumble playing** | | |
| CPU (% of one core) | under 1% | about 2× more |
| Memory | about 100 MB | about 4× more |

Trigger feedback includes both Bluetooth trips and the trigger motor starting, so most of it is the controller itself.

Part of the low overhead is that every virtual controller is served inside the app itself, built from the USB/IP and USB Audio Class specifications. No separate USB/IP server such as VIIPER is used, so there is no extra process and no IPC hop. And because the app contains no VIIPER code (VIIPER is GPL-3.0), it stays MIT.

## Download and run

Download `dsz.exe` from the [Releases](../../releases) page and run it; there is nothing to install. The exe isn't code-signed, so the first time you run it Windows SmartScreen may say "Windows protected your PC": choose **More info**, then **Run anyway**.

### Build from source

On Windows with the Rust toolchain (MSVC):

```powershell
cargo run --release -p dsz
```

The release build is a single file, `target\release\dsz.exe`. Settings, profiles, and the log live in `%APPDATA%\DSZ\`.

Flags: `--minimized` starts in the tray; `--page <name>` opens a page (`lighting`, `adaptive`, `haptics`, `motion`, `button`, `virtual`, `profiles`, `settings`); `--edit <profile>` opens a profile for editing without activating it; `--probe-virtual <hidhide|usbip-x360|usbip-ds4|usbip-ds|install-check|game-output|tone|default-audio>` checks one virtual-device driver from a console (`usbip-x360` attaches a virtual Xbox 360 pad and checks XInput and rumble; `usbip-ds4` reads a virtual DualShock 4 back through HID; `usbip-ds` attaches a virtual DualSense, reads it back through HID, and plays a 4-channel tone into it; `install-check <file|url>` shows what the driver-install check sees; `game-output` and `tone` act like a game against the running app's virtual DualSense). `DSZ_PERF=1` logs UI frame rate. `DSZ_DATA_DIR=<folder>` runs with a separate settings folder, for testing. In debug builds, `DSZ_SNAPSHOT=<file.bmp>` renders the window once, saves it, and exits (it doesn't open controllers, so it runs beside the real app); `DSZ_DEMO=1` shows the no-controller drawing with a few inputs live.

A fresh install starts with two profiles, from `crates\dsz\defaults\`: **Xbox** (a virtual Xbox 360 controller, touchpad as mouse, a dim red lightbar) and **DualSense** (a virtual DualSense with game haptics, PS opens the Xbox Game Bar). A long press on Mute switches between them, and games with native DualSense support switch to DualSense on their own.

## What it does

| Area | Features |
|---|---|
| Connection | Bluetooth and USB, DualSense and Edge, hot-plug in under a second. Read timeouts never drop the device and write errors back off and retry; a controller goes away only when Windows removes its node. Bluetooth input CRCs are checked |
| Lighting | Static, rainbow, breathing, strobe, battery gradient, off; brightness and speed; player LEDs (player 1–5, custom, battery gauge); mute LED (off, on, pulse, toggle with button); low-battery warning (a double blink every 2.5 s, 15 % by default) |
| Adaptive triggers | Resistance, weapon, vibration, slope, per-zone resistance and vibration (drag to paint), bow, galloping, machine, and named presets (Very soft to Rigid, GameCube click, Choppy, Vibrate pulse) built from those effects; live zone preview with the current pull |
| Wireless haptics | PCM haptics over Bluetooth (report `0x32`, one every 32/3000 s), from system audio (WASAPI loopback, any output device) with high-pass and low-pass filtering, stereo, and gain; button clicks placed under the grip you pressed; trigger texture. The stream takes the actuators only while something needs it (system audio, clicks, trigger texture, game haptics, a test pulse); the rest of the time rumble goes to the controller's own rumble emulation (power and "improved rumble" apply). While streaming, or with "Rumble as haptics" on, rumble plays through a rumble-to-haptics conversion: heavy motor 34–50 Hz on the left, light motor 85–110 Hz on the right, weak rumble lifted, smoothed. The `0x31` control report keeps flowing alongside, and changes from a game or mod are sent at once instead of on the next 8 ms tick. Game actuator channels from a virtual DualSense mix into the same stream (see Virtual controller) |
| Motion & touch | Gyro mouse (always, hold, unless held, toggle; turn, lean, or both; soft deadzone, tiered smoothing, acceleration, auto and manual calibration), touchpad mouse (one-finger tap left click, two-finger tap right click, press to click and drag, optional right-half right click, two-finger scroll; movement waits 60 ms after a finger lands so taps and presses don't nudge the cursor), sticks as mouse, scroll, WASD, or arrows |
| Mappings | Any button or button combo, including Edge paddles and Fn, to keys and key combos, mouse buttons, scroll, media keys, a virtual-controller button, switching to a named profile or next/previous, toggling gyro or touch mouse, or launching a program. Press, long press, double tap; hold, tap, turbo, toggle. "Hide from game" removes the button (or the combo's buttons while it is held) from the virtual controller |
| Virtual controller | Per profile: off (native), Xbox 360, DualSense, or DualShock 4. Every virtual pad is a wired USB controller the app serves itself over USB/IP to usbip-win2, so that is the only virtual-controller driver needed. The Xbox 360 pad presents the real controller's USB identity and vendor interface, so Windows' built-in Xbox 360 driver (`xusb22`) loads and XInput sees it; rumble and the ring LED (which gives its XInput slot) come back on its OUT endpoint. The DualShock 4 is a plain HID device with the controller's report layout, carrying the DualSense's motion and touch, for games that show PlayStation prompts only for one; its rumble and lightbar come back. The virtual pad is plugged in when the app starts, before any controller, so it takes the first free XInput slot, and it stays connected (idle, with the real controller still hidden) while the controller sleeps or drops out; the controller takes the same pad back when it reconnects. "Stay connected while the controller is off" on the Virtual controller page turns this off. Games get the mapped and muted pad with per-stick deadzones (radial, axial, or a curve with inner and outer zones, a response curve, and an optional lift), axis inversion, and trigger ranges. Game rumble comes back and plays as haptics over Bluetooth. The physical controller is hidden with HidHide while the virtual one is up, this app is whitelisted, every change is undone on exit, and crash leftovers are undone at the next start. DualSense mode is a wired DualSense the app serves itself over USB/IP to usbip-win2: games get its HID interface and its 4-channel 48 kHz audio endpoint, and the app relays output reports (rumble, adaptive triggers, LEDs) and game audio (actuators to haptics over Bluetooth, or classic rumble when PCM is off; the speaker channels are not played). Windows' habit of making a new USB audio device the default is undone, so system sound and the default microphone stay where they were. See below |
| Profiles | Create, duplicate, rename, delete, import and export JSON; per-controller assignment, or every controller on the default profile at startup; per-game automatic switching (most recently started game wins, restored on exit). Games with native DualSense support switch to a chosen DualSense profile without a rule: each new program's folder and exe are checked once for Sony's pad library (`libScePad`, `scePad*` calls), Wwise's DualSense haptics sink (`AkScePad`), DualSenseWindows (`ds5w`), or DualSense controller names, and the answer is cached in `dualsense-detect.json`. On the reference PC this flags Alan Wake 2, Baldur's Gate 3, Elden Ring and RPCS3, and not Chrome, Edge, Discord, the Epic launcher, Rocket League or Octopath Traveler II. `--probe-virtual dsdetect [--path <exe>]` runs the check by hand |
| Mod API | UDP server on `127.0.0.1:6969` speaking the protocol existing DualSense game mods use (see [Mod compatibility](#mod-compatibility)): `TriggerUpdate` with trigger modes 0–26 (custom trigger values Off through PulseAB), `RGBUpdate`, `PlayerLED`, `PlayerLEDNewRevision`, `TriggerThreshold`, `MicLED`, `ResetToUserSettings`, and `GetDSXStatus`, with the status reply mods read. Extensions: `HapticPulse` `[index, left%, right%, Hz, ms]` and `Rumble` `[index, heavy, light, ms]` |
| Battery | The haptic stream runs only while something needs it. Animated lighting sends at most 40 control reports a second (34/s measured on an Edge, down from 100), while trigger, rumble, and LED changes still go out at once. Idle power-off for Bluetooth is in Settings. The output report's power-save bits were tested and do not switch the motion sensors off over Bluetooth, so they stay unused |
| App | Tray icon with profile switching and battery tooltip, close-to-tray, start with Windows, Bluetooth power-off and idle shutoff, live log viewer |

### Automatic DualSense switching

The first time a game runs, the app checks its files once for signs of native DualSense support: Sony's PC pad library, Wwise's DualSense haptics, DualSenseWindows, or DualSense controller names. While a game that has them is running, the controller switches to your DualSense profile, which presents a virtual DualSense, so the game's own haptics and trigger effects reach the controller. When the game closes, it switches back.

Everything else keeps your default profile: out of the box, a virtual Xbox 360 controller. A per-game rule overrides the check, and a long press on Mute switches by hand.

### Mod compatibility

Game mods that drive adaptive triggers and lights send JSON over UDP to port 6969. DSZ's side of that protocol follows what these public mods send and read: instruction types, trigger modes and the order of their parameters, LED values, and the status reply. Trigger modes 19–26 follow [Nielk1's trigger effect generator](https://gist.github.com/Nielk1/6d54cc2c00d2201ccb8c2720ad7538db) (MIT), whose parameter order the mods that use them match. The named presets (Very soft to Rigid, GameCube, Choppy, Vibrate pulse) are DSZ's own effects, since mods send only the name. Custom trigger values support Off, Rigid, and Pulse (plain, A, B, and A+B), with the mode bytes pydualsense documents.

`udp.rs` has one test per mod that replays packets as that mod builds them (number or string parameters, booleans, missing parameter lists) and checks what DSZ does with each.

| Mod | Game | License |
|---|---|---|
| [dvize/TarkovDSX](https://github.com/dvize/TarkovDSX) | Escape from Tarkov | MIT |
| [dvize/DSXFallout4](https://github.com/dvize/DSXFallout4) | Fallout 4 | MIT |
| [dvize/DSXSkyrim-NG](https://github.com/dvize/DSXSkyrim-NG) | Skyrim | GPL-3.0 |
| [tommargar/DualSense4Rockstar](https://github.com/tommargar/DualSense4Rockstar) | GTA V, Red Dead Redemption 2 | none stated |
| [SYSTEMATI0N/WF2_DSX](https://github.com/SYSTEMATI0N/WF2_DSX) | Wreckfest 2 | MIT |
| [cosmii02/ForzaDSXlegacy](https://github.com/cosmii02/ForzaDSXlegacy) | Forza Horizon 4 and 5, Forza Motorsport 7 | none stated |
| [aaronfang/RBR_Adaptive_Trigger](https://github.com/aaronfang/RBR_Adaptive_Trigger) | Richard Burns Rally, Assetto Corsa | none stated |
| [RiddleTime/Race-Element](https://github.com/RiddleTime/Race-Element) | Sim racing (ACC and others) | GPL-3.0 |
| [tpetsas/DSXpp](https://github.com/tpetsas/DSXpp) | C++ client library for mods | MIT |
| [Tio-Henry/DualSenseX-Support](https://github.com/Tio-Henry/DualSenseX-Support) | Godot engine add-on | MIT |


### Drivers

Nothing is bundled. The Virtual controller page shows what is installed and installs a missing driver in one click: it downloads the pinned release from the project's GitHub page, checks it (usbip-win2 against its published SHA-256, HidHide by its author's Authenticode signature, since that release publishes no hash), and opens its installer, which asks for admin. `install.rs` has the URLs.

| Driver | Used for | On the reference PC |
|---|---|---|
| [usbip-win2 **0.9.7.7**](https://github.com/vadimgrn/usbip-win2/releases/tag/v.0.9.7.7) (free, BSD-2, Microsoft attestation-signed) | Every virtual controller: Xbox 360, DualShock 4, and DualSense with game haptics | Works. Xbox 360 in XInput slot 0 within 0.19 s with rumble and LED back; DualShock 4 read back through HID; DualSense input at the physical report rate, with output reports and the 4-channel audio endpoint at 48 kHz. **Not 0.9.7.8**, which winget installs by default: its author warns it corrupts memory, it blue-screened the reference PC twice, and the app refuses it |
| [HidHide **1.5.230**](https://github.com/nefarius/HidHide/releases/tag/v1.5.230.0) (free, open source, signed) | Hiding the physical controller from games while a virtual one is up | Works |

The virtual DualSense is the app's own code (`virt\usbip\`): a USB/IP server on 127.0.0.1 that answers every URB, attached through usbip-win2's `PLUGIN_HARDWARE_ONCE` IOCTL. Isochronous audio URBs complete at the end of their real-time window, so the Windows audio engine runs at 48 kHz, and PCM reaches the haptics streamer as soon as a URB arrives. Its USB serial number (served in its own device descriptor) starts with `DSZV`, which is how the app skips its own virtual pad when it looks for physical controllers. When a game asks it for calibration, pairing, or firmware information, it answers with the physical controller's own reports, read when that controller connected (neutral calibration and a DSZ address before any has); other feature requests are declined. Its HID report descriptor is the one dumped from a real DualSense in [dogtopus's write-up](https://gist.github.com/dogtopus/894da226d73afb3bdd195df41b3a26aa). The installed release is read from `usbip.exe`'s version, since the driver files have none, and 0.9.7.8 is refused.

No user-mode API creates a virtual audio endpoint, and InputInjector gamepads are invisible to XInput, so neither feature is fully driverless.

## Windows identity

`build.rs` embeds a `VERSIONINFO` resource (`FileDescription` and `ProductName` "DSZ - DualSense Zen", version from `Cargo.toml`) and the app icon, drawn by `src\icon_art.rs` (the same DualSense outline the overview page draws) and written as a multi-size `.ico` at build time. Task Manager, Startup apps, the taskbar's tray-icon settings, notifications, and Explorer show "DSZ" and the icon instead of `dsz.exe`. It uses the Windows SDK's `rc.exe` through the `winresource` build dependency. Without the SDK the build still succeeds, with a warning and no version info.

## Layout

| Path | What |
|---|---|
| `crates\ds-proto` | Pure protocol: input parse, `0x31`/`0x02` output, CRC, trigger effects, the `0x32` haptics report, virtual-pad reports |
| `crates\dsz` | The app: Win32 HID (`hid.rs`), device threads (`device.rs`), output composer, input pipeline, haptics engine (`haptics\`, game audio in `game.rs`), virtual controllers (`virt\`: USB/IP Xbox 360, DualShock 4, and DualSense, HidHide, default-audio guard, deadzones), profiles, UDP, game watcher, tray, egui UI (`ui\`) |
| `vendor\eframe` | eframe 0.32.3 (MIT or Apache-2.0) with one fix: it busy-polled for a repaint of a hidden window, so the app used a full core in the tray (now under 1%) |
| `crates\dsz\defaults` | The profiles a fresh install starts with |

```powershell
cargo test --workspace
```

## Protocol sources

DSZ's protocol code was written from public documentation and code with open licenses:

| Source | License | Used for |
|---|---|---|
| Linux [`hid-playstation`](https://github.com/torvalds/linux/blob/master/drivers/hid/hid-playstation.c) | GPL-2.0, read as documentation | Input and output report layouts, valid flags, status bits, CRC prefixes, feature reports, the DualShock 4 report |
| SDL [`SDL_hidapi_ps5.c`](https://github.com/libsdl-org/SDL/blob/main/src/joystick/hidapi/SDL_hidapi_ps5.c) | zlib | Edge Fn and paddle buttons, the connection-state byte, improved rumble emulation |
| [pydualsense](https://github.com/flok/pydualsense) | MIT | The Bluetooth `0x31` layout, player LED patterns and brightness, motor power, custom trigger modes |
| [SAxense](https://apps.sdore.me/SAxense) and [godot-dualsense-native](https://github.com/Lelisvaldo/godot-dualsense-native/blob/main/docs/PROTOCOL.md) | MPL-2.0, MIT | The Bluetooth haptics report `0x32` |
| Monado [`pssense_protocol.h`](https://monado.pages.freedesktop.org/monado/pssense__protocol_8h_source.html) | Boost | The motor power reduction nibbles |
| [dualsensectl](https://github.com/nowrep/dualsensectl) | GPL-2.0, read as documentation | Rumble power as an attenuation of 0–7 |
| [Nielk1's trigger effect generator](https://gist.github.com/Nielk1/6d54cc2c00d2201ccb8c2720ad7538db) | MIT | Adaptive trigger effect encoding |
| [DS4Windows](https://github.com/Ryochan7/DS4Windows) | GPL-3.0, read as documentation | The stick curve settings: dead zone, max zone, sensitivity curve, anti-dead zone, square stick, max output; rumble power in 12.5 % steps |
| [dogtopus's descriptor dump](https://gist.github.com/dogtopus/894da226d73afb3bdd195df41b3a26aa) and [nondebug/dualsense](https://github.com/nondebug/dualsense) | public dumps | The virtual DualSense's HID report descriptor and USB layout |

Planned: controller-speaker audio over Bluetooth.

## Support

DSZ is free. If it's useful to you and you'd like to support it, you can [buy me a coffee on Ko-fi](https://ko-fi.com/maximumfreak). Bug reports in Issues help just as much.

## Thanks

DSZ builds on the work of [Jays2Kings](https://github.com/Jays2Kings/DS4Windows), [Ryochan7](https://github.com/Ryochan7/DS4Windows), [Schmaldeo](https://github.com/schmaldeo/DS4Windows), [hbashton](https://github.com/hbashton/DS4Windows), and the DS4Windows community, with thanks to [VIIPER](https://github.com/Alia5/VIIPER), [HidHide](https://github.com/nefarius/HidHide), [usbip-win2](https://github.com/vadimgrn/usbip-win2), and the wider controller-research community.


## License

MIT, see [LICENSE](LICENSE). eframe in `vendor\eframe` is MIT or Apache-2.0 (the published crate, with one patch). DualSense, DualSense Edge, DualShock, and PlayStation are trademarks of Sony Interactive Entertainment; this project is not affiliated with Sony.

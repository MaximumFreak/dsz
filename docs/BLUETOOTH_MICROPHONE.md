# Bluetooth microphone (experimental)

DSZ connects the DualSense Bluetooth microphone stream to the existing
virtual DualSense USB Audio capture endpoint. It supports the controller's
built-in microphone and a compatible TRRS headset plugged into the controller.
The new path has automated protocol and audio-buffer tests; physical controller,
headset, firmware and Windows recording tests are still required.

## Use

1. Run DSZ with exclusive access to the controller; close other controller tools.
2. Pair the controller over Bluetooth. Select a profile with **Virtual controller
   → DualSense**. The same usbip-win2 driver DSZ already uses must be installed.
3. Open **Haptics & audio → Bluetooth microphone (experimental)** and enable it.
4. Select **Auto**, **Built-in**, or **Headset**. Auto gives a headset microphone
   priority when a compatible headset is plugged into the controller's 3.5 mm
   jack. A headset paired directly to the PC is a different recording device.
5. In Windows Sound settings, Discord or the game, select the virtual
   **Wireless Controller** microphone. Its displayed name may vary with Windows
   language and cached device names. DSZ does not change your default input.
6. Speak and make a test recording. The DSZ meter measures decoded Bluetooth
   audio, before the virtual USB device; it does not prove an app is recording.

Microphone capture is off by default for existing profiles. The setting is
per profile, independent of the haptics toggle and speaker volume. The physical
Mute button can mute recording; the mute LED reflects the profile/button mute
while microphone capture is enabled. Windows' own microphone mute/volume controls
are also applied at the USB endpoint, independently of that LED.

While enabled and unmuted, Bluetooth capture remains active even when no Windows
application has the endpoint open. It also keeps the idle-off timer awake so a
voice call is not interrupted by controller inactivity. Disable or mute capture
when it is not needed; continuous capture uses more controller battery.

The microphone is exposed only while a virtual **DualSense** is active. Selecting
Xbox 360, DualShock 4 or Off stops this capture path. A headset needs a microphone
and a compatible four-pole connector; ordinary three-pole headphones provide
only output. For USB-wired use, select the physical controller's native Windows
microphone instead.

## Implementation

```text
DualSense Bluetooth HID 0x31/type 2 + CRC
    → bounded encoded queue (8 packets)
    → per-controller Opus decoder worker
    → source gain / mute gate
    → bounded stereo 48 kHz signed 16-bit PCM queue (100 ms)
    → virtual DualSense USB Audio capture endpoint
    → Windows recording applications
```

The Bluetooth media command preserves input reports while enabling the microphone.
Every haptics/speaker configuration preserves the microphone enable bit. When
there is no speaker or haptics audio, a quiet control packet maintains capture.
Source selection updates only microphone-related fields of the classic control
report, preserving the speaker/headset output path and trigger/rumble controls.

The HID reader validates report length, type and CRC and queues compressed data;
it does not decode Opus or wait for USB audio. The decoder handles sequence gaps
with bounded Opus packet-loss concealment. USB underflows return silence, and
overflow drops oldest complete stereo frames. Mute, disable, source changes,
virtual-device changes and disconnect clear buffered speech. Capture epochs
prevent work already being decoded from publishing after a mute transition.

No new driver is introduced. The existing virtual device already advertised a
microphone endpoint, but upstream returned zero-filled packets. Upstream also
already decoded microphone packets for speaker diagnostics. This implementation
connects those mechanisms into continuous capture.

## Validation and limits

Automated tests cover CRC/type/length checks, stale-packet rejection after mute,
bounded queues, gain clipping, USB packet sizes/offsets, underflow, mute controls,
source/output bit preservation and compatibility with existing profile JSON.
They do not establish real Bluetooth delivery, microphone quality, radio load,
latency or headset routing on a particular controller.

Before treating the feature as stable, test built-in and headset recording,
headset insertion/removal, profile changes, mute/unmute, disconnect/reconnect,
and recording while speaker audio, game haptics and adaptive triggers run.
If no level appears, verify Bluetooth connection, active virtual DualSense,
source selection and mute; inspect the DSZ log for errors. If the meter moves
but a recording is silent, check the Windows input device, microphone permission
and app/device mute and volume.

## Protocol references

These sources describe protocol facts; no external implementation was copied.

- [DSZ 0.3.6 source](https://github.com/MaximumFreak/dsz/tree/5431bfa7e7eb068f2d7c77e61d2271c7d33b49f6):
  existing diagnostic decoder, HID reader, stream builder and USB/IP endpoint.
- [SAxense research discussion](https://github.com/egormanga/SAxense/issues/1):
  microphone control packets, feature flags and Opus transport. The discussion
  includes qualifications; the DSZ's existing packets and tests also informed
  the implementation.
- [DS5Dongle](https://github.com/awalol/DS5Dongle): microphone enable preservation
  in media packets and microphone source selection.
- [DualSense report notes](https://gist.github.com/stealth-alex/10a8e7cc6027b78fa18a7f48a0d3d1e4):
  microphone volume, input source bits and hardware mute bit.
- [Sony/Linux HID driver](https://github.com/torvalds/linux/blob/master/drivers/hid/hid-playstation.c):
  common report layout and headset/microphone detection bits.

Capture is opt-in per physical connection. Before it has been enabled, the
original media configuration bytes are preserved. Once enabled, microphone
admission follows recording/mute state. Idle standalone controls are sent on
state changes, and refreshed only while recording. Disabling capture releases
hardware microphone selection/mute; speaker diagnostics do not retain ownership.

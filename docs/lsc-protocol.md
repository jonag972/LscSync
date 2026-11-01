# LSC Smart Connect controller protocol

What is known about the LSC light panel kit (sold at Action), found by trial and error on the
hardware, by listening to the official app and from Tuya's light strip templates. What the kit
actually does (which fields it honors, how fast it follows) is only known from these trials.

## Local connection

- **Tuya local protocol v3.5** (TCP 6668), through [`rustuya`](https://crates.io/crates/rustuya).
- It needs the device **ID** (`device_id`) and its **local key** (`local_key`, 16 characters).
- The local key **changes every time the device is re-paired** in the LSC app.
- The controller accepts **one local connection at a time**: a new engine waits for the
  previous one to close its connection.
- Devices announce themselves on the local network over UDP (ports 6666, 6667 and 7000), which
  gives their IP address.

## Getting the local key: QR code login

An **LSC Smart Connect** account works with Tuya's QR code login ("device sharing"), with no
developer account:

1. In the LSC app: Me › Settings › Account and Security › **User Code**.
2. `POST https://apigw.iotbing.com/v1.0/m/life/home-assistant/qrcode/tokens?clientid=…&usercode=…&schema=…`
   returns a token; the QR code to display contains `tuyaSmart--qrLogin?token=<token>`.
3. The user scans it with the LSC app's scanner and confirms.
4. `GET …/qrcode/tokens/<token>?clientid=…&usercode=…` returns the access tokens and the region
   endpoint (e.g. `https://apigw.tuyaeu.com`).
5. The "customer" API lists homes, then devices, **including `local_key`** (signed requests
   encrypted with AES-GCM, see Tuya's
   [`tuya-device-sharing-sdk`](https://github.com/tuya/tuya-device-sharing-sdk), MIT).

This service was built for Home Assistant: its URL contains `home-assistant` and it requires a
client ID registered with Tuya.

## Data points (DP)

| DP | Role | Value |
|---|---|---|
| 20 | On / off | boolean |
| 21 | Mode | `white`, `colour`, `scene`, `music` |
| 22 | White brightness | 10–1000 |
| 24 | Single color | `HHHHSSSSVVVV` hexadecimal (0–360, 0–1000, 0–1000) |
| 27 | Music data: instant single color | 21 hexadecimal digits, see below |
| 51 | "Dreamlight" scene (`dreamlight_scene_mode`) | base64, see below |
| 52 | Music with the light's own microphone (`dreamlightmic_music_data`) | see below |
| 61 | Color on a list of panels (`paint_colour_data`) | base64, see below |

The names in brackets come from Tuya's standard for RGBIC light strips (category `dd`), which
the kit follows. Tuya's cloud description of the kit lists none of DPs 51, 52 and 61
(`lscsync-cli describe`, 01/11/2026): only 20, 21, 22, 24, 26 and `music_data`.

### DP 24: single color

The controller does a **hardware fade** to each new color, which looks sluggish in a continuous
stream. Music mode therefore uses DP 51, which is applied instantly.

### DP 27: `music_data`, instant single color

Tuya's standard music function, listed in the device's cloud specification (without its DP id).
Tested on the panels on 29/10/2026:

```text
change mode (1 hex digit, 0 = direct) | hue HHHH (0-360) | saturation SSSS | value VVVV
| white brightness BBBB | temperature TTTT          e.g. 0 0000 03e8 03e8 0000 0000 = red
```

- Needs mode `music` on DP 21.
- With mode `0` (direct), the color changes **instantly**: sharp cuts where DP 24 fades.
- Saturation and value are on **0-1000**, although the cloud specification says 0-255 (255 gives
  a dimmer color).
- Streamed at 25 frames/s for 20 s without waiting for replies: no loss, no freeze.
- Being a standard function, it is a candidate for fast single-color control of other Tuya
  lights too.

### DP 51: scene, used for continuous streaming

```text
01 | scene id | mode | speed | speed | options | 00 00 | brightness (0-100)
   | N × [hue (2 bytes, 0-360), saturation (0-100)]
```

- Mode `00` = static. Other modes start built-in animations (e.g. `08` = "snake"), not
  direct control.
- Option `0x80` = split into zones. Without it: the first color everywhere.
- The controller drives **12 segments** (the LSC app shows 12, in a U): the kit's panels are
  segments 1 to 6, and the controller can probably take up to 12 panels.
- **12 colors + `0x80`: one color per panel, applied instantly** (29/10/2026): color *i* goes
  to panel *i* (the same numbering as DP 61), colors 7 to 12 to segments without a panel. Held
  25 frames/s for 2 minutes without a freeze. This is what Ambilight uses.
- **6 colors + `0x80`**: each color covers 2 segments, so the kit shows **3 zones of 2
  panels** and colors 4 to 6 land on segments without a panel. Music mode still uses this.
- A single brightness for the whole scene. **A single panel can't be turned off** (01/11/2026):
  hue 0 / saturation 0 shows white, and out-of-range segments (saturation 101 or 255, hue 361,
  `0x8000` or `0xFFFF`) are clamped into range and show a color, never off.
- Applied **instantly** (no fade).
- Earlier trials (13 option and mode variants, the `0x60` option of official scenes) never gave
  anything finer than 3 pairs; the 12-color scene above does.
- Implementation: `crates/core/src/protocol.rs`.

### DP 61: per-panel control

Tuya's format (`SmearFormatter.ts` in Tuya's
[strip light template](https://github.com/Tuya-Community/tuya-ray-materials), template
`PublicPanelStripLamp`), which matches every frame seen from the kit and from the official app:

```text
version (00) | mode | effect | segments (0C = 12) | ...
mode 00, white:       action | brightness (2 bytes, 0-1000) | temperature (2 bytes, 0-1000)
mode 01, colour:      action | hue (2 bytes) | saturation (2 bytes) | value (2 bytes, 10-1000)
                      [actions 1 and 2] | 0x80 + N | N × panel number (1-6, up to 12)
mode 03, combination: type | N × [hue | saturation | value] (2 bytes each)
action: 00 = paint bucket (every panel), 01 = pencil (the listed panels), 02 = eraser (off)
effect, on this kit: 00 = ~0.5 s fade, 01 = instant (Tuya's template says 01 = gradient)
```

**This is the instant per-panel control**: color *and* brightness for each panel, panels off,
applied at once with effect `01` (01/11/2026). Frames sent by the official LSC app while the
user picked colors panel by panel, all with effect `01`, all applied without a fade:

```text
00 01 01 0c 01 0000 03e8 03e8 81 01              panel 1 red, 100 %
00 01 01 0c 01 0000 03e8 01f3 81 01              panel 1 red, 50 %
00 01 01 0c 01 0000 03e8 000a 81 01              panel 1 red, the app's minimum (1 %)
00 01 01 0c 01 0078 03e8 012b 82 03 04           panels 3 and 4 green, 30 %
00 01 01 0c 02 0000 0000 0000 81 05              panel 5 off
00 01 01 0c 01 0000 0000 01f4 8c 01 02 … 0c      the 12 segments white, 50 % (the app's
                                                 "all panels", which also sets the color)
```

- In `colour` mode only (DP 21). Sent while the DP 51 scene runs (25 frames/s), it is ignored
  and not acknowledged.
- The kit acknowledges each command by reporting DP 61 back. When it changes mode it reports
  DP 61 itself, e.g. `00 00 00 0C 00 00F0 03E8` going to white (brightness 240).
- **Combination is not supported** (01/11/2026): 230 commands (one at a time and at 5, 10 and
  25 frames/s) were ignored, not acknowledged, and changed nothing.
- **Effect `01`, streamed without waiting for replies** (01/11/2026, colors from the screen,
  the panels with about the same color in one command): 10 and 20 commands/s for 60 s follow
  the screen with no visible delay.
- **The limit is commands per second, not panels** (01/11/2026, a wave going round the panels,
  a very different state on every lap, watched by eye, 15 s per rate): 1 panel per command,
  10, 20, 30 and 40 commands/s applied perfectly, 50/s stutters and stalls; 6 panels per
  command, 20 and 40 commands/s perfect, 60/s (30 s) froze. A first few seconds can stutter
  right after connecting.
- **Acknowledgments stop being a measure above ~10 commands/s**: at 10/s every command is
  reported back; at 20 to 60/s only a fraction is (sometimes none), although every command is
  applied.
- **Effect `00`, one panel, without waiting for replies** (01/11/2026, red / blue on panel 1
  for 5 s): 4 and 10 commands/s, every command acknowledged right away, no loss, no freeze. 25
  commands/s: the controller falls behind, still acknowledging (and showing) the burst seconds
  after it ended.
- **Earlier trials, effect `00` and waiting for each reply** (29/10/2026, all 6 panels changing
  on every frame, one command per panel): 2 and 4 frames/s (12 and 24 commands/s) for 60 and
  90 s, no command lost, no freeze. ~8 frames/s (48 commands/s): the controller stops answering
  after ~20 s, then comes back on its own about a minute later. The fade then looked like ~2 s.
  Sent without waiting, commands got lost (older connection handling).

### Fade time: not adjustable

Tuya's lighting standard has DP 48 `white_gradi_time` and DP 49 `colour_gradi_time` (fade
duration in ms, 0-1000) and DP 35 `switch_gradient` (fade when turning on and off). The kit
has none of them that we can see (01/11/2026): `49 = 0` was neither acknowledged nor
reported, and the DP 61 fade stayed the same. Not needed anyway: DP 61 with effect `01` is
instant.

### DP 52: music with the light's microphone

Tuya's format (`MicMusicFormatter.ts`, same template), not tried on the kit:

```text
version | on | music id | options (high nibble: 1 = 10 % glow when silent; low nibble: mode,
0 jump, 1 gradient, 2 breathe, 3 flash) | speed | sensitivity | 3 reserved | brightness
| N × [hue (2 bytes), saturation (1 byte)]
```

It configures the light's own microphone mode: one brightness, nothing per panel, nothing to
stream. For the phone's microphone, Tuya's apps stream `music_data` (DP 27), a single color.

### What the kit reports

- A status query returns DPs 20, 21, 22, 24, 26 and 34 (`do_not_disturb` in Tuya's lighting
  standard, `false`).
- Asked to refresh any list of DPs (command `0x12`, 01/11/2026), it always reports the same
  four: 51 and 61 (the last scene and paint commands), and **31 and 32**, `sleep_mode` and
  `wakeup_mode` in Tuya's standard (scheduled fade to off and to on; `00 01 00 00 06 17 …`
  and `00 01 00 00 06 07 1e …`). No other DP showed up, but write-only ones may exist.
- It doesn't answer a device info request (command `0x25`).
- DPs 51 and 61 are missing from Tuya's cloud description of the device.

## Frame rate

- DP 51 frames are sent **without waiting for the acknowledgment**. The controller keeps up with
  ~50 frames/s, and the eye can't tell the difference above ~20 Hz. The app sends 25 frames/s in
  screen modes and ~50 frames/s in Music mode (one per audio block).
- A frame is only sent when the color has changed enough (hue, saturation and brightness
  thresholds in `crates/engine/src/worker.rs`).

# LSC Smart Connect controller protocol

What is known about the LSC light panel kit (sold at Action), found by trial and error on the
hardware and by listening to the official app. The DP 51 and DP 61 formats are not documented
anywhere else.

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
| 51 | "Dreamlight" scene | base64, see below |
| 61 | Color on a list of panels | base64, see below |

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
- A single brightness for the whole scene.
- Applied **instantly** (no fade).
- Earlier trials (13 option and mode variants, the `0x60` option of official scenes) never gave
  anything finer than 3 pairs; the 12-color scene above does.
- Implementation: `crates/core/src/protocol.rs`.

### DP 61: per-panel control

Seen in frames sent by the official LSC app:

```text
00 01 | 00 0C | 01 | hue (2 bytes, degrees) | saturation (2 bytes, 0-1000)
      | brightness? (2 bytes, the app sends 01 AE = 430) | 0x80 + N | N × panel number (1-6)
```

- One color on any list of panels, one command per color.
- **Each command must wait for the controller's reply**: sent without waiting, commands get
  lost. A reply takes ~30 ms once the connection is warm (~120 ms during the first seconds).
- **Streaming works within a budget** (29/10/2026, all 6 panels changing
  on every frame):
  - 2 frames/s (12 commands/s) for 60 s and 4 frames/s (24 commands/s) for 90 s: no command
    lost, no freeze.
  - As fast as the replies allow (~8 frames/s, 48 commands/s): the controller stops answering
    after ~20 s, then comes back on its own about a minute later.
  - The earlier Python implementation froze even at ~4 frames/s; the difference is the
    connection handling of `rustuya`.
- **Each panel fades to its new color (~2 s)**, whatever the header bytes tried; DP 61 is
  ignored outside `colour` mode. Too slow for Ambilight: use the 12-color DP 51 scene.
- The official app never streams it: it sends one command per color picked by hand.
- The panels' local status lists DPs 20, 21, 22, 26 and 34 (unknown, `false`); DPs 51 and 61 are
  write-only and missing from Tuya's cloud description of the device.

## Frame rate

- DP 51 frames are sent **without waiting for the acknowledgment**. The controller keeps up with
  ~50 frames/s, and the eye can't tell the difference above ~20 Hz. The app sends 25 frames/s in
  screen modes and ~50 frames/s in Music mode (one per audio block).
- A frame is only sent when the color has changed enough (hue, saturation and brightness
  thresholds in `crates/engine/src/worker.rs`).

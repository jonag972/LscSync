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
| 51 | "Dreamlight" scene | base64, see below |
| 61 | Color on a list of panels | base64, see below |

### DP 24: single color

The controller does a **hardware fade** to each new color, which looks sluggish in a continuous
stream. Music mode therefore uses DP 51, which is applied instantly.

### DP 51: scene, used for continuous streaming

```text
01 | scene id | mode | speed | speed | options | 00 00 | brightness (0-100)
   | N × [hue (2 bytes, 0-360), saturation (0-100)]
```

- Mode `00` = static. Other modes start built-in animations (e.g. `08` = "snake"), not
  direct control.
- Option `0x80` = split into zones. Without it: the first color everywhere.
- **6 colors + `0x80`**: the 6-panel Z-shaped kit shows **3 zones of 2 panels**.
  Color 1 = right zone, color 2 = middle, color 3 = left; colors 4 to 6 are ignored but must be
  present.
- A single brightness for the whole scene.
- Applied **instantly** (no fade).
- 13 option and mode variants, 12 colors, and the `0x60` option used by official scenes never
  gave anything finer than 3 pairs of panels.
- Implementation: `crates/core/src/protocol.rs`.

### DP 61: per-panel control

Seen in frames sent by the official LSC app:

```text
00 01 | 00 0C | 01 | hue (2 bytes, degrees) | saturation (2 bytes, 0-1000)
      | brightness? (2 bytes, the app sends 01 AE = 430) | 0x80 + N | N × panel number (1-6)
```

- One color on any list of panels, one command per color.
- **Reliable for one-off commands** (single command, waiting for the reply).
- **Unusable for continuous streaming**: without waiting for replies, commands get lost; when
  waiting for them (~4 to 10 frames/s), the controller freezes after a few seconds. The official
  app never streams it either.
- A lead for manual control (one color per panel, picked by hand).

## Frame rate

- DP 51 frames are sent **without waiting for the acknowledgment**. The controller keeps up with
  ~50 frames/s, and the eye can't tell the difference above ~20 Hz. The app sends 25 frames/s in
  screen modes and ~50 frames/s in Music mode (one per audio block).
- A frame is only sent when the color has changed enough (hue, saturation and brightness
  thresholds in `crates/engine/src/worker.rs`).

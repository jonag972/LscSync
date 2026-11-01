# Changelog

All notable changes to LscSync are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

<!-- Release workflow: the release notes are read from the section of the
     released version (## [X.Y.Z]). Add it in the "Version X.Y.Z" commit,
     before pushing the vX.Y.Z tag — the release fails fast otherwise. -->

## [Unreleased]

## [0.2.8] - 2026-11-01

### Added

- Lights with several zones are detected the first time they join a sync:
  a light that reports the multi-zone scene back gets one color per zone,
  like the LSC panel kit. Tuya's description of a device doesn't say it.
- `lscsync-cli describe` prints everything Tuya says about each device of
  the account, local keys hidden.

### Changed

- The app, the README and the installers talk about lights and zones
  instead of panels, since LscSync drives any LSC Smart Connect light.
- Plainer wording: "Click" instead of "Tap", "Done" at the end of setup,
  "Logs" in Settings, and an up-to-date Linux package description.

## [0.2.7] - 2026-11-01

### Changed

- Music mode follows the default sound output device live: the automatic
  Bluetooth delay turns on and off when a Bluetooth speaker or headphones is
  connected or removed, the delay slider shows the current automatic value,
  and on Windows the loopback capture restarts on the new output device.

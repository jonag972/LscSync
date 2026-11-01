# Changelog

All notable changes to LscSync are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

<!-- Release workflow: the release notes are read from the section of the
     released version (## [X.Y.Z]). Add it in the "Version X.Y.Z" commit,
     before pushing the vX.Y.Z tag — the release fails fast otherwise. -->

## [Unreleased]

## [0.2.7] - 2026-11-01

### Changed

- Music mode follows the default sound output device live: the automatic
  Bluetooth delay turns on and off when a Bluetooth speaker or headphones is
  connected or removed, the delay slider shows the current automatic value,
  and on Windows the loopback capture restarts on the new output device.

# Bloom Squash Engine (Polarity-SC-Dark fork)

This is a GPL-3.0-or-later derivative of **Polarity-SC-Dark 0.5.4**, modified for the Ableton Bloom Audio Effect Rack project.

## Goal

The fork addresses one specific fidelity gap in recreating Oeksound Bloom's documented **Squash** behavior:

- frequency-dependent upward and downward compression
- higher thresholds in frequency regions where hearing is less sensitive
- lower thresholds in more-sensitive regions
- **harder knees** in less-sensitive regions
- **softer knees** in more-sensitive regions
- a continuously variable Squash intensity parameter so the Ableton rack can map Bloom `amount` 7→10 without an abrupt compressor on/off transition
- a `Squash Cal` detector calibration parameter

Oeksound does not publish Bloom's proprietary numeric equal-loudness curve, knee widths, ratios, or mapping coefficients. This fork therefore matches the **documented architecture and direction of behavior**, not Bloom's undisclosed constants. The starting perceptual contour reuses Polarity-SC-Dark's built-in Equal Loudness curve anchors.

## New host parameters

- **Squash Amount** (`squash_amount`) — 0–100%. 0% produces no level-dependent Squash gain change; 100% applies the configured spectral compression fully.
- **Squash Cal** (`squash_cal`) — -24 to +24 dB detector calibration. Positive values make the detector see a hotter signal; negative values make it see a quieter signal.
- Existing **Attack** and **Release** remain host-automatable and control both upward and downward spectral compression.

## Perceptual knee modification

Polarity-SC-Dark originally used one global knee width for all FFT bins. This fork keeps the user-facing base knee parameter but derives a knee width per FFT bin from the same equal-loudness contour shape used for perceptual threshold weighting:

- least-sensitive areas → narrower/harder knee
- most-sensitive areas → wider/softer knee

At the current defaults, a 6 dB base knee maps approximately from **1.98 dB around 35 Hz** to **12 dB around 3.5 kHz**. These values are a tunable starting model, not claimed Oeksound constants.

## Plugin identity

The VST3 class ID and display name are changed so this fork can coexist with the original Polarity-SC-Dark installation.

- Display name: `Bloom Squash Engine`
- VST3 class ID bytes: `BloomSquashEng01`
- Version: `0.1.0`

## Build

On Windows with Rust and the Visual C++ build tools installed:

```powershell
cargo xtask bundle polarity_sc_dark --release
```

The bundle is written to `target/bundled/`. See `BUILD_WINDOWS.md` for a practical setup path.

## License

GPL-3.0-or-later. See `LICENSE`, `COPYING`, and `ORIGINAL_LICENSE_GPL3.txt`. The upstream project copyright notices remain in the source files.

See `MODIFICATIONS.md` for the exact project-specific changes.

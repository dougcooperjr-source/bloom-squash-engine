# Modifications

Modified: 2026-09-04

Base: Polarity-SC-Dark 0.5.4, provided by the user as the upstream source ZIP.

## DSP changes

1. Added per-bin knee-width vectors for upward and downward compressor banks.
2. Added a fixed perceptual knee contour derived from Polarity-SC-Dark's existing built-in Equal Loudness anchor values.
3. Knee width now varies with auditory sensitivity:
   - high equal-loudness threshold offset / lower sensitivity → harder knee
   - low equal-loudness threshold offset / higher sensitivity → softer knee
4. Added `Squash Amount` and scale the computed spectral gain difference continuously from neutral to full Squash.
5. Added `Squash Cal`, applied in the detector domain before the upward/downward transfer functions.
6. Changed default upward/downward ratios to 2:1 and disabled the legacy high-frequency ratio rolloff so frequency weighting is driven by perceptual threshold/knee contours instead of an unrelated HF ratio taper.
7. Initialized the default threshold points from the existing Equal Loudness anchors and made the internal baseline slope neutral.

## Identity changes

- Plugin display name: `Bloom Squash Engine`
- Vendor display string: `Heavy Projects (Polarity-SC-Dark fork)`
- VST3 class ID: `BloomSquashEng01`
- CLAP ID: `local.heavy-projects.bloom-squash-engine`
- Package version: 0.1.0
- Bundle display name: `Bloom Squash Engine`

## Fidelity boundary

Oeksound's public Bloom manual specifies the qualitative relationship between equal-loudness sensitivity, threshold, and knee hardness, but does not publish the exact numeric DSP constants. This fork therefore fixes the architectural limitation (global knee) while leaving the precise contour/ratio tuning as an empirical calibration step.

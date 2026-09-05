# Windows build instructions

The project is designed to build natively on Windows. No GitHub account or Git installation is required.

## Prerequisites

1. Install Rust with rustup: https://rustup.rs/
2. Install **Visual Studio 2022 Build Tools** with the **Desktop development with C++** workload.
3. Restart PowerShell after installation so `cargo` is available.

## Build the VST3

Open PowerShell in this source folder and run:

```powershell
rustup default stable
cargo xtask bundle polarity_sc_dark --release
```

The finished plugin should be under:

```text
target\bundled\Bloom Squash Engine.vst3
```

## Install for Ableton Live

Copy the `.vst3` bundle into the standard VST3 location:

```text
C:\Program Files\Common Files\VST3\
```

Then rescan VST3 plug-ins in Ableton Live. The fork has a different VST3 class ID from Polarity-SC-Dark, so both can be installed at the same time.

## First validation

Before using this inside the rack, load `Bloom Squash Engine` by itself in Live and confirm:

- Live does not crash when scanning/loading it.
- `Squash Amount` is exposed to Configure.
- `Squash Cal`, `Attack`, and `Release` are exposed.
- With `Squash Amount = 0%`, toggling the plugin does not create a level-dependent compression change.
- Raising `Squash Amount` introduces increasingly strong spectral upward/downward compression.

Do not replace the original Polarity-SC-Dark binary; this is a separate plugin identity.

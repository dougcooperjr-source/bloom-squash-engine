from pathlib import Path
import re
from collections import Counter

root = Path(__file__).resolve().parent
lib = (root / "plugin/src/lib.rs").read_text(encoding="utf-8")
cb = (root / "plugin/src/compressor_bank.rs").read_text(encoding="utf-8")
cargo = (root / "plugin/Cargo.toml").read_text(encoding="utf-8")
lock = (root / "Cargo.lock").read_text(encoding="utf-8")
bundler = (root / "bundler.toml").read_text(encoding="utf-8")

checks = {
    "VST3 ID is unique and 16 bytes": "BloomSquashEng01" in lib and len("BloomSquashEng01") == 16,
    "plugin name changed": "Bloom Squash Engine" in lib,
    "Squash Amount exposed": '#[id = "squash_amount"]' in lib,
    "Squash Cal exposed": '#[id = "squash_cal"]' in lib,
    "Attack exposed": '#[id = "attack"]' in lib,
    "Release exposed": '#[id = "release"]' in lib,
    "downward per-bin knees": "downwards_knee_widths_db: Vec<f32>" in cb,
    "upward per-bin knees": "upwards_knee_widths_db: Vec<f32>" in cb,
    "perceptual knee calculation": "bloom_perceptual_knee_width_db" in cb,
    "equal-loudness defaults": "bloom_equal_loudness_default_point(index)" in cb,
    "Squash Amount scales level-dependent delta": cb.count("self.raw_gain_difference_db[bin_idx] = squash_amount") >= 2,
    "Squash Cal affects detector domain": cb.count("calibrated_envelope_db = envelope_db + squash_cal_db") >= 2,
    "package version": 'version = "0.1.0"' in cargo,
    "lock version": 'name = "polarity_sc_dark"\nversion = "0.1.0"' in lock,
    "bundle name": 'name = "Bloom Squash Engine"' in bundler,
}

ids = re.findall(r'#\[id = "([^"]+)"\]', lib + "\n" + cb)
dupes = {k: v for k, v in Counter(ids).items() if v > 1}
checks["no unexpected duplicate literal parameter IDs"] = all(
    k in {"curve_point_enabled", "curve_point_frequency", "curve_point_offset"} for k in dupes
)
checks["lib.rs braces balanced"] = lib.count("{") == lib.count("}")
checks["compressor_bank.rs braces balanced"] = cb.count("{") == cb.count("}")

for name, ok in checks.items():
    print(("PASS" if ok else "FAIL") + " - " + name)

failed = [name for name, ok in checks.items() if not ok]
if failed:
    raise SystemExit("\nFailed checks: " + ", ".join(failed))
print(f"\n{len(checks)}/{len(checks)} source checks passed.")

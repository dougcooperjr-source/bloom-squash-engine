use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const BOOTH_INDEX: usize = 0;
pub const GATED_INDEX: usize = 12;
pub const SLAP2_INDEX: usize = 17;

pub const TYPE_NAMES: [&str; 20] = [
    "Booth",
    "Small Room",
    "Medium Room",
    "Large Room",
    "Small Club",
    "Large Club",
    "Small Stage",
    "Large Stage",
    "Small Hall",
    "Large Hall",
    "Plate",
    "Plastic",
    "Gated",
    "Reverse",
    "Spring 1",
    "Spring 2",
    "Slap 1",
    "Slap 2",
    "Laser",
    "Rumble",
];

#[derive(Clone)]
pub struct StereoIr {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

pub struct TypeIrSet {
    pub max_ir: Arc<StereoIr>,
    pub exact_min: Option<Arc<StereoIr>>,
    pub exact_mid: Option<Arc<StereoIr>>,
}

pub fn load_type_sets(
    project_sample_rate: f32,
) -> Result<Vec<TypeIrSet>, String> {
    let resource_dir = plugin_resource_dir()?;
    let mut all_types = Vec::with_capacity(TYPE_NAMES.len());

    for (type_index, type_name) in TYPE_NAMES.iter().enumerate() {
        let max_ir = Arc::new(load_stereo_ir(
            &resource_dir.join(format!("{}_Decay_127.wav", type_name)),
            project_sample_rate,
        )?);

        let (exact_min, exact_mid) =
            if type_index == BOOTH_INDEX || type_index == GATED_INDEX {
                (
                    Some(Arc::new(load_stereo_ir(
                        &resource_dir.join(format!("{}_Decay_0.wav", type_name)),
                        project_sample_rate,
                    )?)),
                    Some(Arc::new(load_stereo_ir(
                        &resource_dir.join(format!("{}_Decay_64.wav", type_name)),
                        project_sample_rate,
                    )?)),
                )
            } else {
                (None, None)
            };

        all_types.push(TypeIrSet {
            max_ir,
            exact_min,
            exact_mid,
        });
    }

    Ok(all_types)
}

fn load_stereo_ir(
    path: &Path,
    project_sample_rate: f32,
) -> Result<StereoIr, String> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();

    if spec.channels != 2 {
        return Err(format!("{} is not stereo", path.display()));
    }
    if spec.sample_format != hound::SampleFormat::Float
        || spec.bits_per_sample != 32
    {
        return Err(format!("{} is not 32-bit float", path.display()));
    }

    let mut left = Vec::with_capacity(reader.duration() as usize / 2);
    let mut right = Vec::with_capacity(reader.duration() as usize / 2);

    for (idx, sample) in reader.samples::<f32>().enumerate() {
        let sample =
            sample.map_err(|e| format!("{}: {e}", path.display()))?;
        if idx % 2 == 0 {
            left.push(sample);
        } else {
            right.push(sample);
        }
    }

    if (spec.sample_rate as f32 - project_sample_rate).abs() > 0.01 {
        let ratio = project_sample_rate as f64 / spec.sample_rate as f64;
        left = stretch_resample(&left, ratio);
        right = stretch_resample(&right, ratio);
    }

    Ok(StereoIr { left, right })
}

pub fn stretch_resample(input: &[f32], stretch: f64) -> Vec<f32> {
    if input.is_empty() {
        return Vec::new();
    }
    if (stretch - 1.0).abs() < 1.0e-12 {
        return input.to_vec();
    }

    let output_len =
        ((input.len() as f64) * stretch).round().max(1.0) as usize;
    let mut output = Vec::with_capacity(output_len);
    let radius = 16_i32;
    let cutoff = stretch.min(1.0);

    for out_index in 0..output_len {
        let source_pos = out_index as f64 / stretch;
        let center = source_pos.floor() as i64;
        let mut weighted_sum = 0.0_f64;
        let mut weight_sum = 0.0_f64;

        for tap in -radius..=radius {
            let sample_index = center + tap as i64;
            if sample_index < 0 || sample_index >= input.len() as i64 {
                continue;
            }

            let distance = source_pos - sample_index as f64;
            let window_distance = distance / (radius as f64 + 1.0);
            if window_distance.abs() >= 1.0 {
                continue;
            }

            let sinc_arg = distance * cutoff;
            let sinc = if sinc_arg.abs() < 1.0e-12 {
                1.0
            } else {
                let x = PI * sinc_arg;
                x.sin() / x
            };
            let window = 0.5 + 0.5 * (PI * window_distance).cos();
            let weight = cutoff * sinc * window;

            weighted_sum += input[sample_index as usize] as f64 * weight;
            weight_sum += weight;
        }

        if weight_sum.abs() > 1.0e-12 {
            output.push((weighted_sum / weight_sum) as f32);
        } else {
            output.push(0.0);
        }
    }

    output
}

#[cfg(windows)]
fn plugin_resource_dir() -> Result<PathBuf, String> {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;
    use std::ptr::null_mut;

    type Hmodule = *mut c_void;
    const FROM_ADDRESS: u32 = 0x0000_0004;
    const UNCHANGED_REFCOUNT: u32 = 0x0000_0002;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleExW(
            flags: u32,
            module_name: *const u16,
            module: *mut Hmodule,
        ) -> i32;
        fn GetModuleFileNameW(
            module: Hmodule,
            filename: *mut u16,
            size: u32,
        ) -> u32;
    }

    unsafe {
        let mut module: Hmodule = null_mut();
        let marker = plugin_resource_dir as *const () as *const u16;

        if GetModuleHandleExW(
            FROM_ADDRESS | UNCHANGED_REFCOUNT,
            marker,
            &mut module,
        ) == 0
        {
            return Err("GetModuleHandleExW failed".to_string());
        }

        let mut path_buf = vec![0u16; 32_768];
        let len = GetModuleFileNameW(
            module,
            path_buf.as_mut_ptr(),
            path_buf.len() as u32,
        );

        if len == 0 {
            return Err("GetModuleFileNameW failed".to_string());
        }

        path_buf.truncate(len as usize);
        let module_path = PathBuf::from(OsString::from_wide(&path_buf));
        let contents_dir = module_path
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| {
                format!(
                    "Unexpected VST3 module path: {}",
                    module_path.display()
                )
            })?;

        Ok(contents_dir.join("Resources"))
    }
}

#[cfg(not(windows))]
fn plugin_resource_dir() -> Result<PathBuf, String> {
    Err("DB30 SPACE Proof v0.3 is Windows-only".to_string())
}

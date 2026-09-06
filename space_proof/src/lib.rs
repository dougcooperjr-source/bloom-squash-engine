use nih_plug::prelude::*;
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const PARTITION_SIZE: usize = 256;
const FFT_SIZE: usize = PARTITION_SIZE * 2;
const VST3_CLASS_ID_BYTES: [u8; 16] = *b"DB30SPACEPROOF01";

const TYPE_NAMES: [&str; 2] = ["Booth", "Gated"];
const DECAY_ANCHORS: [i32; 3] = [0, 64, 127];

pub struct Db30SpaceProof {
    params: Arc<Db30SpaceProofParams>,
    convolver: Option<PartitionedConvolver>,
    last_type: i32,
}

#[derive(Params)]
pub struct Db30SpaceProofParams {
    #[id = "type"]
    pub type_index: IntParam,
    #[id = "decay"]
    pub decay: IntParam,
}

impl Default for Db30SpaceProofParams {
    fn default() -> Self {
        Self {
            type_index: IntParam::new(
                "Type (0 Booth / 1 Gated)",
                0,
                IntRange::Linear { min: 0, max: 1 },
            ),
            decay: IntParam::new(
                "Decay",
                127,
                IntRange::Linear { min: 0, max: 127 },
            ),
        }
    }
}

impl Default for Db30SpaceProof {
    fn default() -> Self {
        Self {
            params: Arc::new(Db30SpaceProofParams::default()),
            convolver: None,
            last_type: -1,
        }
    }
}

impl Plugin for Db30SpaceProof {
    const NAME: &'static str = "DB30 SPACE Proof";
    const VENDOR: &'static str = "Heavy Projects";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        context: &mut impl InitContext<Self>,
    ) -> bool {
        match load_time_anchors(buffer_config.sample_rate) {
            Ok(anchors) => {
                self.convolver = Some(PartitionedConvolver::new(anchors));
                context.set_latency_samples(PARTITION_SIZE as u32);
                nih_log!("DB30 SPACE Proof loaded measured Booth/Gated IR anchors");
            }
            Err(err) => {
                self.convolver = None;
                context.set_latency_samples(0);
                nih_log!("DB30 SPACE Proof could not load resources: {err}");
            }
        }
        self.last_type = -1;
        true
    }

    fn reset(&mut self) {
        if let Some(convolver) = &mut self.convolver {
            convolver.reset();
        }
        self.last_type = -1;
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let Some(convolver) = &mut self.convolver else {
            return ProcessStatus::Normal;
        };

        let type_index = self.params.type_index.value().clamp(0, 1);
        let decay = self.params.decay.value().clamp(0, 127);
        if type_index != self.last_type {
            convolver.reset();
            self.last_type = type_index;
        }

        let channels = buffer.as_slice();
        if channels.len() < 2 {
            return ProcessStatus::Normal;
        }
        let (left_part, right_part) = channels.split_at_mut(1);
        let left = &mut left_part[0];
        let right = &mut right_part[0];

        for sample_idx in 0..left.len() {
            let (out_l, out_r) = convolver.process_sample(
                left[sample_idx],
                right[sample_idx],
                type_index as usize,
                decay,
            );
            left[sample_idx] = out_l;
            right[sample_idx] = out_r;
        }

        ProcessStatus::Normal
    }
}

impl Vst3Plugin for Db30SpaceProof {
    const VST3_CLASS_ID: [u8; 16] = VST3_CLASS_ID_BYTES;
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[
        Vst3SubCategory::Fx,
        Vst3SubCategory::Reverb,
    ];
}

nih_export_vst3!(Db30SpaceProof);

#[derive(Clone)]
struct StereoIr {
    left: Vec<f32>,
    right: Vec<f32>,
}

struct KernelSet {
    left: Vec<Vec<Complex32>>,
    right: Vec<Vec<Complex32>>,
}

type TypeKernels = [KernelSet; 3];

struct PartitionedConvolver {
    anchors: Vec<TypeKernels>,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
    num_parts: usize,
    num_bins: usize,

    history_l: Vec<Vec<Complex32>>,
    history_r: Vec<Vec<Complex32>>,
    history_pos: usize,

    input_l: Vec<f32>,
    input_r: Vec<f32>,
    output_l: Vec<f32>,
    output_r: Vec<f32>,
    overlap_l: Vec<f32>,
    overlap_r: Vec<f32>,
    input_pos: usize,
    output_pos: usize,

    fft_time: Vec<f32>,
    x_spec: Vec<Complex32>,
    y_spec_l: Vec<Complex32>,
    y_spec_r: Vec<Complex32>,
    ifft_l: Vec<f32>,
    ifft_r: Vec<f32>,
}

impl PartitionedConvolver {
    fn new(time_anchors: Vec<[StereoIr; 3]>) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(FFT_SIZE);
        let c2r = planner.plan_fft_inverse(FFT_SIZE);
        let num_bins = FFT_SIZE / 2 + 1;

        let max_len = time_anchors
            .iter()
            .flat_map(|t| t.iter())
            .map(|ir| ir.left.len().max(ir.right.len()))
            .max()
            .unwrap_or(PARTITION_SIZE);
        let num_parts = (max_len + PARTITION_SIZE - 1) / PARTITION_SIZE;

        let mut anchors = Vec::with_capacity(time_anchors.len());
        for type_set in time_anchors {
            anchors.push([
                build_kernel_set(&type_set[0], num_parts, r2c.as_ref()),
                build_kernel_set(&type_set[1], num_parts, r2c.as_ref()),
                build_kernel_set(&type_set[2], num_parts, r2c.as_ref()),
            ]);
        }

        Self {
            anchors,
            r2c,
            c2r,
            num_parts,
            num_bins,
            history_l: vec![vec![Complex32::default(); num_bins]; num_parts],
            history_r: vec![vec![Complex32::default(); num_bins]; num_parts],
            history_pos: 0,
            input_l: vec![0.0; PARTITION_SIZE],
            input_r: vec![0.0; PARTITION_SIZE],
            output_l: vec![0.0; PARTITION_SIZE],
            output_r: vec![0.0; PARTITION_SIZE],
            overlap_l: vec![0.0; PARTITION_SIZE],
            overlap_r: vec![0.0; PARTITION_SIZE],
            input_pos: 0,
            output_pos: 0,
            fft_time: vec![0.0; FFT_SIZE],
            x_spec: vec![Complex32::default(); num_bins],
            y_spec_l: vec![Complex32::default(); num_bins],
            y_spec_r: vec![Complex32::default(); num_bins],
            ifft_l: vec![0.0; FFT_SIZE],
            ifft_r: vec![0.0; FFT_SIZE],
        }
    }

    fn reset(&mut self) {
        for part in &mut self.history_l {
            part.fill(Complex32::default());
        }
        for part in &mut self.history_r {
            part.fill(Complex32::default());
        }
        self.input_l.fill(0.0);
        self.input_r.fill(0.0);
        self.output_l.fill(0.0);
        self.output_r.fill(0.0);
        self.overlap_l.fill(0.0);
        self.overlap_r.fill(0.0);
        self.history_pos = 0;
        self.input_pos = 0;
        self.output_pos = 0;
    }

    fn process_sample(
        &mut self,
        input_l: f32,
        input_r: f32,
        type_index: usize,
        decay: i32,
    ) -> (f32, f32) {
        let out = (self.output_l[self.output_pos], self.output_r[self.output_pos]);

        self.input_l[self.input_pos] = input_l;
        self.input_r[self.input_pos] = input_r;
        self.input_pos += 1;
        self.output_pos += 1;

        if self.input_pos == PARTITION_SIZE {
            self.process_block(type_index.min(self.anchors.len() - 1), decay);
            self.input_pos = 0;
            self.output_pos = 0;
        }

        out
    }

    fn process_block(&mut self, type_index: usize, decay: i32) {
        self.fft_time.fill(0.0);
        self.fft_time[..PARTITION_SIZE].copy_from_slice(&self.input_l);
        self.r2c
            .process_with_scratch(&mut self.fft_time, &mut self.x_spec, &mut [])
            .expect("DB30 SPACE Proof forward FFT failed");
        self.history_l[self.history_pos].copy_from_slice(&self.x_spec);

        self.fft_time.fill(0.0);
        self.fft_time[..PARTITION_SIZE].copy_from_slice(&self.input_r);
        self.r2c
            .process_with_scratch(&mut self.fft_time, &mut self.x_spec, &mut [])
            .expect("DB30 SPACE Proof forward FFT failed");
        self.history_r[self.history_pos].copy_from_slice(&self.x_spec);

        self.y_spec_l.fill(Complex32::default());
        self.y_spec_r.fill(Complex32::default());

        let (anchor_a, anchor_b, alpha) = decay_interpolation(decay);
        let kernels_a = &self.anchors[type_index][anchor_a];
        let kernels_b = &self.anchors[type_index][anchor_b];

        for part_idx in 0..self.num_parts {
            let hist_idx = (self.history_pos + self.num_parts - part_idx) % self.num_parts;
            let x_l = &self.history_l[hist_idx];
            let x_r = &self.history_r[hist_idx];
            let h_la = &kernels_a.left[part_idx];
            let h_lb = &kernels_b.left[part_idx];
            let h_ra = &kernels_a.right[part_idx];
            let h_rb = &kernels_b.right[part_idx];

            for bin in 0..self.num_bins {
                let h_l = complex_lerp(h_la[bin], h_lb[bin], alpha);
                let h_r = complex_lerp(h_ra[bin], h_rb[bin], alpha);
                self.y_spec_l[bin] += x_l[bin] * h_l;
                self.y_spec_r[bin] += x_r[bin] * h_r;
            }
        }

        self.c2r
            .process_with_scratch(&mut self.y_spec_l, &mut self.ifft_l, &mut [])
            .expect("DB30 SPACE Proof inverse FFT failed");
        self.c2r
            .process_with_scratch(&mut self.y_spec_r, &mut self.ifft_r, &mut [])
            .expect("DB30 SPACE Proof inverse FFT failed");

        let scale = 1.0 / FFT_SIZE as f32;
        for i in 0..PARTITION_SIZE {
            self.output_l[i] = self.ifft_l[i] * scale + self.overlap_l[i];
            self.output_r[i] = self.ifft_r[i] * scale + self.overlap_r[i];
            self.overlap_l[i] = self.ifft_l[i + PARTITION_SIZE] * scale;
            self.overlap_r[i] = self.ifft_r[i + PARTITION_SIZE] * scale;
        }

        self.history_pos = (self.history_pos + 1) % self.num_parts;
    }
}

fn complex_lerp(a: Complex32, b: Complex32, alpha: f32) -> Complex32 {
    Complex32::new(
        a.re + (b.re - a.re) * alpha,
        a.im + (b.im - a.im) * alpha,
    )
}

fn decay_interpolation(decay: i32) -> (usize, usize, f32) {
    let d = decay.clamp(0, 127);
    if d <= DECAY_ANCHORS[1] {
        (0, 1, d as f32 / DECAY_ANCHORS[1] as f32)
    } else {
        (
            1,
            2,
            (d - DECAY_ANCHORS[1]) as f32
                / (DECAY_ANCHORS[2] - DECAY_ANCHORS[1]) as f32,
        )
    }
}

fn build_kernel_set(
    ir: &StereoIr,
    num_parts: usize,
    r2c: &dyn RealToComplex<f32>,
) -> KernelSet {
    KernelSet {
        left: build_channel_kernels(&ir.left, num_parts, r2c),
        right: build_channel_kernels(&ir.right, num_parts, r2c),
    }
}

fn build_channel_kernels(
    samples: &[f32],
    num_parts: usize,
    r2c: &dyn RealToComplex<f32>,
) -> Vec<Vec<Complex32>> {
    let num_bins = FFT_SIZE / 2 + 1;
    let mut result = Vec::with_capacity(num_parts);
    let mut time = vec![0.0f32; FFT_SIZE];
    let mut spec = vec![Complex32::default(); num_bins];

    for part_idx in 0..num_parts {
        time.fill(0.0);
        let start = part_idx * PARTITION_SIZE;
        if start < samples.len() {
            let end = (start + PARTITION_SIZE).min(samples.len());
            time[..end - start].copy_from_slice(&samples[start..end]);
        }
        r2c.process_with_scratch(&mut time, &mut spec, &mut [])
            .expect("DB30 SPACE Proof IR FFT failed");
        result.push(spec.clone());
    }

    result
}

fn load_time_anchors(project_sample_rate: f32) -> Result<Vec<[StereoIr; 3]>, String> {
    let resource_dir = plugin_resource_dir()?;
    let mut all_types = Vec::with_capacity(TYPE_NAMES.len());

    for type_name in TYPE_NAMES {
        let a0 = load_stereo_ir(
            &resource_dir.join(format!("{}_Decay_0.wav", type_name)),
            project_sample_rate,
        )?;
        let a64 = load_stereo_ir(
            &resource_dir.join(format!("{}_Decay_64.wav", type_name)),
            project_sample_rate,
        )?;
        let a127 = load_stereo_ir(
            &resource_dir.join(format!("{}_Decay_127.wav", type_name)),
            project_sample_rate,
        )?;
        all_types.push([a0, a64, a127]);
    }

    Ok(all_types)
}

fn load_stereo_ir(path: &Path, project_sample_rate: f32) -> Result<StereoIr, String> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 2 {
        return Err(format!("{} is not stereo", path.display()));
    }
    if spec.sample_format != hound::SampleFormat::Float || spec.bits_per_sample != 32 {
        return Err(format!("{} is not 32-bit float", path.display()));
    }

    let mut left = Vec::with_capacity(reader.duration() as usize / 2);
    let mut right = Vec::with_capacity(reader.duration() as usize / 2);
    for (idx, sample) in reader.samples::<f32>().enumerate() {
        let sample = sample.map_err(|e| format!("{}: {e}", path.display()))?;
        if idx % 2 == 0 {
            left.push(sample);
        } else {
            right.push(sample);
        }
    }

    if (spec.sample_rate as f32 - project_sample_rate).abs() > 0.01 {
        left = linear_resample(&left, spec.sample_rate as f32, project_sample_rate);
        right = linear_resample(&right, spec.sample_rate as f32, project_sample_rate);
    }

    Ok(StereoIr { left, right })
}

fn linear_resample(input: &[f32], source_rate: f32, target_rate: f32) -> Vec<f32> {
    if input.is_empty() || (source_rate - target_rate).abs() < 0.01 {
        return input.to_vec();
    }
    let output_len = ((input.len() as f64) * target_rate as f64 / source_rate as f64)
        .round()
        .max(1.0) as usize;
    let mut output = Vec::with_capacity(output_len);
    let ratio = source_rate as f64 / target_rate as f64;
    for i in 0..output_len {
        let pos = i as f64 * ratio;
        let idx = pos.floor() as usize;
        let frac = (pos - idx as f64) as f32;
        if idx + 1 < input.len() {
            output.push(input[idx] + (input[idx + 1] - input[idx]) * frac);
        } else {
            output.push(*input.last().unwrap_or(&0.0));
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
        fn GetModuleHandleExW(flags: u32, module_name: *const u16, module: *mut Hmodule) -> i32;
        fn GetModuleFileNameW(module: Hmodule, filename: *mut u16, size: u32) -> u32;
    }

    unsafe {
        let mut module: Hmodule = null_mut();
        let marker = plugin_resource_dir as *const () as *const u16;
        if GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT, marker, &mut module) == 0 {
            return Err("GetModuleHandleExW failed".to_string());
        }

        let mut path_buf = vec![0u16; 32_768];
        let len = GetModuleFileNameW(module, path_buf.as_mut_ptr(), path_buf.len() as u32);
        if len == 0 {
            return Err("GetModuleFileNameW failed".to_string());
        }
        path_buf.truncate(len as usize);
        let module_path = PathBuf::from(OsString::from_wide(&path_buf));
        let contents_dir = module_path
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| format!("Unexpected VST3 module path: {}", module_path.display()))?;
        Ok(contents_dir.join("Resources"))
    }
}

#[cfg(not(windows))]
fn plugin_resource_dir() -> Result<PathBuf, String> {
    Err("DB30 SPACE Proof v0.1 is Windows-only".to_string())
}

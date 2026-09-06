use nih_plug::prelude::*;
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::f64::consts::PI;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread;

const PARTITION_SIZE: usize = 256;
const FFT_SIZE: usize = PARTITION_SIZE * 2;
const VST3_CLASS_ID_BYTES: [u8; 16] = *b"DB30SPACEPROOF01";
const MAX_STRETCH: f32 = 2.0;
const MAX_PREDELAY_MS: i32 = 300;

const TYPE_NAMES: [&str; 20] = [
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
const DECAY_ANCHORS: [i32; 3] = [0, 64, 127];
const BOOTH_INDEX: usize = 0;
const GATED_INDEX: usize = 12;

pub struct Db30SpaceProof {
    params: Arc<Db30SpaceProofParams>,
    convolver: Option<PartitionedConvolver>,
    predelay: WetPredelay,
    rebuild_tx: Option<SyncSender<BuildRequest>>,
    rebuild_rx: Option<Receiver<BuildResult>>,
    next_generation: u64,
    latest_requested_generation: u64,
    last_requested_type: i32,
    last_requested_stretch: i32,
}

#[derive(Params)]
pub struct Db30SpaceProofParams {
    #[id = "type"]
    pub type_index: IntParam,
    #[id = "decay"]
    pub decay: IntParam,
    #[id = "stretch"]
    pub stretch: IntParam,
    #[id = "predelay"]
    pub predelay_ms: IntParam,
}

impl Default for Db30SpaceProofParams {
    fn default() -> Self {
        Self {
            type_index: IntParam::new(
                "Type (DB30 order 0-19)",
                0,
                IntRange::Linear { min: 0, max: 19 },
            ),
            decay: IntParam::new(
                "Decay",
                127,
                IntRange::Linear { min: 0, max: 127 },
            ),
            stretch: IntParam::new(
                "Stretch (%)",
                100,
                IntRange::Linear { min: 50, max: 200 },
            ),
            predelay_ms: IntParam::new(
                "Predelay (ms)",
                0,
                IntRange::Linear {
                    min: 0,
                    max: MAX_PREDELAY_MS,
                },
            ),
        }
    }
}

impl Default for Db30SpaceProof {
    fn default() -> Self {
        Self {
            params: Arc::new(Db30SpaceProofParams::default()),
            convolver: None,
            predelay: WetPredelay::default(),
            rebuild_tx: None,
            rebuild_rx: None,
            next_generation: 1,
            latest_requested_generation: 0,
            last_requested_type: 0,
            last_requested_stretch: 100,
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
        self.rebuild_tx = None;
        self.rebuild_rx = None;

        match load_time_anchors(buffer_config.sample_rate) {
            Ok(raw_types) => {
                let max_num_parts = max_partition_count(&raw_types);
                let initial_num_parts = stretched_partition_count(&raw_types[BOOTH_INDEX], 100);
                let initial_kernels = build_type_kernels(
                    &raw_types[BOOTH_INDEX],
                    100,
                    initial_num_parts,
                );
                self.convolver = Some(PartitionedConvolver::new(
                    initial_kernels,
                    initial_num_parts,
                    max_num_parts,
                ));
                self.predelay = WetPredelay::new(buffer_config.sample_rate, MAX_PREDELAY_MS);

                let (request_tx, request_rx) = mpsc::sync_channel::<BuildRequest>(1);
                let (result_tx, result_rx) = mpsc::channel::<BuildResult>();
                spawn_kernel_worker(raw_types, request_rx, result_tx);
                self.rebuild_tx = Some(request_tx);
                self.rebuild_rx = Some(result_rx);

                self.next_generation = 1;
                self.latest_requested_generation = 0;
                self.last_requested_type = 0;
                self.last_requested_stretch = 100;
                context.set_latency_samples(PARTITION_SIZE as u32);
                nih_log!(
                    "DB30 SPACE Proof v0.2 loaded all 20 DB30 IRs; Booth/Gated have measured Decay anchors"
                );
            }
            Err(err) => {
                self.convolver = None;
                self.predelay = WetPredelay::default();
                context.set_latency_samples(0);
                nih_log!("DB30 SPACE Proof could not load resources: {err}");
            }
        }
        true
    }

    fn reset(&mut self) {
        if let Some(convolver) = &mut self.convolver {
            convolver.reset();
        }
        self.predelay.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        self.receive_rebuilt_kernels();

        let requested_type = self.params.type_index.value().clamp(0, 19);
        let requested_stretch = self.params.stretch.value().clamp(50, 200);
        self.request_rebuild_if_needed(requested_type, requested_stretch);

        let decay = self.params.decay.value().clamp(0, 127);
        let predelay_ms = self.params.predelay_ms.value().clamp(0, MAX_PREDELAY_MS);
        self.predelay.set_delay_ms(predelay_ms);

        let Some(convolver) = &mut self.convolver else {
            return ProcessStatus::Normal;
        };

        let channels = buffer.as_slice();
        if channels.len() < 2 {
            return ProcessStatus::Normal;
        }
        let (left_part, right_part) = channels.split_at_mut(1);
        let left = &mut left_part[0];
        let right = &mut right_part[0];

        for sample_idx in 0..left.len() {
            let (wet_l, wet_r) = convolver.process_sample(left[sample_idx], right[sample_idx], decay);
            let (out_l, out_r) = self.predelay.process_sample(wet_l, wet_r);
            left[sample_idx] = out_l;
            right[sample_idx] = out_r;
        }

        ProcessStatus::Normal
    }
}

impl Db30SpaceProof {
    fn request_rebuild_if_needed(&mut self, type_index: i32, stretch_percent: i32) {
        if type_index == self.last_requested_type && stretch_percent == self.last_requested_stretch {
            return;
        }

        let Some(tx) = self.rebuild_tx.clone() else {
            return;
        };

        let generation = self.next_generation;
        let request = BuildRequest {
            generation,
            type_index: type_index as usize,
            stretch_percent,
        };

        match tx.try_send(request) {
            Ok(()) => {
                self.next_generation = self.next_generation.wrapping_add(1).max(1);
                self.latest_requested_generation = generation;
                self.last_requested_type = type_index;
                self.last_requested_stretch = stretch_percent;
            }
            Err(TrySendError::Full(_)) => {
                // The worker is already rebuilding. Keep the old requested values so the
                // latest host value will be retried on the next process call.
            }
            Err(TrySendError::Disconnected(_)) => {
                self.rebuild_tx = None;
            }
        }
    }

    fn receive_rebuilt_kernels(&mut self) {
        let mut disconnected = false;
        let mut installed = false;

        if let Some(rx) = self.rebuild_rx.as_ref() {
            loop {
                match rx.try_recv() {
                    Ok(result) => {
                        if result.generation == self.latest_requested_generation {
                            if let Some(convolver) = &mut self.convolver {
                                convolver.install_kernels(result.kernels, result.num_parts);
                                installed = true;
                            }
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        if installed {
            self.predelay.reset();
        }
        if disconnected {
            self.rebuild_rx = None;
        }
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

struct TypeIrSet {
    anchors: [Arc<StereoIr>; 3],
    decay_calibrated: bool,
}

struct KernelSet {
    left: Vec<Vec<Complex32>>,
    right: Vec<Vec<Complex32>>,
}

type TypeKernels = [Arc<KernelSet>; 3];

struct BuildRequest {
    generation: u64,
    type_index: usize,
    stretch_percent: i32,
}

struct BuildResult {
    generation: u64,
    kernels: TypeKernels,
    num_parts: usize,
}

struct PartitionedConvolver {
    kernels: TypeKernels,
    r2c: Arc<dyn RealToComplex<f32>>,
    c2r: Arc<dyn ComplexToReal<f32>>,
    num_parts: usize,
    max_num_parts: usize,
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
    fn new(kernels: TypeKernels, num_parts: usize, max_num_parts: usize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(FFT_SIZE);
        let c2r = planner.plan_fft_inverse(FFT_SIZE);
        let num_bins = FFT_SIZE / 2 + 1;

        Self {
            kernels,
            r2c,
            c2r,
            num_parts,
            max_num_parts,
            num_bins,
            history_l: vec![vec![Complex32::default(); num_bins]; max_num_parts],
            history_r: vec![vec![Complex32::default(); num_bins]; max_num_parts],
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

    fn install_kernels(&mut self, kernels: TypeKernels, num_parts: usize) {
        self.kernels = kernels;
        self.num_parts = num_parts.clamp(1, self.max_num_parts);
        self.reset();
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

    fn process_sample(&mut self, input_l: f32, input_r: f32, decay: i32) -> (f32, f32) {
        let out = (self.output_l[self.output_pos], self.output_r[self.output_pos]);

        self.input_l[self.input_pos] = input_l;
        self.input_r[self.input_pos] = input_r;
        self.input_pos += 1;
        self.output_pos += 1;

        if self.input_pos == PARTITION_SIZE {
            self.process_block(decay);
            self.input_pos = 0;
            self.output_pos = 0;
        }

        out
    }

    fn process_block(&mut self, decay: i32) {
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
        let kernels_a = &self.kernels[anchor_a];
        let kernels_b = &self.kernels[anchor_b];

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

#[derive(Default)]
struct WetPredelay {
    left: Vec<f32>,
    right: Vec<f32>,
    write_pos: usize,
    delay_samples: usize,
    sample_rate: f32,
}

impl WetPredelay {
    fn new(sample_rate: f32, max_delay_ms: i32) -> Self {
        let max_delay_samples = ((sample_rate as f64 * max_delay_ms as f64 / 1000.0).ceil()
            as usize)
            .max(1);
        Self {
            left: vec![0.0; max_delay_samples + 1],
            right: vec![0.0; max_delay_samples + 1],
            write_pos: 0,
            delay_samples: 0,
            sample_rate,
        }
    }

    fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write_pos = 0;
    }

    fn set_delay_ms(&mut self, delay_ms: i32) {
        if self.left.is_empty() {
            self.delay_samples = 0;
            return;
        }
        let samples = (self.sample_rate as f64 * delay_ms as f64 / 1000.0).round() as usize;
        self.delay_samples = samples.min(self.left.len() - 1);
    }

    fn process_sample(&mut self, input_l: f32, input_r: f32) -> (f32, f32) {
        if self.left.is_empty() || self.delay_samples == 0 {
            return (input_l, input_r);
        }

        let len = self.left.len();
        let read_pos = (self.write_pos + len - self.delay_samples) % len;
        let out = (self.left[read_pos], self.right[read_pos]);
        self.left[self.write_pos] = input_l;
        self.right[self.write_pos] = input_r;
        self.write_pos += 1;
        if self.write_pos == len {
            self.write_pos = 0;
        }
        out
    }
}

fn spawn_kernel_worker(
    raw_types: Vec<TypeIrSet>,
    request_rx: Receiver<BuildRequest>,
    result_tx: Sender<BuildResult>,
) {
    thread::Builder::new()
        .name("DB30 SPACE IR builder".to_string())
        .spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let type_index = request.type_index.min(raw_types.len().saturating_sub(1));
                let type_ir = &raw_types[type_index];
                let num_parts = stretched_partition_count(type_ir, request.stretch_percent);
                let kernels = build_type_kernels(type_ir, request.stretch_percent, num_parts);
                if result_tx
                    .send(BuildResult {
                        generation: request.generation,
                        kernels,
                        num_parts,
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .ok();
}

fn build_type_kernels(type_ir: &TypeIrSet, stretch_percent: i32, num_parts: usize) -> TypeKernels {
    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(FFT_SIZE);

    if type_ir.decay_calibrated {
        let k0 = Arc::new(build_stretched_kernel_set(
            &type_ir.anchors[0],
            stretch_percent,
            num_parts,
            r2c.as_ref(),
        ));
        let k64 = Arc::new(build_stretched_kernel_set(
            &type_ir.anchors[1],
            stretch_percent,
            num_parts,
            r2c.as_ref(),
        ));
        let k127 = Arc::new(build_stretched_kernel_set(
            &type_ir.anchors[2],
            stretch_percent,
            num_parts,
            r2c.as_ref(),
        ));
        [k0, k64, k127]
    } else {
        let kernel = Arc::new(build_stretched_kernel_set(
            &type_ir.anchors[2],
            stretch_percent,
            num_parts,
            r2c.as_ref(),
        ));
        [kernel.clone(), kernel.clone(), kernel]
    }
}

fn build_stretched_kernel_set(
    ir: &StereoIr,
    stretch_percent: i32,
    num_parts: usize,
    r2c: &dyn RealToComplex<f32>,
) -> KernelSet {
    let stretch = stretch_percent.clamp(50, 200) as f64 / 100.0;
    let left = stretch_resample(&ir.left, stretch);
    let right = stretch_resample(&ir.right, stretch);
    KernelSet {
        left: build_channel_kernels(&left, num_parts, r2c),
        right: build_channel_kernels(&right, num_parts, r2c),
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

fn max_partition_count(raw_types: &[TypeIrSet]) -> usize {
    let max_len = raw_types
        .iter()
        .flat_map(|t| t.anchors.iter())
        .map(|ir| ir.left.len().max(ir.right.len()))
        .max()
        .unwrap_or(PARTITION_SIZE);
    let stretched_len = ((max_len as f64) * MAX_STRETCH as f64).ceil() as usize;
    ((stretched_len + PARTITION_SIZE - 1) / PARTITION_SIZE).max(1)
}

fn stretched_partition_count(type_ir: &TypeIrSet, stretch_percent: i32) -> usize {
    let max_len = type_ir
        .anchors
        .iter()
        .map(|ir| ir.left.len().max(ir.right.len()))
        .max()
        .unwrap_or(PARTITION_SIZE);
    let stretch = stretch_percent.clamp(50, 200) as f64 / 100.0;
    let stretched_len = ((max_len as f64) * stretch).ceil() as usize;
    ((stretched_len + PARTITION_SIZE - 1) / PARTITION_SIZE).max(1)
}

fn load_time_anchors(project_sample_rate: f32) -> Result<Vec<TypeIrSet>, String> {
    let resource_dir = plugin_resource_dir()?;
    let mut all_types = Vec::with_capacity(TYPE_NAMES.len());

    for (type_index, type_name) in TYPE_NAMES.iter().enumerate() {
        if type_index == BOOTH_INDEX || type_index == GATED_INDEX {
            let a0 = Arc::new(load_stereo_ir(
                &resource_dir.join(format!("{}_Decay_0.wav", type_name)),
                project_sample_rate,
            )?);
            let a64 = Arc::new(load_stereo_ir(
                &resource_dir.join(format!("{}_Decay_64.wav", type_name)),
                project_sample_rate,
            )?);
            let a127 = Arc::new(load_stereo_ir(
                &resource_dir.join(format!("{}_Decay_127.wav", type_name)),
                project_sample_rate,
            )?);
            all_types.push(TypeIrSet {
                anchors: [a0, a64, a127],
                decay_calibrated: true,
            });
        } else {
            let a127 = Arc::new(load_stereo_ir(
                &resource_dir.join(format!("{}_Decay_127.wav", type_name)),
                project_sample_rate,
            )?);
            all_types.push(TypeIrSet {
                anchors: [a127.clone(), a127.clone(), a127],
                decay_calibrated: false,
            });
        }
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
        let ratio = project_sample_rate as f64 / spec.sample_rate as f64;
        left = stretch_resample(&left, ratio);
        right = stretch_resample(&right, ratio);
    }

    Ok(StereoIr { left, right })
}

fn stretch_resample(input: &[f32], stretch: f64) -> Vec<f32> {
    if input.is_empty() {
        return Vec::new();
    }
    if (stretch - 1.0).abs() < 1.0e-12 {
        return input.to_vec();
    }

    let output_len = ((input.len() as f64) * stretch).round().max(1.0) as usize;
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
    Err("DB30 SPACE Proof v0.2 is Windows-only".to_string())
}

use crate::decay_calibration::{DECAY_GAIN, DECAY_K_PER_S, DECAY_POSITIONS};
use crate::resources::{
    stretch_resample, StereoIr, TypeIrSet, SLAP2_INDEX,
};
use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;

pub(crate) const PARTITION_SIZE: usize = 256;
const FFT_SIZE: usize = PARTITION_SIZE * 2;
const MAX_STRETCH: f32 = 2.0;

pub(crate) struct KernelSet {
    left: Vec<Vec<Complex32>>,
    right: Vec<Vec<Complex32>>,
}

pub(crate) type TypeKernels = Vec<Arc<KernelSet>>;

pub(crate) struct BuildRequest {
    pub generation: u64,
    pub type_index: usize,
    pub stretch_percent: i32,
}

pub(crate) struct BuildResult {
    pub generation: u64,
    pub kernels: TypeKernels,
    pub num_parts: usize,
}

pub(crate) struct PartitionedConvolver {
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
    pub fn new(
        kernels: TypeKernels,
        num_parts: usize,
        max_num_parts: usize,
    ) -> Self {
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

    pub fn install_kernels(
        &mut self,
        kernels: TypeKernels,
        num_parts: usize,
    ) {
        self.kernels = kernels;
        self.num_parts = num_parts.clamp(1, self.max_num_parts);
        self.reset();
    }

    pub fn reset(&mut self) {
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

    pub fn process_sample(
        &mut self,
        input_l: f32,
        input_r: f32,
        decay: i32,
    ) -> (f32, f32) {
        let out = (
            self.output_l[self.output_pos],
            self.output_r[self.output_pos],
        );

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
            .process_with_scratch(
                &mut self.fft_time,
                &mut self.x_spec,
                &mut [],
            )
            .expect("DB30 SPACE Proof forward FFT failed");
        self.history_l[self.history_pos].copy_from_slice(&self.x_spec);

        self.fft_time.fill(0.0);
        self.fft_time[..PARTITION_SIZE].copy_from_slice(&self.input_r);
        self.r2c
            .process_with_scratch(
                &mut self.fft_time,
                &mut self.x_spec,
                &mut [],
            )
            .expect("DB30 SPACE Proof forward FFT failed");
        self.history_r[self.history_pos].copy_from_slice(&self.x_spec);

        self.y_spec_l.fill(Complex32::default());
        self.y_spec_r.fill(Complex32::default());

        let (anchor_a, anchor_b, alpha) = decay_interpolation(decay);
        let kernels_a = &self.kernels[anchor_a];
        let kernels_b = &self.kernels[anchor_b];

        for part_idx in 0..self.num_parts {
            let hist_idx =
                (self.history_pos + self.num_parts - part_idx) % self.num_parts;
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
            .process_with_scratch(
                &mut self.y_spec_l,
                &mut self.ifft_l,
                &mut [],
            )
            .expect("DB30 SPACE Proof inverse FFT failed");
        self.c2r
            .process_with_scratch(
                &mut self.y_spec_r,
                &mut self.ifft_r,
                &mut [],
            )
            .expect("DB30 SPACE Proof inverse FFT failed");

        let scale = 1.0 / FFT_SIZE as f32;
        for i in 0..PARTITION_SIZE {
            self.output_l[i] =
                self.ifft_l[i] * scale + self.overlap_l[i];
            self.output_r[i] =
                self.ifft_r[i] * scale + self.overlap_r[i];

            self.overlap_l[i] =
                self.ifft_l[i + PARTITION_SIZE] * scale;
            self.overlap_r[i] =
                self.ifft_r[i + PARTITION_SIZE] * scale;
        }

        self.history_pos =
            (self.history_pos + 1) % self.num_parts;
    }
}

#[derive(Default)]
pub(crate) struct WetPredelay {
    left: Vec<f32>,
    right: Vec<f32>,
    write_pos: usize,
    delay_samples: usize,
    sample_rate: f32,
}

impl WetPredelay {
    pub fn new(sample_rate: f32, max_delay_ms: i32) -> Self {
        let max_delay_samples =
            ((sample_rate as f64 * max_delay_ms as f64 / 1000.0).ceil()
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

    pub fn reset(&mut self) {
        self.left.fill(0.0);
        self.right.fill(0.0);
        self.write_pos = 0;
    }

    pub fn set_delay_ms(&mut self, delay_ms: i32) {
        if self.left.is_empty() {
            self.delay_samples = 0;
            return;
        }

        let samples =
            (self.sample_rate as f64 * delay_ms as f64 / 1000.0).round()
                as usize;
        self.delay_samples = samples.min(self.left.len() - 1);
    }

    pub fn process_sample(
        &mut self,
        input_l: f32,
        input_r: f32,
    ) -> (f32, f32) {
        if self.left.is_empty() || self.delay_samples == 0 {
            return (input_l, input_r);
        }

        let len = self.left.len();
        let read_pos =
            (self.write_pos + len - self.delay_samples) % len;
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

pub(crate) fn spawn_kernel_worker(
    raw_types: Vec<TypeIrSet>,
    sample_rate: f32,
    request_rx: Receiver<BuildRequest>,
    result_tx: Sender<BuildResult>,
) {
    thread::Builder::new()
        .name("DB30 SPACE IR builder".to_string())
        .spawn(move || {
            while let Ok(request) = request_rx.recv() {
                let type_index =
                    request.type_index.min(raw_types.len().saturating_sub(1));
                let type_ir = &raw_types[type_index];

                let num_parts =
                    stretched_partition_count(type_ir, request.stretch_percent);
                let kernels = build_type_kernels(
                    type_index,
                    type_ir,
                    request.stretch_percent,
                    num_parts,
                    sample_rate,
                );

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

pub(crate) fn build_type_kernels(
    type_index: usize,
    type_ir: &TypeIrSet,
    stretch_percent: i32,
    num_parts: usize,
    sample_rate: f32,
) -> TypeKernels {
    let mut planner = RealFftPlanner::<f32>::new();
    let r2c = planner.plan_fft_forward(FFT_SIZE);
    let mut kernels = Vec::with_capacity(DECAY_POSITIONS.len());

    for anchor_index in 0..DECAY_POSITIONS.len() {
        let modeled;
        let ir: &StereoIr;

        if anchor_index == 0 {
            if let Some(exact) = type_ir.exact_min.as_ref() {
                ir = exact.as_ref();
            } else {
                modeled = apply_decay_model(
                    type_index,
                    anchor_index,
                    type_ir.max_ir.as_ref(),
                    sample_rate,
                );
                ir = &modeled;
            }
        } else if DECAY_POSITIONS[anchor_index] == 64 {
            if let Some(exact) = type_ir.exact_mid.as_ref() {
                ir = exact.as_ref();
            } else {
                modeled = apply_decay_model(
                    type_index,
                    anchor_index,
                    type_ir.max_ir.as_ref(),
                    sample_rate,
                );
                ir = &modeled;
            }
        } else if anchor_index == DECAY_POSITIONS.len() - 1 {
            ir = type_ir.max_ir.as_ref();
        } else {
            modeled = apply_decay_model(
                type_index,
                anchor_index,
                type_ir.max_ir.as_ref(),
                sample_rate,
            );
            ir = &modeled;
        }

        kernels.push(Arc::new(build_stretched_kernel_set(
            ir,
            stretch_percent,
            num_parts,
            r2c.as_ref(),
        )));
    }

    kernels
}

fn apply_decay_model(
    type_index: usize,
    anchor_index: usize,
    max_ir: &StereoIr,
    sample_rate: f32,
) -> StereoIr {
    if type_index == SLAP2_INDEX && anchor_index == 0 {
        // Audio7 measured the Slap 2 minimum as left-silent while the
        // right-only response remained close to the first non-minimum anchor.
        let k = DECAY_K_PER_S[type_index][1];
        let gain = DECAY_GAIN[type_index][1];

        return StereoIr {
            left: vec![0.0; max_ir.left.len()],
            right: decay_channel(&max_ir.right, k, gain, sample_rate),
        };
    }

    let k = DECAY_K_PER_S[type_index][anchor_index];
    let gain = DECAY_GAIN[type_index][anchor_index];

    StereoIr {
        left: decay_channel(&max_ir.left, k, gain, sample_rate),
        right: decay_channel(&max_ir.right, k, gain, sample_rate),
    }
}

fn decay_channel(
    input: &[f32],
    k_per_s: f32,
    gain: f32,
    sample_rate: f32,
) -> Vec<f32> {
    if input.is_empty() {
        return Vec::new();
    }
    if k_per_s == 0.0 && (gain - 1.0).abs() < 1.0e-7 {
        return input.to_vec();
    }

    let inv_sr = 1.0 / sample_rate.max(1.0);

    input
        .iter()
        .enumerate()
        .map(|(index, &sample)| {
            let t = index as f32 * inv_sr;
            sample * gain * (-k_per_s * t).exp()
        })
        .collect()
}

fn build_stretched_kernel_set(
    ir: &StereoIr,
    stretch_percent: i32,
    num_parts: usize,
    r2c: &dyn RealToComplex<f32>,
) -> KernelSet {
    let stretch =
        stretch_percent.clamp(50, 200) as f64 / 100.0;
    let left = stretch_resample(&ir.left, stretch);
    let right = stretch_resample(&ir.right, stretch);

    KernelSet {
        left: build_channel_kernels(&left, num_parts, r2c),
        right: build_channel_kernels(&right, num_parts, r2c),
    }
}

fn complex_lerp(
    a: Complex32,
    b: Complex32,
    alpha: f32,
) -> Complex32 {
    Complex32::new(
        a.re + (b.re - a.re) * alpha,
        a.im + (b.im - a.im) * alpha,
    )
}

fn decay_interpolation(decay: i32) -> (usize, usize, f32) {
    let d = decay.clamp(0, 127);

    for index in 0..DECAY_POSITIONS.len() - 1 {
        let a = DECAY_POSITIONS[index];
        let b = DECAY_POSITIONS[index + 1];

        if d <= b {
            let alpha = (d - a) as f32 / (b - a) as f32;
            return (index, index + 1, alpha.clamp(0.0, 1.0));
        }
    }

    let last = DECAY_POSITIONS.len() - 1;
    (last, last, 0.0)
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
            let end =
                (start + PARTITION_SIZE).min(samples.len());
            time[..end - start].copy_from_slice(&samples[start..end]);
        }

        r2c.process_with_scratch(
            &mut time,
            &mut spec,
            &mut [],
        )
        .expect("DB30 SPACE Proof IR FFT failed");

        result.push(spec.clone());
    }

    result
}

fn max_type_len(type_ir: &TypeIrSet) -> usize {
    let mut max_len = type_ir
        .max_ir
        .left
        .len()
        .max(type_ir.max_ir.right.len());

    if let Some(ir) = type_ir.exact_min.as_ref() {
        max_len = max_len.max(ir.left.len().max(ir.right.len()));
    }
    if let Some(ir) = type_ir.exact_mid.as_ref() {
        max_len = max_len.max(ir.left.len().max(ir.right.len()));
    }

    max_len
}

pub(crate) fn max_partition_count(raw_types: &[TypeIrSet]) -> usize {
    let max_len = raw_types
        .iter()
        .map(max_type_len)
        .max()
        .unwrap_or(PARTITION_SIZE);

    let stretched_len =
        ((max_len as f64) * MAX_STRETCH as f64).ceil() as usize;

    ((stretched_len + PARTITION_SIZE - 1) / PARTITION_SIZE).max(1)
}

pub(crate) fn stretched_partition_count(
    type_ir: &TypeIrSet,
    stretch_percent: i32,
) -> usize {
    let max_len = max_type_len(type_ir);
    let stretch =
        stretch_percent.clamp(50, 200) as f64 / 100.0;
    let stretched_len =
        ((max_len as f64) * stretch).ceil() as usize;

    ((stretched_len + PARTITION_SIZE - 1) / PARTITION_SIZE).max(1)
}

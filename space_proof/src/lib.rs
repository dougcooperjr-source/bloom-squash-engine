use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;

mod decay_calibration;
mod engine;
mod resources;

use engine::{
    build_type_kernels, max_partition_count, spawn_kernel_worker,
    stretched_partition_count, BuildRequest, BuildResult, PartitionedConvolver,
    WetPredelay, PARTITION_SIZE,
};
use resources::{load_type_sets, BOOTH_INDEX};

const VST3_CLASS_ID_BYTES: [u8; 16] = *b"DB30SPACEPROOF01";
const MAX_PREDELAY_MS: i32 = 300;

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

        match load_type_sets(buffer_config.sample_rate) {
            Ok(raw_types) => {
                let max_num_parts = max_partition_count(&raw_types);
                let initial_num_parts =
                    stretched_partition_count(&raw_types[BOOTH_INDEX], 100);
                let initial_kernels = build_type_kernels(
                    BOOTH_INDEX,
                    &raw_types[BOOTH_INDEX],
                    100,
                    initial_num_parts,
                    buffer_config.sample_rate,
                );

                self.convolver = Some(PartitionedConvolver::new(
                    initial_kernels,
                    initial_num_parts,
                    max_num_parts,
                ));
                self.predelay =
                    WetPredelay::new(buffer_config.sample_rate, MAX_PREDELAY_MS);

                let (request_tx, request_rx) =
                    mpsc::sync_channel::<BuildRequest>(1);
                let (result_tx, result_rx) = mpsc::channel::<BuildResult>();
                spawn_kernel_worker(
                    raw_types,
                    buffer_config.sample_rate,
                    request_rx,
                    result_tx,
                );
                self.rebuild_tx = Some(request_tx);
                self.rebuild_rx = Some(result_rx);

                self.next_generation = 1;
                self.latest_requested_generation = 0;
                self.last_requested_type = 0;
                self.last_requested_stretch = 100;

                context.set_latency_samples(PARTITION_SIZE as u32);
                nih_log!(
                    "DB30 SPACE Proof v0.3 loaded 20 Types with Audio7 decay calibration"
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
        let predelay_ms = self
            .params
            .predelay_ms
            .value()
            .clamp(0, MAX_PREDELAY_MS);
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
            let (wet_l, wet_r) =
                convolver.process_sample(left[sample_idx], right[sample_idx], decay);
            let (out_l, out_r) = self.predelay.process_sample(wet_l, wet_r);
            left[sample_idx] = out_l;
            right[sample_idx] = out_r;
        }

        ProcessStatus::Normal
    }
}

impl Db30SpaceProof {
    fn request_rebuild_if_needed(&mut self, type_index: i32, stretch_percent: i32) {
        if type_index == self.last_requested_type
            && stretch_percent == self.last_requested_stretch
        {
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
                // Retry the newest host values on the next process call.
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
                                convolver.install_kernels(
                                    result.kernels,
                                    result.num_parts,
                                );
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

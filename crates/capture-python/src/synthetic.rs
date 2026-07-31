use cspa_facade::{
    AggregateManagerBudgets, BackendId, BoundaryKind, Cancellation, CaptureRequest, CaptureTarget,
    Channel, DitherMode, Failure, Format, Header, Layout, MixedBlockMetadata, MixerConfig, Mode,
    OnRootExit, OutputConverterConfig, Overflow, PartialReason, ParticipationState,
    ProcessIdentity, ProcessScope, Provenance, ReadError, ReadResult, SampleType, Slice, Stamp,
    Stop, StorageBudget, StorageLimits,
    core::{
        manager::{MultiInputManager, StorageReservation},
        mixer::{MixStepOutcome, MixerInputPacket},
        ordering,
        reads::ReadStream,
        schema::ByteOrder,
        storage::{CopyStats, Pool},
    },
};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering::SeqCst},
    },
    thread,
    time::{Duration, Instant},
};

pub struct SyntheticSegment {
    pub session_id: u64,
    pub segment_id: u64,
    pub boundary_before: Option<BoundaryKind>,
    pub stream: ReadStream,
    pub format: Arc<Format>,
    pub _reservation: StorageReservation,
}

/// Deterministic synthetic manager for segments, retention, waiting and failures.
pub struct SyntheticSegmentedManager {
    segments: VecDeque<SyntheticSegment>,
    pending_boundary: Option<(u64, u64, BoundaryKind)>,
    current_session: u64,
    current_segment: u64,
    waiting_forever: bool,
    terminal_failure: Option<Failure>,
    stopped: AtomicBool,
    aborted: AtomicBool,
    pub simulate_cleanup_timeout: AtomicBool,
}

impl SyntheticSegmentedManager {
    pub fn new_segmented_demo() -> Result<Self, String> {
        let limits = StorageLimits::new(8, 256, 8, 16384).map_err(|e| format!("{e:?}"))?;
        let budget = StorageBudget::new(2, 65536).map_err(|e| format!("{e:?}"))?;
        let format = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .map_err(|e| format!("{e:?}"))?,
        );

        let res1 = budget.reserve(limits, 4096).map_err(|e| format!("{e:?}"))?;
        let (mut prod1, read1) = ordering::channel(
            Pool::new_reserved(limits, 256, format.clone(), res1.retain())
                .map_err(|e| format!("{e:?}"))?,
            4,
        )
        .map_err(|e| format!("{e:?}"))?;
        let header = Header {
            sequence: 1,
            segment: 1,
            generation: 1,
            frames: 2,
            sample_position: Some(0),
            timestamp: None,
        };
        let mut pcm1 = Vec::with_capacity(16);
        for val in [0.25_f32, -0.25, 0.5, -0.5] {
            pcm1.extend_from_slice(&val.to_le_bytes());
        }
        prod1.offer(&pcm1, header, Overflow::Fail);
        prod1.close();
        drop(prod1);

        let res2 = budget.reserve(limits, 4096).map_err(|e| format!("{e:?}"))?;
        let (mut prod2, read2) = ordering::channel(
            Pool::new_reserved(limits, 256, format.clone(), res2.retain())
                .map_err(|e| format!("{e:?}"))?,
            4,
        )
        .map_err(|e| format!("{e:?}"))?;
        let mut pcm2 = Vec::with_capacity(16);
        for val in [0.75_f32, -0.75, 1.0, -1.0] {
            pcm2.extend_from_slice(&val.to_le_bytes());
        }
        prod2.offer(
            &pcm2,
            Header {
                sequence: 1,
                segment: 2,
                generation: 1,
                frames: 2,
                sample_position: Some(2),
                timestamp: None,
            },
            Overflow::Fail,
        );
        prod2.close();
        drop(prod2);

        let mut segments = VecDeque::with_capacity(2);
        segments.push_back(SyntheticSegment {
            session_id: 1,
            segment_id: 1,
            boundary_before: None,
            stream: ReadStream::new(read1),
            format: format.clone(),
            _reservation: res1,
        });
        segments.push_back(SyntheticSegment {
            session_id: 2,
            segment_id: 2,
            boundary_before: Some(BoundaryKind::ConfigurationChanged),
            stream: ReadStream::new(read2),
            format,
            _reservation: res2,
        });

        Ok(Self {
            segments,
            pending_boundary: None,
            current_session: 1,
            current_segment: 1,
            waiting_forever: false,
            terminal_failure: None,
            stopped: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            simulate_cleanup_timeout: AtomicBool::new(false),
        })
    }

    pub fn new_retention_demo() -> Result<Self, String> {
        let limits = StorageLimits::new(8, 256, 8, 16384).map_err(|e| format!("{e:?}"))?;
        let budget = StorageBudget::new(2, 65536).map_err(|e| format!("{e:?}"))?;
        let format = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .map_err(|e| format!("{e:?}"))?,
        );
        let res = budget.reserve(limits, 4096).map_err(|e| format!("{e:?}"))?;
        let (mut prod, read) = ordering::channel(
            Pool::new_reserved(limits, 256, format.clone(), res.retain())
                .map_err(|e| format!("{e:?}"))?,
            8,
        )
        .map_err(|e| format!("{e:?}"))?;
        for seq in 1..=8_u64 {
            let mut pcm = Vec::with_capacity(64);
            for ch in 0..16_u32 {
                let val = (seq as f32) + (ch as f32) * 0.01;
                pcm.extend_from_slice(&val.to_le_bytes());
            }
            prod.offer(
                &pcm,
                Header {
                    sequence: seq,
                    segment: 1,
                    generation: 1,
                    frames: 8,
                    sample_position: Some((seq - 1) * 8),
                    timestamp: None,
                },
                Overflow::Fail,
            );
        }
        prod.close();
        drop(prod);

        let mut segments = VecDeque::with_capacity(1);
        segments.push_back(SyntheticSegment {
            session_id: 1,
            segment_id: 1,
            boundary_before: None,
            stream: ReadStream::new(read),
            format,
            _reservation: res,
        });

        Ok(Self {
            segments,
            pending_boundary: None,
            current_session: 1,
            current_segment: 1,
            waiting_forever: false,
            terminal_failure: None,
            stopped: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            simulate_cleanup_timeout: AtomicBool::new(false),
        })
    }

    pub fn new_waiting_demo() -> Self {
        Self {
            segments: VecDeque::new(),
            pending_boundary: None,
            current_session: 1,
            current_segment: 1,
            waiting_forever: true,
            terminal_failure: None,
            stopped: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            simulate_cleanup_timeout: AtomicBool::new(false),
        }
    }

    pub fn new_failure_demo(failure: Failure) -> Self {
        Self {
            segments: VecDeque::new(),
            pending_boundary: None,
            current_session: 1,
            current_segment: 1,
            waiting_forever: false,
            terminal_failure: Some(failure),
            stopped: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            simulate_cleanup_timeout: AtomicBool::new(false),
        }
    }

    fn empty_result(reason: PartialReason) -> ReadResult {
        ReadResult {
            frames: 0,
            chunks: Vec::new(),
            first_packet: None,
            last_packet: None,
            reason,
            control: None,
            copies: CopyStats::default(),
        }
    }

    pub fn poll_read(
        &mut self,
        mut dst: Option<&mut [u8]>,
        frames: usize,
        timeout_ms: u64,
        cancel: Cancellation,
    ) -> Result<(u64, u64, Option<BoundaryKind>, ReadResult), String> {
        if frames == 0 {
            return Err(format!("{:?}", ReadError::InvalidRequest));
        }
        if cancel.is_cancelled() {
            return Ok((
                self.current_session,
                self.current_segment,
                None,
                Self::empty_result(PartialReason::Cancelled),
            ));
        }
        if self.aborted.load(SeqCst) {
            return Ok((
                self.current_session,
                self.current_segment,
                None,
                Self::empty_result(PartialReason::Eof),
            ));
        }
        if let Some(failure) = self.terminal_failure {
            return Ok((
                self.current_session,
                self.current_segment,
                None,
                Self::empty_result(PartialReason::Failure(failure)),
            ));
        }
        if self.waiting_forever {
            if timeout_ms == 0 {
                return Ok((
                    self.current_session,
                    self.current_segment,
                    None,
                    Self::empty_result(PartialReason::WouldBlock),
                ));
            }
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            loop {
                if cancel.is_cancelled() {
                    return Ok((
                        self.current_session,
                        self.current_segment,
                        None,
                        Self::empty_result(PartialReason::Cancelled),
                    ));
                }
                if self.aborted.load(SeqCst) || self.stopped.load(SeqCst) {
                    return Ok((
                        self.current_session,
                        self.current_segment,
                        None,
                        Self::empty_result(PartialReason::Eof),
                    ));
                }
                let now = Instant::now();
                if now >= deadline {
                    return Ok((
                        self.current_session,
                        self.current_segment,
                        None,
                        Self::empty_result(PartialReason::Timeout),
                    ));
                }
                thread::park_timeout((deadline - now).min(Duration::from_millis(2)));
            }
        }

        if let Some((sess, seg, bkind)) = self.pending_boundary.take() {
            self.current_session = sess;
            self.current_segment = seg;
            return Ok((
                sess,
                seg,
                Some(bkind),
                Self::empty_result(PartialReason::Boundary),
            ));
        }

        loop {
            let Some(front) = self.segments.front() else {
                return Ok((
                    self.current_session,
                    self.current_segment,
                    None,
                    Self::empty_result(PartialReason::Eof),
                ));
            };
            self.current_session = front.session_id;
            self.current_segment = front.segment_id;

            let deadline = (timeout_ms > 0).then(|| {
                Duration::from_millis(timeout_ms)
                    .as_nanos()
                    .min(u64::MAX as u128) as u64
            });
            let res = if let Some(buf) = dst.as_deref_mut() {
                let mut op = front
                    .stream
                    .begin_into(
                        buf,
                        front.format.clone(),
                        frames,
                        timeout_ms > 0,
                        deadline,
                        cancel.clone(),
                    )
                    .map_err(|e| format!("{e:?}"))?;
                let started = Instant::now();
                loop {
                    let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                    if let Some(r) = op.poll(elapsed) {
                        break r;
                    }
                    thread::park_timeout(Duration::from_millis(1));
                }
            } else {
                let mut op = front
                    .stream
                    .begin(
                        Mode::Variable,
                        frames,
                        timeout_ms > 0,
                        deadline,
                        cancel.clone(),
                    )
                    .map_err(|e| format!("{e:?}"))?;
                let started = Instant::now();
                loop {
                    let elapsed = started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                    if let Some(r) = op.poll(elapsed) {
                        break r;
                    }
                    thread::park_timeout(Duration::from_millis(1));
                }
            };

            if res.reason == PartialReason::Eof && res.frames == 0 {
                self.segments.pop_front();
                if let Some(next_seg) = self.segments.front() {
                    if let Some(bkind) = next_seg.boundary_before {
                        let sess = next_seg.session_id;
                        let seg = next_seg.segment_id;
                        self.current_session = sess;
                        self.current_segment = seg;
                        return Ok((
                            sess,
                            seg,
                            Some(bkind),
                            Self::empty_result(PartialReason::Boundary),
                        ));
                    }
                    continue;
                }
                return Ok((self.current_session, self.current_segment, None, res));
            }
            return Ok((self.current_session, self.current_segment, None, res));
        }
    }

    pub fn stop(&mut self, mode: Stop) {
        self.stopped.store(true, SeqCst);
        if mode == Stop::Abort {
            self.aborted.store(true, SeqCst);
            for seg in &self.segments {
                seg.stream.control_handle().stop(Stop::Abort);
            }
            self.segments.clear();
        } else {
            for seg in &self.segments {
                seg.stream.control_handle().stop(Stop::Drain);
            }
        }
    }

    pub fn wait_cleanup(&self) -> bool {
        !self.simulate_cleanup_timeout.load(SeqCst)
    }
}

pub type SyntheticMixOutcome = (
    u64,
    u64,
    Option<BoundaryKind>,
    Option<MixedBlockMetadata>,
    ReadResult,
);

pub struct PortableMultiState {
    pub manager: Arc<MultiInputManager>,
    pub out_pool: Mutex<Pool>,
    pub sequence: AtomicU64,
    pub stopped: AtomicBool,
    pub aborted: AtomicBool,
    pub queued_batches: Mutex<Vec<Vec<(usize, MixerInputPacket)>>>,
}

impl PortableMultiState {
    pub fn new_demo(
        sample: SampleType,
        clamp_f32: bool,
        silence_on_idle: bool,
        inject_failed_code: Option<u32>,
    ) -> Result<Arc<Self>, String> {
        let t1 = CaptureTarget::process(
            ProcessIdentity {
                pid: 501,
                generation: 5001,
            },
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .map_err(|e| format!("{e:?}"))?;
        let t2 = CaptureTarget::application(
            ProcessIdentity {
                pid: 502,
                generation: 5002,
            },
            Some("app.two".into()),
            Some("AppTwo".into()),
            Some("C:\\Apps\\two.exe".into()),
            ProcessScope::Tree,
            OnRootExit::Stop,
        )
        .map_err(|e| format!("{e:?}"))?;

        let native_format = Arc::new(
            Format::new(
                SampleType::F32,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .map_err(|e| format!("{e:?}"))?,
        );
        let mixer_cfg = MixerConfig {
            clamp_f32,
            silence_on_idle,
            ..MixerConfig::default()
        };

        let mut req = CaptureRequest::new(vec![t1, t2]).with_mixer_config(mixer_cfg);
        if sample != SampleType::F32 {
            let conv_fmt = Format::new(
                sample,
                ByteOrder::Little,
                48_000,
                2,
                Layout::Known(Arc::from([Channel::FrontLeft, Channel::FrontRight])),
            )
            .map_err(|e| format!("{e:?}"))?;
            req = req.with_output_converter(Some(OutputConverterConfig::new(
                conv_fmt,
                DitherMode::Tpdf,
            )));
        }
        req = req.with_budgets(AggregateManagerBudgets::default());

        let out_format = req.output_converter().map_or_else(
            || native_format.clone(),
            |c| Arc::new(c.target_format.clone()),
        );
        let default_bk = BackendId::Wasapi;
        let per_input = vec![
            (default_bk, native_format.clone()),
            (default_bk, native_format),
        ];
        let multi = MultiInputManager::new_heterogeneous(req, per_input, 480, 480)
            .map_err(|e| format!("{e:?}"))?;

        if let Some(code) = inject_failed_code {
            let _ = multi.set_input_state(1, ParticipationState::Failed(code));
        }

        let out_limits = StorageLimits::new(8, 480, out_format.bytes_per_frame(), 262_144)
            .map_err(|e| format!("{e:?}"))?;
        let out_res = multi
            .storage_budget()
            .reserve_auxiliary_bytes(out_limits, 16384)
            .map_err(|e| format!("{e:?}"))?;
        let out_pool = Mutex::new(
            Pool::new_reserved(out_limits, 480, out_format, out_res)
                .map_err(|e| format!("{e:?}"))?,
        );

        let mut batches = Vec::new();
        if !silence_on_idle {
            let mut s1 = vec![0.25_f32; 960];
            s1[0] = 0.75;
            s1[1] = -0.75;
            let mut s2 = vec![0.25_f32; 960];
            s2[0] = 0.75;
            s2[1] = -0.75;
            s2[2] = f32::NAN;
            s2[3] = f32::INFINITY;
            let stamp1 = Some(Stamp {
                ticks: 100_000,
                ticks_per_second: 10_000_000,
                clock_domain: 1,
                provenance: Provenance::Native,
                uncertainty_ns: Some(250_000),
            });
            let stamp2 = Some(Stamp {
                ticks: 120_000,
                ticks_per_second: 10_000_000,
                clock_domain: 1,
                provenance: Provenance::Native,
                uncertainty_ns: Some(250_000),
            });
            let mut batch = vec![(
                0,
                MixerInputPacket {
                    samples: s1,
                    frames: 480,
                    timestamp: stamp1,
                    arrival_ns: 1_000_000,
                },
            )];
            if inject_failed_code.is_none() {
                batch.push((
                    1,
                    MixerInputPacket {
                        samples: s2,
                        frames: 480,
                        timestamp: stamp2,
                        arrival_ns: 1_000_000,
                    },
                ));
            }
            batches.push(batch);
        }

        Ok(Arc::new(Self {
            manager: Arc::new(multi),
            out_pool,
            sequence: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            queued_batches: Mutex::new(batches),
        }))
    }

    pub fn poll_read(
        &self,
        dst: Option<&mut [u8]>,
        frames: usize,
        cancel: Cancellation,
    ) -> Result<SyntheticMixOutcome, String> {
        if cancel.is_cancelled() {
            return Ok((
                1,
                1,
                None,
                None,
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Cancelled,
                    control: None,
                    copies: CopyStats::default(),
                },
            ));
        }
        if self.aborted.load(SeqCst) {
            return Ok((
                1,
                1,
                None,
                None,
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Eof,
                    control: None,
                    copies: CopyStats::default(),
                },
            ));
        }
        {
            let mut batches = self
                .queued_batches
                .lock()
                .map_err(|_| "batches lock poisoned")?;
            if !batches.is_empty() {
                let batch = batches.remove(0);
                for (idx, pkt) in batch {
                    let _ = self.manager.push_input_packet(idx, pkt);
                }
            } else if self.stopped.load(SeqCst) {
                for idx in 0..self.manager.input_handles().len() {
                    let _ = self
                        .manager
                        .set_input_state(idx, ParticipationState::Exited);
                }
            }
        }
        let mut out_bytes = Vec::new();
        let chunk_frames = frames.clamp(1, 480);
        let (outcome, meta) = self
            .manager
            .poll_mix_bytes(chunk_frames, 25_000_000, 25_000_000, &mut out_bytes)
            .map_err(|e| format!("{e:?}"))?;
        match outcome {
            MixStepOutcome::Produced(out_frames) => {
                if let Some(buf) = dst {
                    let n = out_bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&out_bytes[..n]);
                    return Ok((
                        1,
                        meta.membership_version,
                        None,
                        Some(meta),
                        ReadResult {
                            frames: out_frames,
                            chunks: Vec::new(),
                            first_packet: None,
                            last_packet: None,
                            reason: PartialReason::Complete,
                            control: None,
                            copies: CopyStats {
                                bytes: n as u64,
                                destinations: 1,
                                ..Default::default()
                            },
                        },
                    ));
                }
                let seq = self.sequence.fetch_add(1, SeqCst) + 1;
                let hdr = Header {
                    sequence: seq,
                    segment: meta.membership_version,
                    generation: 1,
                    frames: out_frames,
                    sample_position: Some((seq - 1) * (out_frames as u64)),
                    timestamp: None,
                };
                let block = self
                    .out_pool
                    .lock()
                    .map_err(|_| "out_pool lock poisoned")?
                    .offer(&out_bytes, hdr)
                    .map_err(|e| format!("pool offer failed: {e:?}"))?;
                let slice = Slice {
                    block,
                    first: 0,
                    frames: out_frames,
                };
                Ok((
                    1,
                    meta.membership_version,
                    None,
                    Some(meta),
                    ReadResult {
                        frames: out_frames,
                        chunks: vec![slice],
                        first_packet: None,
                        last_packet: None,
                        reason: PartialReason::Complete,
                        control: None,
                        copies: CopyStats {
                            bytes: out_bytes.len() as u64,
                            destinations: 1,
                            ..Default::default()
                        },
                    },
                ))
            }
            MixStepOutcome::ParkedIdle => Ok((
                1,
                meta.membership_version,
                None,
                Some(meta),
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Timeout,
                    control: None,
                    copies: CopyStats::default(),
                },
            )),
            MixStepOutcome::Eof => Ok((
                1,
                meta.membership_version,
                None,
                Some(meta),
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Eof,
                    control: None,
                    copies: CopyStats::default(),
                },
            )),
            MixStepOutcome::Failed(_) => Ok((
                1,
                meta.membership_version,
                None,
                Some(meta),
                ReadResult {
                    frames: 0,
                    chunks: Vec::new(),
                    first_packet: None,
                    last_packet: None,
                    reason: PartialReason::Failure(Failure::Backend),
                    control: None,
                    copies: CopyStats::default(),
                },
            )),
        }
    }

    pub fn stop(&self, mode: Stop) {
        self.stopped.store(true, SeqCst);
        if mode == Stop::Abort {
            self.aborted.store(true, SeqCst);
            if let Ok(mut b) = self.queued_batches.lock() {
                b.clear();
            }
        }
    }
}

use super::*;

#[cfg(test)]
static PANIC_DISK_WRITER_ON_NEXT_JOB: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(super) fn panic_disk_writer_on_next_job() {
    PANIC_DISK_WRITER_ON_NEXT_JOB.store(true, Ordering::Release);
}

#[derive(Debug, Clone)]
pub(super) struct DiskSegmentMeta {
    pub(super) index: u64,
    pub(super) run_index: u64,
    pub(super) source_start_ns: u64,
    pub(super) source_end_ns: u64,
    pub(super) mp4_path: PathBuf,
    pub(super) sidecar_path: PathBuf,
    pub(super) duration_90k: u64,
    pub(super) audio_access_units: usize,
    pub(super) bytes: u64,
    pub(super) lease: Arc<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct DiskSaveCursor {
    pub(super) run_index: u64,
    pub(super) source_pts_ns: u64,
}

impl DiskSegmentMeta {
    pub(super) fn duration(&self) -> Duration {
        Duration::from_nanos(scale_90k_to_ns(self.duration_90k))
    }

    fn start_cursor(&self) -> DiskSaveCursor {
        DiskSaveCursor {
            run_index: self.run_index,
            source_pts_ns: self.source_start_ns,
        }
    }

    fn end_cursor(&self) -> DiskSaveCursor {
        DiskSaveCursor {
            run_index: self.run_index,
            source_pts_ns: self.source_end_ns,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct DiskSegmentReservation {
    pub(super) index: u64,
    pub(super) mp4_path: PathBuf,
    pub(super) sidecar_path: PathBuf,
    pub(super) mp4_part_path: PathBuf,
    pub(super) sidecar_part_path: PathBuf,
}

pub(super) struct DiskSegmentWriteTransaction {
    reservation: DiskSegmentReservation,
    committed: bool,
}

impl DiskSegmentWriteTransaction {
    pub(super) fn new(reservation: DiskSegmentReservation) -> Self {
        Self {
            reservation,
            committed: false,
        }
    }

    pub(super) fn publish(&self) -> Result<(), BackendError> {
        fs::rename(&self.reservation.mp4_part_path, &self.reservation.mp4_path)
            .map_err(|err| BackendError::Io(err.to_string()))?;
        fs::rename(
            &self.reservation.sidecar_part_path,
            &self.reservation.sidecar_path,
        )
        .map_err(|err| BackendError::Io(err.to_string()))
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for DiskSegmentWriteTransaction {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in [
            &self.reservation.mp4_part_path,
            &self.reservation.sidecar_part_path,
            &self.reservation.mp4_path,
            &self.reservation.sidecar_path,
        ] {
            let _ = fs::remove_file(path);
        }
    }
}

#[derive(Debug)]
pub(super) struct DiskReplayStore {
    pub(super) dir: PathBuf,
    pub(super) retention: Duration,
    pub(super) segment_slop: Duration,
    pub(super) segments: VecDeque<DiskSegmentMeta>,
    pub(super) next_index: u64,
}

impl DiskReplayStore {
    pub(super) fn prepare_directory(dir: &Path) -> Result<(), BackendError> {
        fs::create_dir_all(dir).map_err(|err| BackendError::Io(err.to_string()))?;
        for entry in fs::read_dir(dir).map_err(|err| BackendError::Io(err.to_string()))? {
            let entry = entry.map_err(|err| BackendError::Io(err.to_string()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("rustreplay_segment_")
                && (name.ends_with(".mp4") || name.ends_with(".rrseg") || name.ends_with(".part"))
            {
                fs::remove_file(entry.path()).map_err(|err| {
                    BackendError::Io(format!(
                        "清理残留磁盘循环文件 {} 失败：{err}",
                        entry.path().display()
                    ))
                })?;
            }
        }
        Ok(())
    }

    pub(super) fn new(dir: PathBuf, retention: Duration, segment_slop: Duration) -> Self {
        Self {
            dir,
            retention,
            segment_slop,
            segments: VecDeque::new(),
            next_index: 0,
        }
    }

    pub(super) fn has_saveable_segments(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| segment.audio_access_units > 0)
    }

    pub(super) fn reserve_segment_paths(&mut self) -> Result<DiskSegmentReservation, BackendError> {
        fs::create_dir_all(&self.dir).map_err(|err| BackendError::Io(err.to_string()))?;
        let index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        let stem = format!("rustreplay_segment_{}_{}", timestamp_for_filename(), index);
        Ok(DiskSegmentReservation {
            index,
            mp4_path: self.dir.join(format!("{stem}.mp4")),
            sidecar_path: self.dir.join(format!("{stem}.rrseg")),
            mp4_part_path: self.dir.join(format!("{stem}.mp4.part")),
            sidecar_part_path: self.dir.join(format!("{stem}.rrseg.part")),
        })
    }

    pub(super) fn commit_segment(&mut self, meta: DiskSegmentMeta) -> Vec<String> {
        self.segments.push_back(meta);
        self.prune_old_segments()
    }

    pub(super) fn snapshot_recent_tracks(
        &self,
        duration: Duration,
    ) -> Result<Option<DiskPreparedReplaySnapshot>, BackendError> {
        self.snapshot_recent_tracks_after(duration, None)
            .map(|snapshot| snapshot.map(|(tracks, _)| tracks))
    }

    pub(super) fn snapshot_recent_tracks_after(
        &self,
        duration: Duration,
        not_before: Option<DiskSaveCursor>,
    ) -> Result<Option<(DiskPreparedReplaySnapshot, DiskSaveCursor)>, BackendError> {
        let selected = self.select_recent_segments_after(duration, not_before);
        if selected.is_empty() {
            return Ok(None);
        }
        let last_cursor = selected
            .last()
            .map(DiskSegmentMeta::end_cursor)
            .expect("selected segments are non-empty");
        let mut segments = Vec::with_capacity(selected.len());
        for meta in selected {
            segments.push(DiskSegmentIndexedTracks {
                mp4_path: meta.mp4_path.clone(),
                index: read_disk_segment_sidecar(&meta.sidecar_path)?,
                lease: meta.lease.clone(),
            });
        }
        let latest = segments.last().expect("selected segments are non-empty");
        validate_disk_segment_compatibility(latest, latest)?;
        let mut epoch_start = segments.len() - 1;
        while epoch_start > 0
            && validate_disk_segment_compatibility(latest, &segments[epoch_start - 1]).is_ok()
        {
            epoch_start -= 1;
        }
        Ok(concat_disk_indexed_segments(&segments[epoch_start..])?
            .map(|tracks| (tracks, last_cursor)))
    }

    pub(super) fn select_recent_segments(&self, duration: Duration) -> Vec<DiskSegmentMeta> {
        self.select_recent_segments_after(duration, None)
    }

    pub(super) fn select_recent_segments_after(
        &self,
        duration: Duration,
        not_before: Option<DiskSaveCursor>,
    ) -> Vec<DiskSegmentMeta> {
        let target_ns = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut selected = VecDeque::new();
        let mut accumulated_ns = 0u64;
        for segment in self
            .segments
            .iter()
            .rev()
            .filter(|segment| not_before.is_none_or(|cursor| segment.start_cursor() >= cursor))
        {
            selected.push_front(segment.clone());
            accumulated_ns = accumulated_ns.saturating_add(scale_90k_to_ns(segment.duration_90k));
            if accumulated_ns >= target_ns {
                break;
            }
        }
        selected.into_iter().collect()
    }

    pub(super) fn prune_old_segments(&mut self) -> Vec<String> {
        let keep_for = self.retention.saturating_add(self.segment_slop);
        let keep_ns = keep_for.as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut accumulated_ns = 0u64;
        let mut keep_from = self.segments.len();
        for (idx, segment) in self.segments.iter().enumerate().rev() {
            accumulated_ns = accumulated_ns.saturating_add(scale_90k_to_ns(segment.duration_90k));
            keep_from = idx;
            if accumulated_ns >= keep_ns {
                break;
            }
        }
        let mut retained = VecDeque::with_capacity(self.segments.len());
        let mut failures = Vec::new();
        for (index, segment) in self.segments.drain(..).enumerate() {
            if index < keep_from && Arc::strong_count(&segment.lease) == 1 {
                let mut deleted = true;
                for path in [&segment.mp4_path, &segment.sidecar_path] {
                    match fs::remove_file(path) {
                        Ok(()) => {}
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                        Err(err) => {
                            deleted = false;
                            failures.push(format!("{}: {err}", path.display()));
                        }
                    }
                }
                if !deleted {
                    retained.push_back(segment);
                }
            } else {
                retained.push_back(segment);
            }
        }
        self.segments = retained;
        failures
    }

    pub(super) fn clear_segments(&mut self) -> Result<(), BackendError> {
        let segments = self.segments.drain(..).collect::<Vec<_>>();
        let mut retained = VecDeque::new();
        let mut failures = Vec::new();
        for segment in segments {
            if Arc::strong_count(&segment.lease) > 1 {
                retained.push_back(segment);
                continue;
            }
            let mut deleted = true;
            for path in [&segment.mp4_path, &segment.sidecar_path] {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        deleted = false;
                        failures.push(format!("{}: {err}", path.display()));
                    }
                }
            }
            if !deleted {
                retained.push_back(segment);
            }
        }
        self.segments = retained;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(BackendError::Io(format!(
                "删除磁盘循环缓存失败，已保留元数据供后续重试：{}",
                failures.join(" | ")
            )))
        }
    }
}

pub(super) struct DiskSegmentWriteJob {
    pub(super) run_index: u64,
    pub(super) segment: DiskSegmentTracks,
    pub(super) enqueued_at: Instant,
}

pub(super) struct DiskSegmentWriteReport {
    pub(super) meta: DiskSegmentMeta,
    pub(super) queue_wait: Duration,
    pub(super) mux_write: Duration,
    pub(super) sidecar_write: Duration,
    pub(super) commit: Duration,
    pub(super) total: Duration,
    pub(super) cleanup_warnings: Vec<String>,
}

pub(super) struct DiskSegmentWriter {
    pub(super) sender: Option<SyncSender<DiskSegmentWriteJob>>,
    pub(super) handle: Option<JoinHandle<()>>,
    pub(super) shutdown: Arc<AtomicBool>,
    pub(super) failure: Arc<Mutex<Option<String>>>,
}

impl DiskSegmentWriter {
    pub(super) fn spawn(
        store: Arc<Mutex<DiskReplayStore>>,
        tx: Sender<ReplayEvent>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        let (sender, rx) = mpsc::sync_channel::<DiskSegmentWriteJob>(DISK_WRITER_QUEUE_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let thread_shutdown = shutdown.clone();
        let thread_failure = failure.clone();
        let handle = thread::spawn(move || {
            lower_current_disk_writer_priority();
            let writer_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                while let Ok(job) = rx.recv() {
                    if thread_shutdown.load(Ordering::Relaxed) {
                        break;
                    }
                    let run_index = job.run_index;
                    match write_disk_segment_job(&store, job) {
                        Ok(report) => {
                            let _ = tx.send(ReplayEvent::BackendStatus {
                                index: run_index,
                                message: format!(
                                    "磁盘循环分段已异步写入 #{}：{}，duration={:.3}s，audio_au={}，bytes={}，queue={:.1}ms mux={:.1}ms sidecar={:.1}ms commit={:.1}ms total={:.1}ms",
                                    report.meta.index,
                                    report.meta.mp4_path.display(),
                                    report.meta.duration().as_secs_f64(),
                                    report.meta.audio_access_units,
                                    report.meta.bytes,
                                    report.queue_wait.as_secs_f64() * 1000.0,
                                    report.mux_write.as_secs_f64() * 1000.0,
                                    report.sidecar_write.as_secs_f64() * 1000.0,
                                    report.commit.as_secs_f64() * 1000.0,
                                    report.total.as_secs_f64() * 1000.0,
                                ),
                            });
                            if !report.cleanup_warnings.is_empty() {
                                let _ = tx.send(ReplayEvent::BackendStatus {
                                    index: run_index,
                                    message: format!(
                                        "磁盘循环旧分段删除失败，已保留元数据供后续重试：{}",
                                        report.cleanup_warnings.join(" | ")
                                    ),
                                });
                            }
                        }
                        Err(err) => {
                            let message =
                                format!("磁盘循环分段异步写入失败（片段 #{run_index}）：{err}");
                            if let Ok(mut failure) = thread_failure.lock() {
                                *failure = Some(message.clone());
                            }
                            stop.store(true, Ordering::Relaxed);
                            let _ = tx.send(ReplayEvent::BackendStatus {
                                index: run_index,
                                message,
                            });
                            break;
                        }
                    }
                }
            }));
            if writer_result.is_err() {
                let message = "磁盘循环异步 writer 发生 panic，已终止当前录制会话".to_owned();
                if let Ok(mut failure) = thread_failure.lock() {
                    *failure = Some(message.clone());
                }
                stop.store(true, Ordering::Relaxed);
                let _ = tx.send(ReplayEvent::BackendStatus { index: 0, message });
            }
        });
        Self {
            sender: Some(sender),
            handle: Some(handle),
            shutdown,
            failure,
        }
    }

    pub(super) fn sender(&self) -> Option<SyncSender<DiskSegmentWriteJob>> {
        self.sender.as_ref().cloned()
    }

    pub(super) fn shutdown(mut self) -> Option<String> {
        self.shutdown.store(true, Ordering::Relaxed);
        self.sender.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        match self.failure.lock() {
            Ok(mut failure) => failure.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }
}

impl Drop for DiskSegmentWriter {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.sender.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(super) fn write_disk_segment_job(
    store: &Arc<Mutex<DiskReplayStore>>,
    job: DiskSegmentWriteJob,
) -> Result<DiskSegmentWriteReport, BackendError> {
    #[cfg(test)]
    if PANIC_DISK_WRITER_ON_NEXT_JOB.swap(false, Ordering::AcqRel) {
        panic!("injected disk writer panic");
    }
    let started = Instant::now();
    let queue_wait = job.enqueued_at.elapsed();
    let reservation = store
        .lock()
        .map_err(|_| BackendError::Io("磁盘循环缓存锁已中毒".to_owned()))?
        .reserve_segment_paths()?;
    let mut transaction = DiskSegmentWriteTransaction::new(reservation);

    let mux_started = Instant::now();
    let index = super::mp4_mux::write_hevc_aac_mp4_with_index(
        &transaction.reservation.mp4_part_path,
        &job.segment.video_track,
        job.segment.audio_track.as_ref(),
    )?;
    let mux_write = mux_started.elapsed();

    let sidecar_started = Instant::now();
    write_disk_segment_sidecar(&transaction.reservation.sidecar_part_path, &index)?;
    let sidecar_write = sidecar_started.elapsed();
    transaction.publish()?;

    let bytes = fs::metadata(&transaction.reservation.mp4_path)
        .map(|meta| meta.len())
        .unwrap_or(0)
        + fs::metadata(&transaction.reservation.sidecar_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
    let meta = DiskSegmentMeta {
        index: transaction.reservation.index,
        run_index: job.run_index,
        source_start_ns: scale_90k_to_ns(job.segment.source_start_90k),
        source_end_ns: scale_90k_to_ns(job.segment.source_end_90k),
        mp4_path: transaction.reservation.mp4_path.clone(),
        sidecar_path: transaction.reservation.sidecar_path.clone(),
        duration_90k: index.video_track.duration_90k,
        audio_access_units: index
            .audio_track
            .as_ref()
            .map(|track| track.samples.len())
            .unwrap_or(0),
        bytes,
        lease: Arc::new(()),
    };

    let commit_started = Instant::now();
    let cleanup_warnings = store
        .lock()
        .map_err(|_| BackendError::Io("磁盘循环缓存锁已中毒".to_owned()))?
        .commit_segment(meta.clone());
    transaction.commit();
    let commit = commit_started.elapsed();
    Ok(DiskSegmentWriteReport {
        meta,
        queue_wait,
        mux_write,
        sidecar_write,
        commit,
        total: started.elapsed(),
        cleanup_warnings,
    })
}

#[cfg(windows)]
pub(super) fn lower_current_disk_writer_priority() {
    unsafe {
        let _ = windows::Win32::System::Threading::SetThreadPriority(
            windows::Win32::System::Threading::GetCurrentThread(),
            windows::Win32::System::Threading::THREAD_PRIORITY(-1),
        );
    }
}

#[cfg(not(windows))]
pub(super) fn lower_current_disk_writer_priority() {}

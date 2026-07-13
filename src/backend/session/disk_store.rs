use super::*;

#[derive(Debug, Clone)]
pub(super) struct DiskSegmentMeta {
    pub(super) index: u64,
    pub(super) mp4_path: PathBuf,
    pub(super) sidecar_path: PathBuf,
    pub(super) duration_90k: u64,
    pub(super) audio_access_units: usize,
    pub(super) bytes: u64,
    pub(super) lease: Arc<()>,
}

impl DiskSegmentMeta {
    pub(super) fn duration(&self) -> Duration {
        Duration::from_nanos(scale_90k_to_ns(self.duration_90k))
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

struct DiskSegmentWriteTransaction {
    reservation: DiskSegmentReservation,
    committed: bool,
}

impl DiskSegmentWriteTransaction {
    fn new(reservation: DiskSegmentReservation) -> Self {
        Self {
            reservation,
            committed: false,
        }
    }

    fn publish(&self) -> Result<(), BackendError> {
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

    pub(super) fn commit_segment(&mut self, meta: DiskSegmentMeta) {
        self.segments.push_back(meta);
        self.prune_old_segments();
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
        after_segment: Option<u64>,
    ) -> Result<Option<(DiskPreparedReplaySnapshot, u64)>, BackendError> {
        let selected = self.select_recent_segments_after(duration, after_segment);
        if selected.is_empty() {
            return Ok(None);
        }
        let last_segment = selected.last().map(|segment| segment.index).unwrap_or(0);
        let mut segments = Vec::with_capacity(selected.len());
        for meta in selected {
            segments.push(DiskSegmentIndexedTracks {
                mp4_path: meta.mp4_path.clone(),
                index: read_disk_segment_sidecar(&meta.sidecar_path)?,
                lease: meta.lease.clone(),
            });
        }
        Ok(concat_disk_indexed_segments(&segments)?.map(|tracks| (tracks, last_segment)))
    }

    pub(super) fn select_recent_segments(&self, duration: Duration) -> Vec<DiskSegmentMeta> {
        self.select_recent_segments_after(duration, None)
    }

    pub(super) fn select_recent_segments_after(
        &self,
        duration: Duration,
        after_segment: Option<u64>,
    ) -> Vec<DiskSegmentMeta> {
        let target_ns = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let mut selected = VecDeque::new();
        let mut accumulated_ns = 0u64;
        for segment in self
            .segments
            .iter()
            .rev()
            .filter(|segment| after_segment.is_none_or(|after| segment.index > after))
        {
            selected.push_front(segment.clone());
            accumulated_ns = accumulated_ns.saturating_add(scale_90k_to_ns(segment.duration_90k));
            if accumulated_ns >= target_ns {
                break;
            }
        }
        selected.into_iter().collect()
    }

    pub(super) fn prune_old_segments(&mut self) {
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
        for (index, segment) in self.segments.drain(..).enumerate() {
            if index < keep_from && Arc::strong_count(&segment.lease) == 1 {
                let _ = fs::remove_file(&segment.mp4_path);
                let _ = fs::remove_file(&segment.sidecar_path);
            } else {
                retained.push_back(segment);
            }
        }
        self.segments = retained;
    }

    pub(super) fn clear_segments(&mut self) -> Result<(), BackendError> {
        let segments = self.segments.drain(..).collect::<Vec<_>>();
        for segment in segments {
            for path in [&segment.mp4_path, &segment.sidecar_path] {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => {
                        return Err(BackendError::Io(format!(
                            "删除磁盘循环缓存 {} 失败：{err}",
                            path.display()
                        )));
                    }
                }
            }
        }
        Ok(())
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
}

pub(super) struct DiskSegmentWriter {
    pub(super) sender: Option<SyncSender<DiskSegmentWriteJob>>,
    pub(super) handle: Option<JoinHandle<()>>,
}

impl DiskSegmentWriter {
    pub(super) fn spawn(
        store: Arc<Mutex<DiskReplayStore>>,
        tx: Sender<ReplayEvent>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        let (sender, rx) = mpsc::sync_channel::<DiskSegmentWriteJob>(DISK_WRITER_QUEUE_CAPACITY);
        let handle = thread::spawn(move || {
            lower_current_disk_writer_priority();
            while let Ok(job) = rx.recv() {
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
                    }
                    Err(err) => {
                        stop.store(true, Ordering::Relaxed);
                        let _ = tx.send(ReplayEvent::Error(format!(
                            "磁盘循环分段异步写入失败（片段 #{run_index}）：{err}"
                        )));
                        break;
                    }
                }
            }
        });
        Self {
            sender: Some(sender),
            handle: Some(handle),
        }
    }

    pub(super) fn sender(&self) -> Option<SyncSender<DiskSegmentWriteJob>> {
        self.sender.as_ref().cloned()
    }

    pub(super) fn shutdown(mut self) {
        self.sender.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DiskSegmentWriter {
    fn drop(&mut self) {
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
    store
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

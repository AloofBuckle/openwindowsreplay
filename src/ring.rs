#![allow(dead_code)]
//! 已编码码流环形缓存。
//!
//! 文档要求裸 HEVC/AAC 码流在内存中循环保存；此模块只保存“已编码字节”。
//! 这些字节不属于 raw frame surface，因此不违反 GPU-only 的 raw frame 路径约束。

use crate::backend::mp4_mux::{
    AacAccessUnit, AacLcMp4Track, HevcAccessUnit, HevcCodecMetadata, HevcMp4Track,
    NclxColorMetadata,
};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodedStreamKind {
    Video,
    Audio,
}

impl EncodedStreamKind {
    const fn sort_key(self) -> u8 {
        match self {
            Self::Video => 0,
            Self::Audio => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub stream: EncodedStreamKind,
    pub pts_ns: u64,
    pub dts_ns: u64,
    pub duration_ns: u64,
    pub is_key: bool,
    pub data: Arc<[u8]>,
}

impl EncodedPacket {
    const fn stream_sort_key(&self) -> u8 {
        self.stream.sort_key()
    }
}

#[derive(Debug, Clone)]
pub struct EncodedRingBuffer {
    retention_ns: u64,
    video_packets: VecDeque<EncodedPacket>,
    audio_packets: VecDeque<EncodedPacket>,
    bytes: usize,
    video_key_packets: usize,
    newest_pts_ns: u64,
    newest_end_ns: u64,
}

impl EncodedRingBuffer {
    pub fn new(retention: Duration) -> Self {
        Self {
            retention_ns: retention.as_nanos().min(u128::from(u64::MAX)) as u64,
            video_packets: VecDeque::new(),
            audio_packets: VecDeque::new(),
            bytes: 0,
            video_key_packets: 0,
            newest_pts_ns: 0,
            newest_end_ns: 0,
        }
    }

    pub fn push(&mut self, packet: EncodedPacket) {
        self.bytes += packet.data.len();
        if packet.stream == EncodedStreamKind::Video && packet.is_key {
            self.video_key_packets = self.video_key_packets.saturating_add(1);
        }
        self.newest_pts_ns = self.newest_pts_ns.max(packet.pts_ns);
        self.newest_end_ns = self
            .newest_end_ns
            .max(packet.pts_ns.saturating_add(packet.duration_ns));
        let queue = match packet.stream {
            EncodedStreamKind::Video => &mut self.video_packets,
            EncodedStreamKind::Audio => &mut self.audio_packets,
        };
        insert_packet_ordered(queue, packet);
        self.prune_before(self.newest_end_ns.saturating_sub(self.retention_ns));
    }

    pub fn snapshot_recent(&self, duration: Duration) -> Vec<EncodedPacket> {
        if self.is_empty() {
            return Vec::new();
        }
        let duration_ns = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let window_start = self.newest_end_ns.saturating_sub(duration_ns);
        self.snapshot_overlapping(window_start)
    }

    pub fn snapshot_recent_with_leading_video_key(&self, duration: Duration) -> Vec<EncodedPacket> {
        self.snapshot_recent_with_leading_video_key_after(duration, None)
    }

    fn snapshot_recent_with_leading_video_key_after(
        &self,
        duration: Duration,
        not_before_pts_ns: Option<u64>,
    ) -> Vec<EncodedPacket> {
        if self.video_packets.is_empty() {
            return Vec::new();
        }
        let duration_ns = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let min_pts = self.newest_end_ns.saturating_sub(duration_ns);
        let start_pts = if let Some(not_before) = not_before_pts_ns {
            let window_start = min_pts.max(not_before);
            let Some(key) = self
                .video_packets
                .iter()
                .find(|packet| packet.is_key && packet.pts_ns >= window_start)
            else {
                return Vec::new();
            };
            key.pts_ns
        } else {
            self.video_packets
                .iter()
                .rev()
                .find(|packet| packet.is_key && packet.pts_ns <= min_pts)
                .map(|packet| packet.pts_ns)
                .or_else(|| {
                    self.video_packets
                        .iter()
                        .find(|packet| packet.is_key && packet.pts_ns >= min_pts)
                        .map(|packet| packet.pts_ns)
                })
                .unwrap_or(u64::MAX)
        };
        if start_pts == u64::MAX {
            Vec::new()
        } else {
            self.snapshot_from_pts(start_pts)
        }
    }

    pub fn newest_end_ns(&self) -> u64 {
        self.newest_end_ns
    }

    pub fn snapshot_recent_stream(
        &self,
        stream: EncodedStreamKind,
        duration: Duration,
    ) -> Vec<EncodedPacket> {
        self.snapshot_recent(duration)
            .into_iter()
            .filter(|packet| packet.stream == stream)
            .collect()
    }

    pub fn len(&self) -> usize {
        self.video_packets.len() + self.audio_packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.video_packets.is_empty() && self.audio_packets.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn prune_before(&mut self, min_pts: u64) {
        prune_queue_before(
            &mut self.video_packets,
            min_pts,
            &mut self.bytes,
            Some(&mut self.video_key_packets),
        );
        prune_queue_before(&mut self.audio_packets, min_pts, &mut self.bytes, None);
    }

    fn snapshot_from_pts(&self, min_pts: u64) -> Vec<EncodedPacket> {
        let video = self
            .video_packets
            .iter()
            .filter(|packet| packet.pts_ns >= min_pts);
        let audio = self
            .audio_packets
            .iter()
            .filter(|packet| packet.pts_ns >= min_pts);
        merge_packet_iterators(video, audio)
    }

    fn snapshot_overlapping(&self, window_start: u64) -> Vec<EncodedPacket> {
        let video = self
            .video_packets
            .iter()
            .filter(|packet| packet_end_ns(packet) > window_start);
        let audio = self
            .audio_packets
            .iter()
            .filter(|packet| packet_end_ns(packet) > window_start);
        merge_packet_iterators(video, audio)
    }

    fn clear(&mut self) {
        self.video_packets.clear();
        self.audio_packets.clear();
        self.bytes = 0;
        self.video_key_packets = 0;
        self.newest_pts_ns = 0;
        self.newest_end_ns = 0;
    }
}

fn insert_packet_ordered(queue: &mut VecDeque<EncodedPacket>, packet: EncodedPacket) {
    if queue
        .back()
        .is_none_or(|last| (last.pts_ns, last.dts_ns) <= (packet.pts_ns, packet.dts_ns))
    {
        queue.push_back(packet);
        return;
    }
    let insert_at = queue
        .iter()
        .position(|current| (current.pts_ns, current.dts_ns) > (packet.pts_ns, packet.dts_ns))
        .unwrap_or(queue.len());
    queue.insert(insert_at, packet);
}

fn prune_queue_before(
    queue: &mut VecDeque<EncodedPacket>,
    min_pts: u64,
    bytes: &mut usize,
    mut video_key_packets: Option<&mut usize>,
) {
    while queue
        .front()
        .is_some_and(|packet| packet_end_ns(packet) <= min_pts)
    {
        if let Some(packet) = queue.pop_front() {
            *bytes = bytes.saturating_sub(packet.data.len());
            if packet.is_key
                && let Some(count) = video_key_packets.as_deref_mut()
            {
                *count = count.saturating_sub(1);
            }
        }
    }
}

fn packet_end_ns(packet: &EncodedPacket) -> u64 {
    packet.pts_ns.saturating_add(packet.duration_ns)
}

fn merge_packet_iterators<'a, V, A>(video: V, audio: A) -> Vec<EncodedPacket>
where
    V: Iterator<Item = &'a EncodedPacket>,
    A: Iterator<Item = &'a EncodedPacket>,
{
    let mut video = video.peekable();
    let mut audio = audio.peekable();
    let mut packets = Vec::with_capacity(video.size_hint().0 + audio.size_hint().0);
    loop {
        let take_video = match (video.peek(), audio.peek()) {
            (Some(video), Some(audio)) => {
                (video.pts_ns, video.stream_sort_key()) <= (audio.pts_ns, audio.stream_sort_key())
            }
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        packets.push(if take_video {
            video.next().expect("peeked video packet").clone()
        } else {
            audio.next().expect("peeked audio packet").clone()
        });
    }
    packets
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedReplayMetadata {
    pub width: u16,
    pub height: u16,
    pub color: NclxColorMetadata,
    pub codec: HevcCodecMetadata,
    pub audio_sample_rate: u32,
    pub audio_channel_count: u16,
}

#[derive(Debug, Clone)]
pub struct EncodedReplaySnapshot {
    pub video_track: HevcMp4Track,
    pub audio_track: Option<AacLcMp4Track>,
}

#[derive(Debug, Clone)]
pub struct EncodedReplayPacketSnapshot {
    codec_epoch: u64,
    metadata: EncodedReplayMetadata,
    header_units: Vec<HevcAccessUnit>,
    packets: Vec<EncodedPacket>,
    first_video_pts_ns: u64,
    video_end_pts_ns: u64,
    save_cursor_pts_ns: u64,
}

impl EncodedReplayPacketSnapshot {
    pub fn codec_epoch(&self) -> u64 {
        self.codec_epoch
    }

    pub fn video_end_pts_ns(&self) -> u64 {
        self.video_end_pts_ns
    }

    pub fn save_cursor_pts_ns(&self) -> u64 {
        self.save_cursor_pts_ns
    }

    pub fn into_tracks(self) -> EncodedReplaySnapshot {
        let video_packets: Vec<&EncodedPacket> = self
            .packets
            .iter()
            .filter(|packet| {
                packet.stream == EncodedStreamKind::Video
                    && packet.pts_ns >= self.first_video_pts_ns
            })
            .collect();
        let video_duration_ns = self
            .video_end_pts_ns
            .saturating_sub(self.first_video_pts_ns)
            .max(1);
        let video_duration_90k = scale_ns_to_90k(video_duration_ns).max(1);
        let presentation_duration_100ns = scale_ns_to_100ns(video_duration_ns).max(1);
        let mut video_samples = self.header_units;
        video_samples.extend(video_packets.into_iter().map(|packet| HevcAccessUnit {
            timestamp_90k: scale_ns_to_90k(packet.pts_ns.saturating_sub(self.first_video_pts_ns)),
            presentation_timestamp_100ns: Some(scale_ns_to_100ns(
                packet.pts_ns.saturating_sub(self.first_video_pts_ns),
            )),
            data: packet.data.clone(),
            is_sync: packet.is_key,
            discard_from_track: false,
        }));
        let video_track = HevcMp4Track {
            width: self.metadata.width,
            height: self.metadata.height,
            duration_90k: video_duration_90k,
            presentation_duration_100ns: Some(presentation_duration_100ns),
            color: self.metadata.color,
            codec: self.metadata.codec,
            samples: video_samples,
        };
        let audio_samples: Vec<AacAccessUnit> = self
            .packets
            .iter()
            .filter(|packet| {
                packet.stream == EncodedStreamKind::Audio
                    && packet.pts_ns >= self.first_video_pts_ns
                    && packet_end_ns(packet) <= self.video_end_pts_ns
            })
            .map(|packet| AacAccessUnit {
                timestamp_ticks: scale_ns_to_ticks(
                    packet.pts_ns.saturating_sub(self.first_video_pts_ns),
                    self.metadata.audio_sample_rate,
                ),
                duration_ticks: scale_ns_to_ticks(
                    packet.duration_ns,
                    self.metadata.audio_sample_rate,
                )
                .max(1)
                .min(u64::from(u32::MAX)) as u32,
                data: packet.data.clone(),
            })
            .collect();
        let audio_track = if audio_samples.is_empty() {
            None
        } else {
            let audio_duration_ticks = audio_samples
                .iter()
                .map(|sample| u64::from(sample.duration_ticks))
                .sum::<u64>()
                .max(1);
            Some(AacLcMp4Track {
                sample_rate: self.metadata.audio_sample_rate,
                channel_count: self.metadata.audio_channel_count,
                duration_ticks: audio_duration_ticks,
                samples: audio_samples,
            })
        };
        EncodedReplaySnapshot {
            video_track,
            audio_track,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncodedReplayAvailability {
    pub metadata_ready: bool,
    pub parameter_sets_ready: bool,
    pub video_packets: usize,
    pub video_key_packets: usize,
    pub audio_packets: usize,
    pub pending_video_packet: bool,
    pub pending_video_key: bool,
    pub bytes: usize,
}

#[derive(Debug, Clone)]
pub struct EncodedReplayRing {
    ring: EncodedRingBuffer,
    codec_epoch: u64,
    metadata: Option<EncodedReplayMetadata>,
    next_segment_base_ns: u64,
    active_segment_base_ns: u64,
    pending_video: Option<EncodedPacket>,
    parameter_sets: crate::backend::mp4_mux::HevcParameterSetTracker,
    header_units: Vec<HevcAccessUnit>,
}

impl EncodedReplayRing {
    pub fn new(retention: Duration) -> Self {
        Self {
            ring: EncodedRingBuffer::new(retention),
            codec_epoch: 0,
            metadata: None,
            next_segment_base_ns: 0,
            active_segment_base_ns: 0,
            pending_video: None,
            parameter_sets: crate::backend::mp4_mux::HevcParameterSetTracker::default(),
            header_units: Vec::new(),
        }
    }

    pub fn push_tracks(&mut self, video: &HevcMp4Track, audio: Option<&AacLcMp4Track>) {
        self.start_segment(EncodedReplayMetadata {
            width: video.width,
            height: video.height,
            color: video.color,
            codec: video.codec,
            audio_sample_rate: audio.map(|track| track.sample_rate).unwrap_or(48_000),
            audio_channel_count: audio.map(|track| track.channel_count).unwrap_or(2),
        });
        for sample in &video.samples {
            self.push_video_au_90k(sample);
        }
        self.finish_segment_with_presentation_duration(
            video.duration_90k,
            video.presentation_duration_100ns,
            audio,
        );
    }

    pub fn start_segment(&mut self, metadata: EncodedReplayMetadata) {
        if self
            .metadata
            .as_ref()
            .is_some_and(|current| current != &metadata)
        {
            self.reset_codec_epoch();
        } else {
            self.flush_pending_video_with_source_tail();
        }
        self.metadata = Some(metadata);
        self.active_segment_base_ns = self.next_segment_base_ns;
        self.pending_video = None;
    }

    pub fn restart_timeline(&mut self, metadata: EncodedReplayMetadata) {
        if self
            .metadata
            .as_ref()
            .is_some_and(|current| current != &metadata)
        {
            self.reset_codec_epoch();
        } else {
            self.reset_media_timeline();
        }
        self.metadata = Some(metadata);
    }

    pub fn abort_segment(&mut self) {
        self.flush_pending_video_with_source_tail();
        self.next_segment_base_ns = self
            .ring
            .newest_end_ns()
            .max(self.active_segment_base_ns.saturating_add(1));
        self.active_segment_base_ns = self.next_segment_base_ns;
    }

    pub fn push_video_au_90k(&mut self, sample: &HevcAccessUnit) {
        self.remember_parameter_sets(sample);
        if sample.discard_from_track {
            return;
        }
        let relative_pts_ns = sample
            .presentation_timestamp_100ns
            .map(scale_100ns_to_ns)
            .unwrap_or_else(|| scale_90k_to_ns(sample.timestamp_90k));
        let pts_ns = self.active_segment_base_ns.saturating_add(relative_pts_ns);
        if let Some(mut previous) = self.pending_video.take() {
            previous.duration_ns = pts_ns.saturating_sub(previous.pts_ns).max(1);
            self.ring.push(previous);
        }
        self.pending_video = Some(EncodedPacket {
            stream: EncodedStreamKind::Video,
            pts_ns,
            dts_ns: pts_ns,
            duration_ns: 1,
            is_key: sample.is_sync,
            data: sample.data.clone(),
        });
    }

    pub fn push_audio_au_ticks(&mut self, sample: &AacAccessUnit, sample_rate: u32) {
        let start_offset_ns = scale_ticks_to_ns(sample.timestamp_ticks, sample_rate);
        let end_offset_ns = scale_ticks_to_ns(
            sample
                .timestamp_ticks
                .saturating_add(u64::from(sample.duration_ticks)),
            sample_rate,
        );
        let pts_ns = self.active_segment_base_ns.saturating_add(start_offset_ns);
        self.ring.push(EncodedPacket {
            stream: EncodedStreamKind::Audio,
            pts_ns,
            dts_ns: pts_ns,
            duration_ns: end_offset_ns.saturating_sub(start_offset_ns).max(1),
            is_key: true,
            data: sample.data.clone(),
        });
    }

    pub fn finish_segment(&mut self, video_duration_90k: u64, audio: Option<&AacLcMp4Track>) {
        self.finish_segment_with_presentation_duration(video_duration_90k, None, audio);
    }

    pub fn finish_segment_with_presentation_duration(
        &mut self,
        video_duration_90k: u64,
        presentation_duration_100ns: Option<u64>,
        audio: Option<&AacLcMp4Track>,
    ) {
        let base_ns = self.active_segment_base_ns;
        let video_duration_ns = presentation_duration_100ns
            .map(scale_100ns_to_ns)
            .unwrap_or_else(|| scale_90k_to_ns(video_duration_90k))
            .max(1);
        self.flush_pending_video_with_duration_ns(video_duration_ns);
        let mut audio_packets = Vec::new();
        if let Some(audio) = audio {
            for sample in &audio.samples {
                let start_offset_ns = scale_ticks_to_ns(sample.timestamp_ticks, audio.sample_rate);
                let end_offset_ns = scale_ticks_to_ns(
                    sample
                        .timestamp_ticks
                        .saturating_add(u64::from(sample.duration_ticks)),
                    audio.sample_rate,
                );
                audio_packets.push(EncodedPacket {
                    stream: EncodedStreamKind::Audio,
                    pts_ns: base_ns.saturating_add(start_offset_ns),
                    dts_ns: base_ns.saturating_add(start_offset_ns),
                    duration_ns: end_offset_ns.saturating_sub(start_offset_ns).max(1),
                    is_key: true,
                    data: sample.data.clone(),
                });
            }
        }
        audio_packets.sort_by_key(|packet| (packet.pts_ns, packet.stream_sort_key()));
        for packet in audio_packets {
            self.ring.push(packet);
        }
        let segment_duration_ns = video_duration_ns
            .max(
                audio
                    .map(|track| scale_ticks_to_ns(track.duration_ticks, track.sample_rate))
                    .unwrap_or(0),
            )
            .max(1);
        self.next_segment_base_ns = base_ns.saturating_add(segment_duration_ns);
    }

    pub fn finish_segment_with_audio_duration(
        &mut self,
        video_duration_90k: u64,
        audio_duration_ticks: Option<(u64, u32)>,
    ) {
        self.finish_segment_with_audio_and_presentation_duration(
            video_duration_90k,
            None,
            audio_duration_ticks,
        );
    }

    pub fn finish_segment_with_audio_and_presentation_duration(
        &mut self,
        video_duration_90k: u64,
        presentation_duration_100ns: Option<u64>,
        audio_duration_ticks: Option<(u64, u32)>,
    ) {
        let base_ns = self.active_segment_base_ns;
        let video_duration_ns = presentation_duration_100ns
            .map(scale_100ns_to_ns)
            .unwrap_or_else(|| scale_90k_to_ns(video_duration_90k))
            .max(1);
        self.flush_pending_video_with_duration_ns(video_duration_ns);
        let audio_duration_ns = audio_duration_ticks
            .map(|(ticks, sample_rate)| scale_ticks_to_ns(ticks, sample_rate))
            .unwrap_or(0);
        self.next_segment_base_ns =
            base_ns.saturating_add(video_duration_ns.max(audio_duration_ns).max(1));
    }

    fn flush_pending_video_with_duration_ns(&mut self, segment_end_offset_ns: u64) {
        if let Some(mut previous) = self.pending_video.take() {
            let segment_end_ns = self
                .active_segment_base_ns
                .saturating_add(segment_end_offset_ns);
            previous.duration_ns = segment_end_ns.saturating_sub(previous.pts_ns).max(1);
            self.ring.push(previous);
        }
    }

    fn flush_pending_video_with_source_tail(&mut self) {
        let Some(pending) = self.pending_video.as_ref() else {
            return;
        };
        let source_tail_ns = self
            .ring
            .video_packets
            .back()
            .map(|packet| packet.duration_ns.max(1))
            .unwrap_or(1);
        let segment_end_offset_ns = pending
            .pts_ns
            .saturating_sub(self.active_segment_base_ns)
            .saturating_add(source_tail_ns);
        self.flush_pending_video_with_duration_ns(segment_end_offset_ns);
    }

    fn remember_parameter_sets(&mut self, sample: &HevcAccessUnit) {
        if self.parameter_sets.is_ready() && !sample.discard_from_track && !sample.is_sync {
            return;
        }
        if !self.parameter_sets.observe(&sample.data, sample.is_sync) {
            return;
        }
        let Some(parameter_sets) = self.parameter_sets.header_access_unit() else {
            return;
        };
        if !self.header_units.is_empty() || !self.ring.is_empty() || self.pending_video.is_some() {
            self.reset_media_timeline();
            self.codec_epoch = self.codec_epoch.saturating_add(1);
        }
        self.header_units = vec![HevcAccessUnit {
            timestamp_90k: 0,
            presentation_timestamp_100ns: None,
            data: parameter_sets,
            is_sync: false,
            discard_from_track: true,
        }];
    }

    fn reset_codec_epoch(&mut self) {
        self.reset_media_timeline();
        self.codec_epoch = self.codec_epoch.saturating_add(1);
        self.parameter_sets.clear();
        self.header_units.clear();
    }

    fn reset_media_timeline(&mut self) {
        self.ring.clear();
        self.next_segment_base_ns = 0;
        self.active_segment_base_ns = 0;
        self.pending_video = None;
    }

    pub fn snapshot_recent_tracks(&self, duration: Duration) -> Option<EncodedReplaySnapshot> {
        self.snapshot_recent_packets_after(duration, None)
            .map(EncodedReplayPacketSnapshot::into_tracks)
    }

    pub fn snapshot_recent_packets_after(
        &self,
        duration: Duration,
        not_before_pts_ns: Option<u64>,
    ) -> Option<EncodedReplayPacketSnapshot> {
        let metadata = self.metadata.clone()?;
        let snapshot_ring = self.ring_with_pending_video();
        let packets =
            snapshot_ring.snapshot_recent_with_leading_video_key_after(duration, not_before_pts_ns);
        let first_video_pts = packets
            .iter()
            .filter(|packet| packet.stream == EncodedStreamKind::Video)
            .find(|packet| packet.is_key)?
            .pts_ns;
        let raw_video_end_pts_ns = packets
            .iter()
            .filter(|packet| {
                packet.stream == EncodedStreamKind::Video && packet.pts_ns >= first_video_pts
            })
            .map(|packet| packet.pts_ns.saturating_add(packet.duration_ns))
            .max()?;
        let has_audio = packets.iter().any(|packet| {
            packet.stream == EncodedStreamKind::Audio && packet.pts_ns >= first_video_pts
        });
        let synchronized_end_pts_ns = if has_audio {
            packets
                .iter()
                .filter(|packet| {
                    packet.stream == EncodedStreamKind::Audio
                        && packet.pts_ns >= first_video_pts
                        && packet_end_ns(packet) <= raw_video_end_pts_ns
                })
                .map(packet_end_ns)
                .max()?
        } else {
            raw_video_end_pts_ns
        };
        if synchronized_end_pts_ns <= first_video_pts {
            return None;
        }
        let packets = packets
            .into_iter()
            .filter_map(|mut packet| match packet.stream {
                EncodedStreamKind::Video
                    if packet.pts_ns >= first_video_pts
                        && packet.pts_ns < synchronized_end_pts_ns =>
                {
                    packet.duration_ns = packet
                        .duration_ns
                        .min(synchronized_end_pts_ns.saturating_sub(packet.pts_ns))
                        .max(1);
                    Some(packet)
                }
                EncodedStreamKind::Audio
                    if packet.pts_ns >= first_video_pts
                        && packet_end_ns(&packet) <= synchronized_end_pts_ns =>
                {
                    Some(packet)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let synchronized_video_end_pts_ns = packets
            .iter()
            .filter(|packet| packet.stream == EncodedStreamKind::Video)
            .map(packet_end_ns)
            .max()?;
        if synchronized_video_end_pts_ns != synchronized_end_pts_ns {
            return None;
        }
        Some(EncodedReplayPacketSnapshot {
            codec_epoch: self.codec_epoch,
            metadata,
            header_units: self.header_units.clone(),
            packets,
            first_video_pts_ns: first_video_pts,
            video_end_pts_ns: synchronized_end_pts_ns,
            save_cursor_pts_ns: synchronized_end_pts_ns,
        })
    }

    fn ring_with_pending_video(&self) -> EncodedRingBuffer {
        let mut ring = self.ring.clone();
        if let Some(mut pending) = self.pending_video.clone() {
            // Until the next source video timestamp arrives, the completed
            // audio/video cursor is the only observed end of this pending
            // frame.  Retain that cursor so a one-frame/static scene can be
            // saved with a playable A/V tail instead of being discarded.
            let completed_cursor_ns = ring.newest_end_ns().max(packet_end_ns(&pending));
            pending.duration_ns = completed_cursor_ns.saturating_sub(pending.pts_ns).max(1);
            ring.push(pending);
        }
        ring
    }

    pub fn availability(&self) -> EncodedReplayAvailability {
        EncodedReplayAvailability {
            metadata_ready: self.metadata.is_some(),
            parameter_sets_ready: self.parameter_sets.is_ready(),
            video_packets: self.ring.video_packets.len(),
            video_key_packets: self.ring.video_key_packets,
            audio_packets: self.ring.audio_packets.len(),
            pending_video_packet: self.pending_video.is_some(),
            pending_video_key: self
                .pending_video
                .as_ref()
                .is_some_and(|packet| packet.is_key),
            bytes: self.ring.bytes(),
        }
    }

    pub fn codec_epoch(&self) -> u64 {
        self.codec_epoch
    }

    pub fn bytes(&self) -> usize {
        self.ring.bytes()
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }
}

fn scale_90k_to_ns(value: u64) -> u64 {
    ((u128::from(value) * 1_000_000_000u128).div_ceil(90_000)).min(u128::from(u64::MAX)) as u64
}

fn scale_100ns_to_ns(value: u64) -> u64 {
    value.saturating_mul(100)
}

fn scale_ns_to_100ns(value: u64) -> u64 {
    ((u128::from(value) + 50) / 100).min(u128::from(u64::MAX)) as u64
}

fn scale_ns_to_90k(value: u64) -> u64 {
    ((u128::from(value) * 90_000u128 + 500_000_000u128) / 1_000_000_000u128)
        .min(u128::from(u64::MAX)) as u64
}

fn scale_ticks_to_ns(value: u64, sample_rate: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    ((u128::from(value) * 1_000_000_000u128).div_ceil(u128::from(sample_rate)))
        .min(u128::from(u64::MAX)) as u64
}

fn scale_ns_to_ticks(value: u64, sample_rate: u32) -> u64 {
    ((u128::from(value) * u128::from(sample_rate) + 500_000_000u128) / 1_000_000_000u128)
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_prunes_by_absolute_time() {
        let mut ring = EncodedRingBuffer::new(Duration::from_secs(2));
        ring.push(pkt(0));
        ring.push(pkt(1_000_000_000));
        ring.push(pkt(3_000_000_000));
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.video_packets.front().unwrap().pts_ns, 3_000_000_000);
    }

    #[test]
    fn ring_retains_a_vfr_packet_that_overlaps_the_retention_boundary() {
        let mut ring = EncodedRingBuffer::new(Duration::from_secs(2));
        let mut long_frame = pkt(0);
        long_frame.duration_ns = 2_000_000_000;
        ring.push(long_frame);
        ring.push(pkt(3_000_000_000));

        assert_eq!(ring.len(), 2);
        let recent = ring.snapshot_recent(Duration::from_secs(2));
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].pts_ns, 0);
    }

    #[test]
    fn ring_prunes_late_out_of_order_audio_independently() {
        let mut ring = EncodedRingBuffer::new(Duration::from_secs(2));
        ring.push(pkt(3_000_000_000));
        let mut late_audio = pkt(0);
        late_audio.stream = EncodedStreamKind::Audio;
        ring.push(late_audio);

        assert_eq!(ring.len(), 1);
        assert!(ring.audio_packets.is_empty());
        assert_eq!(ring.bytes(), 3);
    }

    #[test]
    fn snapshot_uses_time_not_frame_count() {
        let mut ring = EncodedRingBuffer::new(Duration::from_secs(60));
        ring.push(pkt(0));
        ring.push(pkt(9_000_000_000));
        ring.push(pkt(10_000_000_000));
        let recent = ring.snapshot_recent(Duration::from_secs(2));
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].pts_ns, 9_000_000_000);
    }

    #[test]
    fn snapshot_can_filter_video_and_audio_packets() {
        let mut ring = EncodedRingBuffer::new(Duration::from_secs(10));
        ring.push(pkt(1_000_000_000));
        let mut audio = pkt(1_100_000_000);
        audio.stream = EncodedStreamKind::Audio;
        ring.push(audio);
        assert_eq!(
            ring.snapshot_recent_stream(EncodedStreamKind::Video, Duration::from_secs(2))
                .len(),
            1
        );
        assert_eq!(
            ring.snapshot_recent_stream(EncodedStreamKind::Audio, Duration::from_secs(2))
                .len(),
            1
        );
    }

    #[test]
    fn replay_ring_roundtrips_tracks_with_rebased_timestamps() {
        let video = HevcMp4Track {
            width: 16,
            height: 16,
            duration_90k: 180_000,
            presentation_duration_100ns: None,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            samples: vec![hevc(0, true), hevc(90_000, false)],
        };
        let audio = AacLcMp4Track {
            sample_rate: 48_000,
            channel_count: 2,
            duration_ticks: 96_000,
            samples: vec![aac(0), aac(48_000)],
        };
        let video_bytes = video.samples[0].data.clone();
        let audio_bytes = audio.samples[0].data.clone();
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.push_tracks(&video, Some(&audio));
        let snap = replay
            .snapshot_recent_tracks(Duration::from_secs(2))
            .unwrap();
        assert_eq!(snap.video_track.width, 16);
        assert_eq!(snap.video_track.samples[0].timestamp_90k, 0);
        assert!(snap.video_track.samples[0].is_sync);
        assert!(Arc::ptr_eq(&video_bytes, &snap.video_track.samples[0].data));
        let audio_track = snap.audio_track.unwrap();
        assert_eq!(audio_track.samples[0].timestamp_ticks, 0);
        assert!(Arc::ptr_eq(&audio_bytes, &audio_track.samples[0].data));
    }

    #[test]
    fn replay_ring_availability_separates_metadata_pending_video_and_audio() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(EncodedReplayMetadata {
            width: 16,
            height: 16,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            audio_sample_rate: 48_000,
            audio_channel_count: 2,
        });
        let availability = replay.availability();
        assert!(availability.metadata_ready);
        assert_eq!(availability.video_packets, 0);
        assert_eq!(availability.audio_packets, 0);

        replay.push_video_au_90k(&hevc(0, true));
        let availability = replay.availability();
        assert_eq!(availability.video_packets, 0);
        assert!(availability.pending_video_packet);

        replay.push_video_au_90k(&hevc(90_000, false));
        replay.push_audio_au_ticks(&aac(0), 48_000);
        let availability = replay.availability();
        assert_eq!(availability.video_packets, 1);
        assert_eq!(availability.audio_packets, 1);
    }

    #[test]
    fn packet_snapshot_after_save_cursor_starts_at_next_video_key() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(EncodedReplayMetadata {
            width: 16,
            height: 16,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            audio_sample_rate: 48_000,
            audio_channel_count: 2,
        });
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_video_au_90k(&hevc(90_000, false));
        replay.push_video_au_90k(&hevc(180_000, true));
        replay.push_video_au_90k(&hevc(270_000, false));
        replay.push_video_au_90k(&hevc(360_000, false));

        let packet_snapshot = replay
            .snapshot_recent_packets_after(Duration::from_secs(10), Some(scale_90k_to_ns(135_000)))
            .unwrap();
        let snapshot = packet_snapshot.into_tracks();

        assert_eq!(snapshot.video_track.samples.len(), 3);
        assert_eq!(snapshot.video_track.samples[0].timestamp_90k, 0);
        assert!(snapshot.video_track.samples[0].is_sync);
        assert_eq!(snapshot.video_track.samples[1].timestamp_90k, 90_000);
        assert_eq!(snapshot.video_track.samples[2].timestamp_90k, 180_000);
    }

    #[test]
    fn packet_snapshot_clones_arc_references_under_ring_lock() {
        let video = HevcMp4Track {
            width: 16,
            height: 16,
            duration_90k: 180_000,
            presentation_duration_100ns: None,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            samples: vec![hevc(0, true), hevc(90_000, false)],
        };
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.push_tracks(&video, None);
        let stored = replay
            .ring
            .video_packets
            .iter()
            .next()
            .unwrap()
            .data
            .clone();

        let packet_snapshot = replay
            .snapshot_recent_packets_after(Duration::from_secs(10), None)
            .unwrap();
        let snapped = packet_snapshot
            .packets
            .iter()
            .find(|packet| packet.stream == EncodedStreamKind::Video)
            .unwrap();

        assert!(Arc::ptr_eq(&stored, &snapped.data));
    }

    #[test]
    fn replay_snapshot_requires_a_real_video_keyframe() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(metadata(16, 16));
        replay.push_video_au_90k(&hevc(0, false));
        replay.push_video_au_90k(&hevc(90_000, false));
        replay.finish_segment_with_audio_duration(180_000, None);

        assert!(
            replay
                .snapshot_recent_packets_after(Duration::from_secs(10), None)
                .is_none()
        );
    }

    #[test]
    fn metadata_change_discards_previous_codec_epoch() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(metadata(16, 16));
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_video_au_90k(&hevc(90_000, false));
        replay.finish_segment_with_audio_duration(180_000, None);
        assert!(!replay.ring.is_empty());

        let previous_epoch = replay.codec_epoch();
        replay.start_segment(metadata(32, 16));

        assert!(replay.ring.is_empty());
        assert!(replay.codec_epoch() > previous_epoch);
        assert_eq!(replay.metadata.as_ref().unwrap().width, 32);
        assert_eq!(replay.active_segment_base_ns, 0);
    }

    #[test]
    fn audio_format_change_discards_previous_codec_epoch() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        let initial = metadata(16, 16);
        replay.start_segment(initial.clone());
        replay.push_video_au_90k(&hevc_header(1));
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_audio_au_ticks(&aac(0), initial.audio_sample_rate);
        let previous_epoch = replay.codec_epoch();
        let mut changed = initial;
        changed.audio_sample_rate = 44_100;

        replay.start_segment(changed);

        assert!(replay.ring.is_empty());
        assert!(replay.pending_video.is_none());
        assert!(replay.codec_epoch() > previous_epoch);
    }

    #[test]
    fn snapshot_carries_parameter_sets_from_a_discarded_warmup_access_unit() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(metadata(16, 16));
        let header = hevc_header(1);
        replay.push_video_au_90k(&header);
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_video_au_90k(&hevc(90_000, false));

        let snapshot = replay
            .snapshot_recent_tracks(Duration::from_secs(10))
            .unwrap();

        assert!(snapshot.video_track.samples[0].discard_from_track);
        assert!(Arc::ptr_eq(
            &snapshot.video_track.samples[0].data,
            &replay.header_units[0].data
        ));
        assert_eq!(
            snapshot
                .video_track
                .samples
                .iter()
                .filter(|sample| !sample.discard_from_track)
                .count(),
            2
        );
    }

    #[test]
    fn parameter_set_change_starts_a_new_memory_ring_epoch() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(metadata(16, 16));
        replay.push_video_au_90k(&hevc_header(1));
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_video_au_90k(&hevc(90_000, false));
        assert!(!replay.ring.is_empty());
        let previous_epoch = replay.codec_epoch();

        let new_header = hevc_header(2);
        let new_header_data = new_header.data.clone();
        replay.push_video_au_90k(&new_header);
        assert!(!replay.ring.is_empty());
        assert!(replay.pending_video.is_some());
        replay.push_video_au_90k(&hevc(180_000, true));
        assert!(replay.ring.is_empty());
        assert!(replay.codec_epoch() > previous_epoch);
        replay.push_video_au_90k(&hevc(270_000, false));

        let snapshot = replay
            .snapshot_recent_tracks(Duration::from_secs(10))
            .unwrap();
        assert_eq!(snapshot.video_track.samples[0].data, new_header_data);
        assert_eq!(
            snapshot
                .video_track
                .samples
                .iter()
                .filter(|sample| !sample.discard_from_track)
                .count(),
            2
        );
    }

    #[test]
    fn codec_epoch_allows_a_controller_to_discard_a_stale_pts_cursor() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(metadata(16, 16));
        replay.push_video_au_90k(&hevc_header(1));
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_video_au_90k(&hevc(90_000, false));
        replay.push_audio_au_ticks(&aac(0), 48_000);
        let old = replay
            .snapshot_recent_packets_after(Duration::from_secs(10), None)
            .unwrap();
        let old_epoch = old.codec_epoch();
        let old_cursor = old.save_cursor_pts_ns();

        replay.start_segment(metadata(32, 16));
        replay.push_video_au_90k(&hevc_header(2));
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_audio_au_ticks(&aac(0), 48_000);

        assert_ne!(replay.codec_epoch(), old_epoch);
        assert!(
            replay
                .snapshot_recent_packets_after(Duration::from_secs(10), Some(old_cursor))
                .is_none()
        );
        assert!(
            replay
                .snapshot_recent_packets_after(Duration::from_secs(10), None)
                .is_some()
        );
    }

    #[test]
    fn aborted_segment_advances_same_metadata_timeline_before_restart() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(metadata(16, 16));
        replay.push_video_au_90k(&hevc_header(1));
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_video_au_90k(&hevc(90_000, false));
        replay.abort_segment();
        let previous_end = replay.ring.newest_end_ns();

        replay.start_segment(metadata(16, 16));
        replay.push_video_au_90k(&hevc(0, true));

        assert!(
            replay
                .pending_video
                .as_ref()
                .is_some_and(|packet| packet.pts_ns >= previous_end)
        );
    }

    #[test]
    fn availability_requires_complete_parameter_sets_and_a_keyframe() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(metadata(16, 16));
        replay.push_video_au_90k(&hevc(0, false));
        let availability = replay.availability();
        assert!(!availability.parameter_sets_ready);
        assert_eq!(availability.video_key_packets, 0);
        assert!(!availability.pending_video_key);

        replay.push_video_au_90k(&hevc_header(1));
        replay.push_video_au_90k(&hevc(90_000, true));
        let availability = replay.availability();
        assert!(availability.parameter_sets_ready);
        assert!(availability.pending_video_key);
    }

    #[test]
    fn save_cursor_includes_pending_video_without_leaking_it_into_the_next_save() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(EncodedReplayMetadata {
            width: 16,
            height: 16,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            audio_sample_rate: 48_000,
            audio_channel_count: 2,
        });
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_video_au_90k(&hevc(90_000, false));
        replay.push_video_au_90k(&hevc(180_000, true));
        let first = replay
            .snapshot_recent_packets_after(Duration::from_secs(10), None)
            .unwrap();
        assert_eq!(
            first
                .packets
                .iter()
                .filter(|packet| packet.stream == EncodedStreamKind::Video)
                .count(),
            3
        );
        let save_cursor = first.save_cursor_pts_ns();

        replay.push_video_au_90k(&hevc(270_000, false));
        assert!(
            replay
                .snapshot_recent_packets_after(Duration::from_secs(10), Some(save_cursor))
                .is_none(),
            "the key frame pending at save time must not leak into the next replay"
        );

        replay.push_video_au_90k(&hevc(360_000, true));
        replay.push_video_au_90k(&hevc(450_000, false));
        let next = replay
            .snapshot_recent_packets_after(Duration::from_secs(10), Some(save_cursor))
            .unwrap()
            .into_tracks();
        assert_eq!(next.video_track.samples.len(), 2);
        assert_eq!(next.video_track.samples[0].timestamp_90k, 0);
        assert!(next.video_track.samples[0].is_sync);
        assert_eq!(next.video_track.samples[1].timestamp_90k, 90_000);
    }

    #[test]
    fn snapshot_can_save_a_single_pending_idr_using_the_completed_audio_cursor() {
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.start_segment(metadata(16, 16));
        replay.push_video_au_90k(&hevc(0, true));
        replay.push_audio_au_ticks(&aac(0), 48_000);

        let snapshot = replay
            .snapshot_recent_packets_after(Duration::from_secs(10), None)
            .unwrap();
        let expected_audio_end_ns = scale_ticks_to_ns(1_024, 48_000);

        assert_eq!(snapshot.first_video_pts_ns, 0);
        assert_eq!(snapshot.video_end_pts_ns, expected_audio_end_ns);
        assert_eq!(snapshot.save_cursor_pts_ns(), expected_audio_end_ns);
        let tracks = snapshot.into_tracks();
        assert_eq!(tracks.video_track.samples.len(), 1);
        assert!(tracks.video_track.samples[0].is_sync);
        assert_eq!(tracks.video_track.duration_90k, 1_920);
        assert_eq!(tracks.audio_track.unwrap().samples.len(), 1);
    }

    fn pkt(pts_ns: u64) -> EncodedPacket {
        EncodedPacket {
            stream: EncodedStreamKind::Video,
            pts_ns,
            dts_ns: pts_ns,
            duration_ns: 1,
            is_key: false,
            data: vec![1, 2, 3].into(),
        }
    }

    fn hevc(timestamp_90k: u64, is_sync: bool) -> HevcAccessUnit {
        HevcAccessUnit {
            timestamp_90k,
            presentation_timestamp_100ns: None,
            data: vec![0, 0, 1, 38, 1].into(),
            is_sync,
            discard_from_track: false,
        }
    }

    fn hevc_header(seed: u8) -> HevcAccessUnit {
        let mut data = Vec::new();
        for nal_type in [32u8, 33, 34] {
            data.extend_from_slice(&[0, 0, 0, 1, nal_type << 1, 1, seed]);
        }
        HevcAccessUnit {
            timestamp_90k: 0,
            presentation_timestamp_100ns: None,
            data: data.into(),
            is_sync: false,
            discard_from_track: true,
        }
    }

    fn aac(timestamp_ticks: u64) -> AacAccessUnit {
        AacAccessUnit {
            timestamp_ticks,
            duration_ticks: 1024,
            data: vec![0x21, 0x10].into(),
        }
    }

    fn metadata(width: u16, height: u16) -> EncodedReplayMetadata {
        EncodedReplayMetadata {
            width,
            height,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            audio_sample_rate: 48_000,
            audio_channel_count: 2,
        }
    }
}

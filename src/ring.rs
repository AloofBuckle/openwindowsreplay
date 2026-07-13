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
    packets: VecDeque<EncodedPacket>,
    bytes: usize,
    newest_pts_ns: u64,
    newest_end_ns: u64,
}

impl EncodedRingBuffer {
    pub fn new(retention: Duration) -> Self {
        Self {
            retention_ns: retention.as_nanos().min(u128::from(u64::MAX)) as u64,
            packets: VecDeque::new(),
            bytes: 0,
            newest_pts_ns: 0,
            newest_end_ns: 0,
        }
    }

    pub fn push(&mut self, packet: EncodedPacket) {
        self.bytes += packet.data.len();
        self.newest_pts_ns = self.newest_pts_ns.max(packet.pts_ns);
        self.newest_end_ns = self
            .newest_end_ns
            .max(packet.pts_ns.saturating_add(packet.duration_ns));
        self.packets.push_back(packet);
        self.prune_before(self.newest_pts_ns.saturating_sub(self.retention_ns));
    }

    pub fn snapshot_recent(&self, duration: Duration) -> Vec<EncodedPacket> {
        if self.packets.is_empty() {
            return Vec::new();
        }
        let duration_ns = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let min_pts = self.newest_pts_ns.saturating_sub(duration_ns);
        self.packets
            .iter()
            .filter(|packet| packet.pts_ns >= min_pts)
            .cloned()
            .collect()
    }

    pub fn snapshot_recent_with_leading_video_key(&self, duration: Duration) -> Vec<EncodedPacket> {
        self.snapshot_recent_with_leading_video_key_after(duration, None)
    }

    fn snapshot_recent_with_leading_video_key_after(
        &self,
        duration: Duration,
        not_before_pts_ns: Option<u64>,
    ) -> Vec<EncodedPacket> {
        if self.packets.is_empty() {
            return Vec::new();
        }
        let duration_ns = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let min_pts = self.newest_pts_ns.saturating_sub(duration_ns);
        let start_pts = if let Some(not_before) = not_before_pts_ns {
            let window_start = min_pts.max(not_before);
            let Some(key) = self.packets.iter().find(|packet| {
                packet.stream == EncodedStreamKind::Video
                    && packet.is_key
                    && packet.pts_ns >= window_start
            }) else {
                return Vec::new();
            };
            key.pts_ns
        } else {
            self.packets
                .iter()
                .rev()
                .find(|packet| {
                    packet.stream == EncodedStreamKind::Video
                        && packet.is_key
                        && packet.pts_ns <= min_pts
                })
                .map(|packet| packet.pts_ns)
                .unwrap_or(min_pts)
        };
        self.packets
            .iter()
            .filter(|packet| packet.pts_ns >= start_pts)
            .cloned()
            .collect()
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
        self.packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    fn prune_before(&mut self, min_pts: u64) {
        while self
            .packets
            .front()
            .is_some_and(|packet| packet.pts_ns < min_pts)
        {
            if let Some(packet) = self.packets.pop_front() {
                self.bytes = self.bytes.saturating_sub(packet.data.len());
            }
        }
    }
}

#[derive(Debug, Clone)]
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
    metadata: EncodedReplayMetadata,
    packets: Vec<EncodedPacket>,
    first_video_pts_ns: u64,
    video_end_pts_ns: u64,
    save_cursor_pts_ns: u64,
}

impl EncodedReplayPacketSnapshot {
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
        let video_duration_90k = video_packets
            .iter()
            .map(|packet| {
                scale_ns_to_90k(packet.pts_ns.saturating_sub(self.first_video_pts_ns))
                    .saturating_add(scale_ns_to_90k(packet.duration_ns).max(1))
            })
            .max()
            .unwrap_or(1)
            .max(1);
        let video_track = HevcMp4Track {
            width: self.metadata.width,
            height: self.metadata.height,
            duration_90k: video_duration_90k,
            color: self.metadata.color,
            codec: self.metadata.codec,
            samples: video_packets
                .into_iter()
                .map(|packet| HevcAccessUnit {
                    timestamp_90k: scale_ns_to_90k(
                        packet.pts_ns.saturating_sub(self.first_video_pts_ns),
                    ),
                    data: packet.data.clone(),
                    is_sync: packet.is_key,
                    discard_from_track: false,
                })
                .collect(),
        };
        let audio_samples: Vec<AacAccessUnit> = self
            .packets
            .iter()
            .filter(|packet| {
                packet.stream == EncodedStreamKind::Audio
                    && packet.pts_ns >= self.first_video_pts_ns
                    && packet.pts_ns < self.video_end_pts_ns
            })
            .map(|packet| AacAccessUnit {
                timestamp_ticks: scale_ns_to_ticks(
                    packet.pts_ns.saturating_sub(self.first_video_pts_ns),
                    self.metadata.audio_sample_rate,
                ),
                duration_ticks: scale_ns_to_ticks(
                    packet
                        .duration_ns
                        .min(self.video_end_pts_ns.saturating_sub(packet.pts_ns)),
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
            let video_duration_ticks = scale_ns_to_ticks(
                self.video_end_pts_ns
                    .saturating_sub(self.first_video_pts_ns),
                self.metadata.audio_sample_rate,
            )
            .max(1);
            Some(AacLcMp4Track {
                sample_rate: self.metadata.audio_sample_rate,
                channel_count: self.metadata.audio_channel_count,
                duration_ticks: video_duration_ticks,
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
    pub video_packets: usize,
    pub audio_packets: usize,
    pub pending_video_packet: bool,
    pub bytes: usize,
}

#[derive(Debug, Clone)]
pub struct EncodedReplayRing {
    ring: EncodedRingBuffer,
    metadata: Option<EncodedReplayMetadata>,
    next_segment_base_ns: u64,
    active_segment_base_ns: u64,
    pending_video: Option<EncodedPacket>,
}

impl EncodedReplayRing {
    pub fn new(retention: Duration) -> Self {
        Self {
            ring: EncodedRingBuffer::new(retention),
            metadata: None,
            next_segment_base_ns: 0,
            active_segment_base_ns: 0,
            pending_video: None,
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
        for sample in video
            .samples
            .iter()
            .filter(|sample| !sample.discard_from_track)
        {
            self.push_video_au_90k(sample);
        }
        self.finish_segment(video.duration_90k, audio);
    }

    pub fn start_segment(&mut self, metadata: EncodedReplayMetadata) {
        self.flush_pending_video_with_duration_ns(1);
        self.metadata = Some(EncodedReplayMetadata {
            width: metadata.width,
            height: metadata.height,
            color: metadata.color,
            codec: metadata.codec,
            audio_sample_rate: metadata.audio_sample_rate,
            audio_channel_count: metadata.audio_channel_count,
        });
        self.active_segment_base_ns = self.next_segment_base_ns;
        self.pending_video = None;
    }

    pub fn push_video_au_90k(&mut self, sample: &HevcAccessUnit) {
        if sample.discard_from_track {
            return;
        }
        let pts_ns = self
            .active_segment_base_ns
            .saturating_add(scale_90k_to_ns(sample.timestamp_90k));
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
        let pts_ns = self
            .active_segment_base_ns
            .saturating_add(scale_ticks_to_ns(sample.timestamp_ticks, sample_rate));
        self.ring.push(EncodedPacket {
            stream: EncodedStreamKind::Audio,
            pts_ns,
            dts_ns: pts_ns,
            duration_ns: scale_ticks_to_ns(u64::from(sample.duration_ticks), sample_rate).max(1),
            is_key: true,
            data: sample.data.clone(),
        });
    }

    pub fn finish_segment(&mut self, video_duration_90k: u64, audio: Option<&AacLcMp4Track>) {
        let base_ns = self.active_segment_base_ns;
        let video_duration_ns = scale_90k_to_ns(video_duration_90k).max(1);
        self.flush_pending_video_with_duration_ns(video_duration_ns);
        let mut audio_packets = Vec::new();
        if let Some(audio) = audio {
            for sample in &audio.samples {
                audio_packets.push(EncodedPacket {
                    stream: EncodedStreamKind::Audio,
                    pts_ns: base_ns.saturating_add(scale_ticks_to_ns(
                        sample.timestamp_ticks,
                        audio.sample_rate,
                    )),
                    dts_ns: base_ns.saturating_add(scale_ticks_to_ns(
                        sample.timestamp_ticks,
                        audio.sample_rate,
                    )),
                    duration_ns: scale_ticks_to_ns(
                        u64::from(sample.duration_ticks),
                        audio.sample_rate,
                    )
                    .max(1),
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
        let base_ns = self.active_segment_base_ns;
        let video_duration_ns = scale_90k_to_ns(video_duration_90k).max(1);
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
        let packets = self
            .ring
            .snapshot_recent_with_leading_video_key_after(duration, not_before_pts_ns);
        let first_video_pts = packets
            .iter()
            .filter(|packet| packet.stream == EncodedStreamKind::Video)
            .find(|packet| packet.is_key)
            .or_else(|| {
                packets
                    .iter()
                    .find(|packet| packet.stream == EncodedStreamKind::Video)
            })?
            .pts_ns;
        let video_end_pts_ns = packets
            .iter()
            .filter(|packet| {
                packet.stream == EncodedStreamKind::Video && packet.pts_ns >= first_video_pts
            })
            .map(|packet| packet.pts_ns.saturating_add(packet.duration_ns))
            .max()?;
        let save_cursor_pts_ns = self
            .pending_video
            .as_ref()
            .map(|packet| packet.pts_ns.saturating_add(packet.duration_ns))
            .unwrap_or(video_end_pts_ns)
            .max(video_end_pts_ns);
        Some(EncodedReplayPacketSnapshot {
            metadata,
            packets,
            first_video_pts_ns: first_video_pts,
            video_end_pts_ns,
            save_cursor_pts_ns,
        })
    }

    pub fn availability(&self) -> EncodedReplayAvailability {
        EncodedReplayAvailability {
            metadata_ready: self.metadata.is_some(),
            video_packets: self
                .ring
                .packets
                .iter()
                .filter(|packet| packet.stream == EncodedStreamKind::Video)
                .count(),
            audio_packets: self
                .ring
                .packets
                .iter()
                .filter(|packet| packet.stream == EncodedStreamKind::Audio)
                .count(),
            pending_video_packet: self.pending_video.is_some(),
            bytes: self.ring.bytes(),
        }
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
        assert_eq!(ring.len(), 2);
        assert_eq!(ring.packets.front().unwrap().pts_ns, 1_000_000_000);
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

        assert_eq!(snapshot.video_track.samples.len(), 2);
        assert_eq!(snapshot.video_track.samples[0].timestamp_90k, 0);
        assert!(snapshot.video_track.samples[0].is_sync);
        assert_eq!(snapshot.video_track.samples[1].timestamp_90k, 90_000);
    }

    #[test]
    fn packet_snapshot_clones_arc_references_under_ring_lock() {
        let video = HevcMp4Track {
            width: 16,
            height: 16,
            duration_90k: 180_000,
            color: NclxColorMetadata::bt709_full(),
            codec: HevcCodecMetadata::main_420_8(),
            samples: vec![hevc(0, true), hevc(90_000, false)],
        };
        let mut replay = EncodedReplayRing::new(Duration::from_secs(10));
        replay.push_tracks(&video, None);
        let stored = replay
            .ring
            .packets
            .iter()
            .find(|packet| packet.stream == EncodedStreamKind::Video)
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
    fn save_cursor_excludes_video_pending_when_save_was_clicked() {
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
        let save_cursor = replay
            .snapshot_recent_packets_after(Duration::from_secs(10), None)
            .unwrap()
            .save_cursor_pts_ns();

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
        assert_eq!(next.video_track.samples.len(), 1);
        assert_eq!(next.video_track.samples[0].timestamp_90k, 0);
        assert!(next.video_track.samples[0].is_sync);
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
            data: vec![0, 0, 1, 38, 1].into(),
            is_sync,
            discard_from_track: false,
        }
    }

    fn aac(timestamp_ticks: u64) -> AacAccessUnit {
        AacAccessUnit {
            timestamp_ticks,
            duration_ticks: 1024,
            data: vec![0x21, 0x10].into(),
        }
    }
}

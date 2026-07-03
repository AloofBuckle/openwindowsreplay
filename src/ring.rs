#![allow(dead_code)]
//! 已编码码流环形缓存。
//!
//! 文档要求裸 HEVC/AAC 码流在内存中循环保存；此模块只保存“已编码字节”。
//! 这些字节不属于 raw frame surface，因此不违反 GPU-only 的 raw frame 路径约束。

use std::collections::VecDeque;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct EncodedPacket {
    pub pts_ns: u64,
    pub dts_ns: u64,
    pub duration_ns: u64,
    pub is_key: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct EncodedRingBuffer {
    retention_ns: u64,
    packets: VecDeque<EncodedPacket>,
    bytes: usize,
}

impl EncodedRingBuffer {
    pub fn new(retention: Duration) -> Self {
        Self {
            retention_ns: retention.as_nanos().min(u128::from(u64::MAX)) as u64,
            packets: VecDeque::new(),
            bytes: 0,
        }
    }

    pub fn push(&mut self, packet: EncodedPacket) {
        self.bytes += packet.data.len();
        let newest_pts = packet.pts_ns;
        self.packets.push_back(packet);
        self.prune_before(newest_pts.saturating_sub(self.retention_ns));
    }

    pub fn snapshot_recent(&self, duration: Duration) -> Vec<EncodedPacket> {
        let Some(last) = self.packets.back() else {
            return Vec::new();
        };
        let duration_ns = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        let min_pts = last.pts_ns.saturating_sub(duration_ns);
        self.packets
            .iter()
            .filter(|packet| packet.pts_ns >= min_pts)
            .cloned()
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

    fn pkt(pts_ns: u64) -> EncodedPacket {
        EncodedPacket {
            pts_ns,
            dts_ns: pts_ns,
            duration_ns: 1,
            is_key: false,
            data: vec![1, 2, 3],
        }
    }
}

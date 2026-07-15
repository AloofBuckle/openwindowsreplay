use super::HevcParameterSets;
use super::writer::find_start_code;
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub(crate) struct HevcParameterSetTracker {
    active: Option<HevcParameterSets>,
    staged: HevcParameterSets,
}

impl HevcParameterSetTracker {
    pub(crate) fn observe(&mut self, data: &[u8], is_sync: bool) -> bool {
        let found = extract_hevc_parameter_sets(data);
        if found.is_empty() && !is_sync {
            return false;
        }

        if self.active.is_none() {
            merge_categories(&mut self.staged, found);
            if self.staged.is_complete() {
                self.active = Some(std::mem::take(&mut self.staged));
                return true;
            }
            return false;
        }

        let active = self.active.as_ref().expect("active parameter sets exist");
        stage_changed_categories(&mut self.staged, active, found);
        if !is_sync || self.staged.is_empty() {
            return false;
        }

        let mut candidate = active.clone();
        replace_non_empty_categories(&mut candidate, std::mem::take(&mut self.staged));
        if candidate == *active {
            return false;
        }
        self.active = Some(candidate);
        true
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(HevcParameterSets::is_complete)
    }

    pub(crate) fn header_access_unit(&self) -> Option<Arc<[u8]>> {
        self.active
            .as_ref()
            .filter(|sets| sets.is_complete())
            .map(|sets| canonical_annex_b_header(sets).into())
    }

    pub(crate) fn clear(&mut self) {
        self.active = None;
        self.staged = HevcParameterSets::default();
    }
}

impl HevcParameterSets {
    pub(crate) fn is_complete(&self) -> bool {
        !self.vps.is_empty() && !self.sps.is_empty() && !self.pps.is_empty()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.vps.is_empty() && self.sps.is_empty() && self.pps.is_empty()
    }
}

pub(crate) fn extract_hevc_parameter_sets(data: &[u8]) -> HevcParameterSets {
    let mut sets = HevcParameterSets::default();
    let mut pos = 0usize;
    while let Some((start, code_len)) = find_start_code(data, pos) {
        let nal_start = start + code_len;
        let next = find_start_code(data, nal_start)
            .map(|(next_start, _)| next_start)
            .unwrap_or(data.len());
        pos = next;
        if nal_start >= next {
            continue;
        }
        let mut nal = &data[nal_start..next];
        while nal.last().copied() == Some(0) {
            nal = &nal[..nal.len() - 1];
        }
        if nal.len() < 2 {
            continue;
        }
        match (nal[0] >> 1) & 0x3f {
            32 => push_unique(&mut sets.vps, nal),
            33 => push_unique(&mut sets.sps, nal),
            34 => push_unique(&mut sets.pps, nal),
            _ => {}
        }
    }
    sets
}

pub(crate) fn canonical_annex_b_header(sets: &HevcParameterSets) -> Vec<u8> {
    let capacity = sets
        .vps
        .iter()
        .chain(&sets.sps)
        .chain(&sets.pps)
        .map(|nal| nal.len().saturating_add(4))
        .sum();
    let mut out = Vec::with_capacity(capacity);
    for nal in sets.vps.iter().chain(&sets.sps).chain(&sets.pps) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    out
}

fn push_unique(dst: &mut Vec<Vec<u8>>, nal: &[u8]) {
    if !dst.iter().any(|current| current.as_slice() == nal) {
        dst.push(nal.to_vec());
    }
}

fn merge_categories(dst: &mut HevcParameterSets, src: HevcParameterSets) {
    for nal in src.vps {
        push_unique(&mut dst.vps, &nal);
    }
    for nal in src.sps {
        push_unique(&mut dst.sps, &nal);
    }
    for nal in src.pps {
        push_unique(&mut dst.pps, &nal);
    }
}

fn stage_changed_categories(
    staged: &mut HevcParameterSets,
    active: &HevcParameterSets,
    found: HevcParameterSets,
) {
    if !found.vps.is_empty() {
        stage_category(&mut staged.vps, &active.vps, found.vps);
    }
    if !found.sps.is_empty() {
        stage_category(&mut staged.sps, &active.sps, found.sps);
    }
    if !found.pps.is_empty() {
        stage_category(&mut staged.pps, &active.pps, found.pps);
    }
}

fn stage_category(staged: &mut Vec<Vec<u8>>, active: &[Vec<u8>], found: Vec<Vec<u8>>) {
    if found == active {
        return;
    }
    for nal in found {
        push_unique(staged, &nal);
    }
}

fn replace_non_empty_categories(dst: &mut HevcParameterSets, src: HevcParameterSets) {
    if !src.vps.is_empty() {
        dst.vps = src.vps;
    }
    if !src.sps.is_empty() {
        dst.sps = src.sps;
    }
    if !src.pps.is_empty() {
        dst.pps = src.pps;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(nal_type: u8, marker: u8) -> Vec<u8> {
        vec![0, 0, 0, 1, nal_type << 1, 1, marker]
    }

    #[test]
    fn split_initial_parameter_sets_commit_only_when_complete() {
        let mut tracker = HevcParameterSetTracker::default();
        assert!(!tracker.observe(&nal(32, 1), false));
        assert!(!tracker.observe(&nal(33, 2), false));
        assert!(!tracker.is_ready());
        assert!(tracker.observe(&nal(34, 3), false));
        assert!(tracker.is_ready());
    }

    #[test]
    fn parameter_sets_of_the_same_category_accumulate_across_access_units() {
        let mut tracker = HevcParameterSetTracker::default();
        assert!(!tracker.observe(&nal(32, 1), false));
        assert!(!tracker.observe(&nal(32, 2), false));
        assert!(!tracker.observe(&nal(33, 3), false));
        assert!(tracker.observe(&nal(34, 4), false));
        let header = tracker.header_access_unit().unwrap();

        assert!(header.windows(3).any(|bytes| bytes == [32 << 1, 1, 1]));
        assert!(header.windows(3).any(|bytes| bytes == [32 << 1, 1, 2]));
    }

    #[test]
    fn partial_update_waits_for_sync_boundary() {
        let mut initial = nal(32, 1);
        initial.extend(nal(33, 2));
        initial.extend(nal(34, 3));
        let mut tracker = HevcParameterSetTracker::default();
        assert!(tracker.observe(&initial, false));
        let old_header = tracker.header_access_unit().unwrap();

        assert!(!tracker.observe(&nal(33, 9), false));
        assert_eq!(tracker.header_access_unit().unwrap(), old_header);
        assert!(tracker.observe(&nal(19, 0), true));
        assert_ne!(tracker.header_access_unit().unwrap(), old_header);
    }
}

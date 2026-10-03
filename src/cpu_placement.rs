//! Optional Windows CPU Set placement for the recording worker.
//!
//! This is deliberately opt-in (`RUSTREPLAY_CPU_PLACEMENT=...`).  Topology
//! discovery and placement are best effort: an unsupported machine or a
//! failed Windows API call must leave recording unrestricted rather than
//! prevent the application from starting.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

static CPU_PLACEMENT_PLAN: OnceLock<CpuPlacementPlan> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PlacementStage {
    Early,
    Recording,
}

impl PlacementStage {
    fn from_env() -> Result<Self, String> {
        match std::env::var("RUSTREPLAY_CPU_PLACEMENT_STAGE")
            .unwrap_or_else(|_| "recording".to_owned())
            .to_ascii_lowercase()
            .as_str()
        {
            "early" => Ok(Self::Early),
            "recording" | "recording-start" | "recording_start" => Ok(Self::Recording),
            value => Err(format!(
                "unsupported RUSTREPLAY_CPU_PLACEMENT_STAGE={value}; expected early or recording"
            )),
        }
    }
}

#[derive(Debug, Clone)]
struct CpuPlacementPlan {
    mode: String,
    stage: PlacementStage,
    selected_ids: Vec<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuSetRecord {
    pub id: u32,
    pub group: u16,
    pub logical_processor: u8,
    pub core_index: u8,
    pub last_level_cache_index: u8,
    pub numa_node_index: u8,
    pub efficiency_class: u8,
    pub scheduling_class: u8,
    pub flags: u8,
    pub l3_cache_bytes: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
struct CacheDomain {
    group: u16,
    mask: u64,
    level: u8,
    cache_bytes: u32,
}

#[derive(Debug, Clone, Serialize)]
struct PlacementReport {
    mode: String,
    stage: PlacementStage,
    applied: bool,
    reason: String,
    selected_ids: Vec<u32>,
    selected_logical_processors: usize,
    selected_physical_cores: usize,
    efficiency_classes: BTreeMap<u8, usize>,
    llc_domains: BTreeMap<String, LlcSummary>,
    cpu_sets: Vec<CpuSetRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct LlcSummary {
    logical_processors: usize,
    physical_cores: usize,
    cache_bytes: Option<u32>,
}

/// Clears an opt-in recording-stage placement when the recording worker exits.
pub struct RecordingPlacementGuard {
    active: bool,
    status: Option<String>,
}

impl RecordingPlacementGuard {
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }
}

impl Drop for RecordingPlacementGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Err(message) = clear_process_default_cpu_sets() {
            // A drop cannot surface an error to the controller.  Keep the failure
            // visible for opt-in experiment users without affecting teardown.
            eprintln!("RustReplay CPU placement release failed: {message}");
            write_placement_event("release_failed", &message);
        } else {
            write_placement_event("release", "process default CPU Sets cleared");
        }
    }
}

/// Initializes an optional placement plan without ever making startup fail.
///
/// Returns a user-readable status only for an enabled plan or a fail-open
/// warning.  The default configuration returns `None` and has no effect.
pub fn initialize_from_env() -> Option<String> {
    let mode = std::env::var("RUSTREPLAY_CPU_PLACEMENT")
        .unwrap_or_else(|_| "off".to_owned())
        .to_ascii_lowercase();
    let stage = match PlacementStage::from_env() {
        Ok(stage) => stage,
        Err(message) => return install_disabled_plan(format!("CPU placement disabled: {message}")),
    };

    if mode == "off" && std::env::var_os("RUSTREPLAY_CPU_PLACEMENT_LOG").is_none() {
        let _ = CPU_PLACEMENT_PLAN.set(CpuPlacementPlan {
            mode,
            stage,
            selected_ids: Vec::new(),
        });
        return None;
    }

    let cpu_sets = match enumerate_cpu_sets() {
        Ok(cpu_sets) => cpu_sets,
        Err(message) => return install_disabled_plan(format!("CPU placement disabled: {message}")),
    };
    let (selected_ids, reason) = select_cpu_sets(&mode, &cpu_sets);
    let should_apply = !matches!(mode.as_str(), "off" | "probe") && !selected_ids.is_empty();
    let applied = if should_apply && stage == PlacementStage::Early {
        match apply_process_cpu_sets_early(&selected_ids) {
            Ok(()) => true,
            Err(message) => {
                return install_disabled_plan(format!(
                    "CPU placement disabled before recording: {message}"
                ));
            }
        }
    } else {
        false
    };

    let plan = CpuPlacementPlan {
        mode: mode.clone(),
        stage,
        selected_ids: selected_ids.clone(),
    };
    if CPU_PLACEMENT_PLAN.set(plan).is_err() {
        return Some("CPU placement was initialized more than once; later plan ignored".to_owned());
    }

    let report = build_report(
        mode.clone(),
        stage,
        applied,
        reason.clone(),
        selected_ids.clone(),
        cpu_sets,
    );
    let log_warning = std::env::var_os("RUSTREPLAY_CPU_PLACEMENT_LOG").and_then(|path| {
        write_json(Path::new(&path), &report)
            .err()
            .map(|message| format!("CPU placement report was not written: {message}"))
    });

    let summary = if should_apply {
        format!(
            "CPU placement prepared stage={} mode={} CPU Sets={}",
            match stage {
                PlacementStage::Early => "early",
                PlacementStage::Recording => "recording",
            },
            mode,
            selected_ids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",")
        )
    } else {
        format!("CPU placement not applied: {reason}")
    };
    Some(match log_warning {
        Some(warning) => format!("{summary}; {warning}"),
        None => summary,
    })
}

/// Applies a prepared placement immediately before the recording worker is
/// created.  Failure is intentionally fail-open: the caller still starts the
/// worker with Windows' normal scheduler policy.
pub fn activate_recording() -> RecordingPlacementGuard {
    let Some(plan) = CPU_PLACEMENT_PLAN.get() else {
        return RecordingPlacementGuard {
            active: false,
            status: None,
        };
    };
    if plan.stage != PlacementStage::Recording || plan.selected_ids.is_empty() {
        return RecordingPlacementGuard {
            active: false,
            status: None,
        };
    }

    match set_process_default_cpu_sets(Some(&plan.selected_ids)) {
        Ok(()) => {
            let status = format!(
                "CPU placement stage=recording activated mode={} CPU Sets={}",
                plan.mode,
                plan.selected_ids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            );
            write_placement_event("activate", &status);
            RecordingPlacementGuard {
                active: true,
                status: Some(status),
            }
        }
        Err(message) => {
            let status = format!(
                "CPU placement activation failed; recording continues unrestricted: {message}"
            );
            eprintln!("RustReplay {status}");
            write_placement_event("activate_failed", &status);
            RecordingPlacementGuard {
                active: false,
                status: Some(status),
            }
        }
    }
}

fn install_disabled_plan(status: String) -> Option<String> {
    let _ = CPU_PLACEMENT_PLAN.set(CpuPlacementPlan {
        mode: "off".to_owned(),
        stage: PlacementStage::Recording,
        selected_ids: Vec::new(),
    });
    eprintln!("RustReplay {status}");
    Some(status)
}

fn build_report(
    mode: String,
    stage: PlacementStage,
    applied: bool,
    reason: String,
    selected_ids: Vec<u32>,
    cpu_sets: Vec<CpuSetRecord>,
) -> PlacementReport {
    let selected = selected_ids.iter().copied().collect::<BTreeSet<_>>();
    let selected_records = cpu_sets
        .iter()
        .filter(|cpu| selected.contains(&cpu.id))
        .collect::<Vec<_>>();
    let selected_physical_cores = selected_records
        .iter()
        .map(|cpu| (cpu.group, cpu.core_index))
        .collect::<BTreeSet<_>>()
        .len();
    let mut efficiency_classes = BTreeMap::new();
    for cpu in &cpu_sets {
        *efficiency_classes.entry(cpu.efficiency_class).or_default() += 1;
    }
    let mut llc_groups = BTreeMap::<(u16, u8), Vec<&CpuSetRecord>>::new();
    for cpu in &cpu_sets {
        llc_groups
            .entry((cpu.group, cpu.last_level_cache_index))
            .or_default()
            .push(cpu);
    }
    let llc_domains = llc_groups
        .into_iter()
        .map(|((group, index), records)| {
            let physical_cores = records
                .iter()
                .map(|cpu| cpu.core_index)
                .collect::<BTreeSet<_>>()
                .len();
            (
                format!("g{group}:llc{index}"),
                LlcSummary {
                    logical_processors: records.len(),
                    physical_cores,
                    cache_bytes: records.iter().find_map(|cpu| cpu.l3_cache_bytes),
                },
            )
        })
        .collect();
    PlacementReport {
        mode,
        stage,
        applied,
        reason,
        selected_ids,
        selected_logical_processors: selected_records.len(),
        selected_physical_cores,
        efficiency_classes,
        llc_domains,
        cpu_sets,
    }
}

fn select_cpu_sets(mode: &str, cpu_sets: &[CpuSetRecord]) -> (Vec<u32>, String) {
    let available = cpu_sets
        .iter()
        .filter(|cpu| cpu.flags & 2 == 0 || cpu.flags & 4 != 0)
        .collect::<Vec<_>>();
    if available.is_empty() {
        return (Vec::new(), "no available CPU sets".to_owned());
    }
    let classes = available
        .iter()
        .map(|cpu| cpu.efficiency_class)
        .collect::<BTreeSet<_>>();
    let min_class = classes.iter().next().copied().unwrap_or(0);
    let max_class = classes.iter().next_back().copied().unwrap_or(0);

    let selected = match mode {
        "off" | "probe" => Vec::new(),
        "efficient" => available
            .iter()
            .filter(|cpu| cpu.efficiency_class == min_class)
            .map(|cpu| cpu.id)
            .collect(),
        "non-performance" | "non_p" => available
            .iter()
            .filter(|cpu| cpu.efficiency_class < max_class)
            .map(|cpu| cpu.id)
            .collect(),
        "small-llc" | "small_llc" => select_llc_by_size(&available, true),
        "large-llc" | "large_llc" => select_llc_by_size(&available, false),
        "lpe" => {
            let target = available
                .iter()
                .filter(|cpu| cpu.efficiency_class == min_class)
                .map(|cpu| cpu.scheduling_class)
                .max();
            target
                .map(|class| {
                    available
                        .iter()
                        .filter(|cpu| {
                            cpu.efficiency_class == min_class && cpu.scheduling_class == class
                        })
                        .map(|cpu| cpu.id)
                        .collect()
                })
                .unwrap_or_default()
        }
        "e-only" | "e_only" => {
            let target = available
                .iter()
                .filter(|cpu| cpu.efficiency_class == min_class)
                .map(|cpu| cpu.scheduling_class)
                .min();
            target
                .map(|class| {
                    available
                        .iter()
                        .filter(|cpu| {
                            cpu.efficiency_class == min_class && cpu.scheduling_class == class
                        })
                        .map(|cpu| cpu.id)
                        .collect()
                })
                .unwrap_or_default()
        }
        "auto" if classes.len() > 1 => available
            .iter()
            .filter(|cpu| cpu.efficiency_class < max_class)
            .map(|cpu| cpu.id)
            .collect(),
        "auto" => select_llc_by_size(&available, true),
        _ if mode.starts_with("class:") => mode[6..]
            .parse::<u8>()
            .ok()
            .map(|class| {
                available
                    .iter()
                    .filter(|cpu| cpu.efficiency_class == class)
                    .map(|cpu| cpu.id)
                    .collect()
            })
            .unwrap_or_default(),
        _ if mode.starts_with("llc:") => mode[4..]
            .parse::<u8>()
            .ok()
            .map(|index| {
                available
                    .iter()
                    .filter(|cpu| cpu.last_level_cache_index == index)
                    .map(|cpu| cpu.id)
                    .collect()
            })
            .unwrap_or_default(),
        _ if mode.starts_with("scheduling:") => mode[11..]
            .parse::<u8>()
            .ok()
            .map(|class| {
                available
                    .iter()
                    .filter(|cpu| cpu.scheduling_class == class)
                    .map(|cpu| cpu.id)
                    .collect()
            })
            .unwrap_or_default(),
        _ if mode.starts_with("ids:") => mode[4..]
            .split(',')
            .filter_map(|value| value.trim().parse::<u32>().ok())
            .filter(|id| available.iter().any(|cpu| cpu.id == *id))
            .collect(),
        _ => Vec::new(),
    };
    let physical_cores = available
        .iter()
        .filter(|cpu| selected.contains(&cpu.id))
        .map(|cpu| (cpu.group, cpu.core_index))
        .collect::<BTreeSet<_>>()
        .len();
    let reason = if selected.is_empty() {
        format!("mode {mode} did not identify a target")
    } else {
        format!(
            "mode {mode} selected {} CPU sets / {physical_cores} physical cores",
            selected.len()
        )
    };
    (selected, reason)
}

fn select_llc_by_size(available: &[&CpuSetRecord], smallest: bool) -> Vec<u32> {
    let mut domains = BTreeMap::<(u16, u8), (u32, Vec<u32>)>::new();
    for cpu in available {
        let Some(size) = cpu.l3_cache_bytes else {
            continue;
        };
        let entry = domains
            .entry((cpu.group, cpu.last_level_cache_index))
            .or_insert_with(|| (size, Vec::new()));
        entry.1.push(cpu.id);
    }
    let sizes = domains
        .values()
        .map(|(size, _)| *size)
        .collect::<BTreeSet<_>>();
    if sizes.len() < 2 {
        return Vec::new();
    }
    let target = if smallest {
        sizes.iter().next().copied()
    } else {
        sizes.iter().next_back().copied()
    };
    let Some(target) = target else {
        return Vec::new();
    };
    domains
        .into_values()
        .filter(|(size, _)| *size == target)
        .flat_map(|(_, ids)| ids)
        .collect()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create {} failed: {err}", parent.display()))?;
    }
    let json = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("serialize CPU placement report failed: {err}"))?;
    std::fs::write(path, json).map_err(|err| format!("write {} failed: {err}", path.display()))
}

#[cfg(windows)]
fn apply_process_cpu_sets_early(ids: &[u32]) -> Result<(), String> {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadSelectedCpuSets};

    set_process_default_cpu_sets(Some(ids))?;
    unsafe {
        if !SetThreadSelectedCpuSets(GetCurrentThread(), ids).as_bool() {
            let _ = clear_process_default_cpu_sets();
            return Err(format!(
                "SetThreadSelectedCpuSets(main) failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn apply_process_cpu_sets_early(_ids: &[u32]) -> Result<(), String> {
    Err("CPU Sets are only supported on Windows".to_owned())
}

#[cfg(windows)]
fn set_process_default_cpu_sets(ids: Option<&[u32]>) -> Result<(), String> {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessDefaultCpuSets};

    unsafe {
        if !SetProcessDefaultCpuSets(GetCurrentProcess(), ids).as_bool() {
            return Err(format!(
                "SetProcessDefaultCpuSets failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn set_process_default_cpu_sets(_ids: Option<&[u32]>) -> Result<(), String> {
    Err("CPU Sets are only supported on Windows".to_owned())
}

fn clear_process_default_cpu_sets() -> Result<(), String> {
    set_process_default_cpu_sets(None)
}

fn write_placement_event(event: &str, message: &str) {
    let Some(path) = std::env::var_os("RUSTREPLAY_CPU_PLACEMENT_EVENT_LOG") else {
        return;
    };
    let path = Path::new(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let line = serde_json::json!({
        "timestamp_ms": timestamp_ms,
        "event": event,
        "message": message,
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(windows)]
fn enumerate_cpu_sets() -> Result<Vec<CpuSetRecord>, String> {
    use windows::Win32::System::SystemInformation::{
        CpuSetInformation, GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
    };

    let caches = enumerate_l3_caches()?;
    let mut required = 0u32;
    unsafe {
        let _ = GetSystemCpuSetInformation(None, 0, &mut required, None, None);
    }
    if required == 0 {
        return Err("GetSystemCpuSetInformation returned size 0".to_owned());
    }
    let words = (required as usize).div_ceil(std::mem::size_of::<usize>());
    let mut storage = vec![0usize; words];
    let ok = unsafe {
        GetSystemCpuSetInformation(
            Some(storage.as_mut_ptr().cast::<SYSTEM_CPU_SET_INFORMATION>()),
            required,
            &mut required,
            None,
            None,
        )
        .as_bool()
    };
    if !ok {
        return Err(format!(
            "GetSystemCpuSetInformation failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let base = storage.as_ptr().cast::<u8>();
    let mut offset = 0usize;
    let mut out = Vec::new();
    while offset + 8 <= required as usize {
        let info = unsafe { &*base.add(offset).cast::<SYSTEM_CPU_SET_INFORMATION>() };
        let size = info.Size as usize;
        if size < std::mem::size_of::<SYSTEM_CPU_SET_INFORMATION>()
            || offset + size > required as usize
        {
            break;
        }
        if info.Type == CpuSetInformation {
            let cpu = unsafe { info.Anonymous.CpuSet };
            let flags = unsafe { cpu.Anonymous1.AllFlags };
            let scheduling_class = unsafe { cpu.Anonymous2.SchedulingClass };
            let bit = 1u64
                .checked_shl(u32::from(cpu.LogicalProcessorIndex))
                .unwrap_or(0);
            let l3_cache_bytes = caches
                .iter()
                .find(|cache| cache.level == 3 && cache.group == cpu.Group && cache.mask & bit != 0)
                .map(|cache| cache.cache_bytes);
            out.push(CpuSetRecord {
                id: cpu.Id,
                group: cpu.Group,
                logical_processor: cpu.LogicalProcessorIndex,
                core_index: cpu.CoreIndex,
                last_level_cache_index: cpu.LastLevelCacheIndex,
                numa_node_index: cpu.NumaNodeIndex,
                efficiency_class: cpu.EfficiencyClass,
                scheduling_class,
                flags,
                l3_cache_bytes,
            });
        }
        offset += size;
    }
    out.sort_by_key(|cpu| (cpu.group, cpu.logical_processor));
    Ok(out)
}

#[cfg(not(windows))]
fn enumerate_cpu_sets() -> Result<Vec<CpuSetRecord>, String> {
    Err("CPU Sets are only supported on Windows".to_owned())
}

#[cfg(windows)]
fn enumerate_l3_caches() -> Result<Vec<CacheDomain>, String> {
    use windows::Win32::System::SystemInformation::{
        GetLogicalProcessorInformationEx, RelationCache, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
    };

    let mut required = 0u32;
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationCache, None, &mut required) };
    if required == 0 {
        return Err("GetLogicalProcessorInformationEx returned size 0".to_owned());
    }
    let words = (required as usize).div_ceil(std::mem::size_of::<usize>());
    let mut storage = vec![0usize; words];
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationCache,
            Some(
                storage
                    .as_mut_ptr()
                    .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>(),
            ),
            &mut required,
        )
        .map_err(|err| format!("GetLogicalProcessorInformationEx failed: {err}"))?;
    }
    let base = storage.as_ptr().cast::<u8>();
    let mut offset = 0usize;
    let mut out = Vec::new();
    while offset + 8 <= required as usize {
        let info = unsafe {
            &*base
                .add(offset)
                .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>()
        };
        let size = info.Size as usize;
        if size < 8 || offset + size > required as usize {
            break;
        }
        if info.Relationship == RelationCache {
            let cache = unsafe { info.Anonymous.Cache };
            let count = cache.GroupCount.max(1) as usize;
            let masks = unsafe {
                std::slice::from_raw_parts(std::ptr::addr_of!(cache.Anonymous.GroupMask), count)
            };
            for mask in masks {
                out.push(CacheDomain {
                    group: mask.Group,
                    mask: mask.Mask as u64,
                    level: cache.Level,
                    cache_bytes: cache.CacheSize,
                });
            }
        }
        offset += size;
    }
    Ok(out)
}

#[cfg(not(windows))]
fn enumerate_l3_caches() -> Result<Vec<CacheDomain>, String> {
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(
        id: u32,
        core_index: u8,
        llc_index: u8,
        efficiency_class: u8,
        scheduling_class: u8,
        cache_bytes: u32,
    ) -> CpuSetRecord {
        CpuSetRecord {
            id,
            group: 0,
            logical_processor: id as u8,
            core_index,
            last_level_cache_index: llc_index,
            numa_node_index: 0,
            efficiency_class,
            scheduling_class,
            flags: 0,
            l3_cache_bytes: Some(cache_bytes),
        }
    }

    #[test]
    fn hybrid_modes_separate_e_and_lpe_classes() {
        let mut sets = Vec::new();
        for id in 0..4 {
            sets.push(cpu(id, id as u8, 0, 1, 2, 24 * 1024 * 1024));
        }
        for id in 4..8 {
            sets.push(cpu(id, id as u8, 0, 0, 0, 24 * 1024 * 1024));
        }
        for id in 8..12 {
            sets.push(cpu(id, id as u8, 0, 0, 1, 24 * 1024 * 1024));
        }

        assert_eq!(
            select_cpu_sets("e-only", &sets).0,
            (4..8).collect::<Vec<_>>()
        );
        assert_eq!(select_cpu_sets("lpe", &sets).0, (8..12).collect::<Vec<_>>());
        assert_eq!(
            select_cpu_sets("non_p", &sets).0,
            (4..12).collect::<Vec<_>>()
        );
    }

    #[test]
    fn llc_modes_separate_x3d_and_frequency_domains() {
        let sets = vec![
            cpu(0, 0, 0, 0, 0, 96 * 1024 * 1024),
            cpu(1, 1, 0, 0, 0, 96 * 1024 * 1024),
            cpu(2, 2, 1, 0, 0, 32 * 1024 * 1024),
            cpu(3, 3, 1, 0, 0, 32 * 1024 * 1024),
        ];

        assert_eq!(select_cpu_sets("small_llc", &sets).0, vec![2, 3]);
        assert_eq!(select_cpu_sets("large_llc", &sets).0, vec![0, 1]);
    }

    #[test]
    fn cpu_sets_allocated_elsewhere_are_excluded() {
        let mut excluded = cpu(0, 0, 0, 0, 0, 32 * 1024 * 1024);
        excluded.flags = 2;
        let mut allocated_to_self = cpu(1, 1, 0, 0, 0, 32 * 1024 * 1024);
        allocated_to_self.flags = 2 | 4;
        let available = cpu(2, 2, 0, 0, 0, 32 * 1024 * 1024);

        assert_eq!(
            select_cpu_sets("ids:0,1,2", &[excluded, allocated_to_self, available]).0,
            vec![1, 2]
        );
    }
}

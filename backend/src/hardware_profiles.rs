use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    CPU,
    CUDA,
    Vulkan,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HardwareTier {
    LowEnd,
    MidRange,
    HighEnd,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HardwareProfile {
    pub ram_gb: f32,
    pub physical_cores: usize,
    pub vram_gb: Option<f32>,
    pub tier: HardwareTier,
}

pub fn detect_hardware() -> HardwareProfile {
    let ram_gb = detect_total_ram_gb();
    let physical_cores = detect_physical_cores();
    let vram_gb = scan_nvidia_vram();
    let vulkan = check_vulkan();

    let backend = if vram_gb.is_some() {
        Backend::CUDA
    } else if vulkan {
        Backend::Vulkan
    } else {
        Backend::CPU
    };

    let tier = select_tier(ram_gb, vram_gb.unwrap_or(0.0), &backend);

    HardwareProfile { ram_gb, physical_cores, vram_gb, tier }
}

fn select_tier(ram_gb: f32, vram_gb: f32, backend: &Backend) -> HardwareTier {
    match backend {
        Backend::CUDA if vram_gb >= 16.0 => HardwareTier::HighEnd,
        Backend::CUDA if vram_gb >= 8.0 => HardwareTier::MidRange,
        // CUDA with <8 GB available VRAM: force LowEnd to prevent OOM crash on mid-range model
        Backend::CUDA => HardwareTier::LowEnd,
        Backend::Vulkan if ram_gb >= 20.0 => HardwareTier::MidRange,
        _ if ram_gb >= 24.0 => HardwareTier::MidRange,
        _ if ram_gb >= 12.0 => HardwareTier::MidRange,
        _ => HardwareTier::LowEnd,
    }
}

fn detect_total_ram_gb() -> f32 {
    #[cfg(target_os = "linux")]
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if line.starts_with("MemTotal:") {
                let kb: f32 = line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0);
                return kb / (1024.0 * 1024.0);
            }
        }
    }

    #[cfg(target_os = "macos")]
    if let Ok(out) = Command::new("sysctl").args(["-n", "hw.memsize"]).output() {
        if let Ok(s) = String::from_utf8(out.stdout) {
            let bytes: f64 = s.trim().parse().unwrap_or(0.0);
            return (bytes / (1024.0 * 1024.0 * 1024.0)) as f32;
        }
    }

    8.0
}

fn detect_physical_cores() -> usize {
    #[cfg(target_os = "linux")]
    {
        use std::collections::HashSet;
        if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
            let mut unique: HashSet<(u32, u32)> = HashSet::new();
            let mut phys_id: Option<u32> = None;
            let mut core_id: Option<u32> = None;
            for line in content.lines() {
                if let Some(val) = line.strip_prefix("physical id") {
                    phys_id = val.trim_start_matches(':').trim().parse().ok();
                } else if let Some(val) = line.strip_prefix("core id") {
                    core_id = val.trim_start_matches(':').trim().parse().ok();
                } else if line.trim().is_empty() {
                    if let (Some(p), Some(c)) = (phys_id.take(), core_id.take()) {
                        unique.insert((p, c));
                    }
                }
            }
            if !unique.is_empty() {
                return unique.len();
            }
        }
    }

    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).max(1))
        .unwrap_or(4)
}

pub fn scan_nvidia_vram() -> Option<f32> {
    let output = Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total,memory.used", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut largest_available_mb = 0.0_f32;

    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let mut fields = line.split(',').map(str::trim);
        let total_mb = fields.next()?.parse::<f32>().ok()?;
        let used_mb = fields.next()?.parse::<f32>().ok()?;
        largest_available_mb = largest_available_mb.max((total_mb - used_mb).max(0.0));
    }

    if largest_available_mb <= 0.0 {
        return None;
    }

    Some(((largest_available_mb / 1024.0) - 3.0).max(0.0))
}

fn check_vulkan() -> bool {
    if Command::new("vulkaninfo")
        .arg("--summary")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return true;
    }

    #[cfg(target_os = "linux")]
    {
        let paths = [
            "/usr/lib/libvulkan.so.1",
            "/usr/lib/x86_64-linux-gnu/libvulkan.so.1",
            "/usr/lib/aarch64-linux-gnu/libvulkan.so.1",
        ];
        if paths.iter().any(|p| std::path::Path::new(p).exists()) {
            return true;
        }
    }

    false
}

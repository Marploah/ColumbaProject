use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentTarget {
    Local(String),
    Cloud(String),
}

pub fn scan_system_vram() -> Option<f32> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.total,memory.used",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut largest_available_mb = 0.0_f32;

    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split(',').map(str::trim);
        let total_mb = fields.next()?.parse::<f32>().ok()?;
        let used_mb = fields.next()?.parse::<f32>().ok()?;

        largest_available_mb = largest_available_mb.max((total_mb - used_mb).max(0.0));
    }

    if largest_available_mb <= 0.0 {
        return None;
    }

    let available_gb = largest_available_mb / 1024.0;
    Some((available_gb - 3.0).max(0.0))
}

pub fn determine_execution_target(mode: &str, preferred_cloud: String) -> AgentTarget {
    match mode {
        "ForceCloud" => AgentTarget::Cloud(preferred_cloud),
        "ForceLocal" => AgentTarget::Local("llama3.2-vision:11b".to_string()),
        "Auto" => match scan_system_vram() {
            Some(available_gb) if available_gb >= 12.0 => {
                AgentTarget::Local("llama3.2-vision:11b".to_string())
            }
            Some(available_gb) if available_gb >= 6.5 => {
                AgentTarget::Local("llama3.2-vision:11b".to_string())
            }
            _ => AgentTarget::Cloud(preferred_cloud),
        },
        _ => AgentTarget::Cloud(preferred_cloud),
    }
}

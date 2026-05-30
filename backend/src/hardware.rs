use crate::hardware_profiles::detect_hardware;

#[derive(Debug, Clone, PartialEq)]
pub enum AgentTarget {
    Local(String),
    Cloud(String),
}

/// Minimum RAM to run the low-end 4B model set (main + draft) on CPU.
const MIN_RAM_FOR_LOCAL_GB: f32 = 5.5;

pub fn determine_execution_target(mode: &str, preferred_cloud: String) -> AgentTarget {
    match mode {
        "ForceCloud" => AgentTarget::Cloud(preferred_cloud),
        "ForceLocal" => AgentTarget::Local("qwen3".to_string()),
        "Auto" => {
            let hw = detect_hardware();
            let has_gpu = hw.vram_gb.map(|v| v >= 4.0).unwrap_or(false);
            let has_ram = hw.ram_gb >= MIN_RAM_FOR_LOCAL_GB;
            if has_gpu || has_ram {
                AgentTarget::Local("qwen3".to_string())
            } else {
                AgentTarget::Cloud(preferred_cloud)
            }
        }
        "Cloud" => AgentTarget::Cloud(preferred_cloud),
        _ => AgentTarget::Cloud(preferred_cloud),
    }
}

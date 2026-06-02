use crate::hardware_profiles::{HardwareProfile, HardwareTier};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelProfile {
    pub main_model: String,
    pub draft_model: String,
    pub context_size: usize,
    pub threads: usize,
    pub gpu_layers: i32,
}

pub fn select_model_profile(hw: &HardwareProfile) -> ModelProfile {
    let threads = ((hw.physical_cores as f32 * 0.70).ceil() as usize).max(1);
    let gpu_layers: i32 = if hw.vram_gb.is_some() { 999 } else { 0 };

    match hw.tier {
        HardwareTier::LowEnd => ModelProfile {
            main_model:   "Qwen3-4B-Q4_K_M.gguf".to_string(),
            draft_model:  "Qwen3-0.6B-Q4_0.gguf".to_string(),
            context_size: 4096,
            threads,
            gpu_layers,
        },
        HardwareTier::MidRange => ModelProfile {
            main_model:   "Qwen3-8B-Q4_K_M.gguf".to_string(),
            draft_model:  "Qwen3-1.7B-Q4_K_M.gguf".to_string(),
            context_size: 8192,
            threads,
            gpu_layers,
        },
        HardwareTier::HighEnd => ModelProfile {
            main_model:   "Qwen3-14B-Q4_K_M.gguf".to_string(),
            draft_model:  "Qwen3-4B-Q4_K_M.gguf".to_string(),
            context_size: 16384,
            threads,
            gpu_layers,
        },
    }
}

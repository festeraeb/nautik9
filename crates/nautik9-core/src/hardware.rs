use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceTier {
    /// Flagship Ultra (e.g. Samsung Galaxy S25 Ultra):
    /// Snapdragon 8 Elite / Oryon cores (ARMv9.2-A SVE2, 4.32GHz), Hexagon NPU (45 TOPS),
    /// Adreno 830 Vulkan 1.3, 12GB-16GB LPDDR5X, One UI 7 Galaxy AI suite, S-Pen digitizer.
    FlagshipS25Ultra,
    /// High-End Modern (e.g. S24 / SD 8 Gen 3, 8-12GB RAM, ARMv9 NEON)
    FlagshipHighEnd,
    /// Mid-Range (SD 7-series, Tensor G2/G3, Dimensity, 6-8GB RAM)
    MidRange,
    /// Baseline (Older ARM64, 4GB RAM, CPU-only fallback)
    Baseline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub device_name: String,
    pub tier: DeviceTier,
    pub cpu_cores: usize,
    pub ram_mb: usize,
    pub has_armv9_sve2: bool,
    pub has_hexagon_npu: bool,
    pub has_vulkan_compute: bool,
    pub one_ui_version: Option<String>,
    pub spen_supported: bool,
    pub recommended_llm_model: String,
    pub recommended_quantization: String,
    pub recommended_embedding_dim: usize,
    pub max_context_tokens: usize,
    pub max_concurrent_threads: usize,
}

impl HardwareProfile {
    /// Autodetect or scaffold configuration based on device identifiers and available sysfs/specs
    pub fn detect_or_scaffold(device_hint: Option<&str>) -> Self {
        let hint = device_hint.unwrap_or("").to_lowercase();
        
        if hint.contains("s25") || hint.contains("sm-s938") || hint.contains("sun") || hint.contains("oryon") || hint.contains("elite") {
            Self::samsung_s25_ultra()
        } else if hint.contains("s24") || hint.contains("sm-s928") || hint.contains("gen3") {
            Self::flagship_high_end("Samsung Galaxy S24 Ultra")
        } else if hint.contains("pixel") || hint.contains("dimensity") || hint.contains("mid") {
            Self::mid_range("Modern Mid-Range ARM64")
        } else if hint.contains("base") || hint.contains("low") {
            Self::baseline()
        } else {
            // Check host environment as fallback
            #[cfg(target_arch = "x86_64")]
            {
                Self::desktop_workstation()
            }
            #[cfg(not(target_arch = "x86_64"))]
            {
                Self::samsung_s25_ultra()
            }
        }
    }

    /// Optimized profile for Samsung Galaxy S25 Ultra
    pub fn samsung_s25_ultra() -> Self {
        Self {
            device_name: "Samsung Galaxy S25 Ultra (Snapdragon 8 Elite)".to_string(),
            tier: DeviceTier::FlagshipS25Ultra,
            cpu_cores: 8, // 2x Prime Oryon 4.32GHz + 6x Performance Oryon 3.53GHz
            ram_mb: 16384, // 16GB LPDDR5X
            has_armv9_sve2: true,
            has_hexagon_npu: true, // 45 TOPS NPU
            has_vulkan_compute: true, // Adreno 830
            one_ui_version: Some("7.0".to_string()),
            spen_supported: true,
            recommended_llm_model: "Qwen2.5-7B-Instruct-NautiK9".to_string(),
            recommended_quantization: "IQ4_XS".to_string(),
            recommended_embedding_dim: 768, // embeddinggemma-300m / bge-large
            max_context_tokens: 8192,
            max_concurrent_threads: 8,
        }
    }

    pub fn flagship_high_end(name: &str) -> Self {
        Self {
            device_name: name.to_string(),
            tier: DeviceTier::FlagshipHighEnd,
            cpu_cores: 8,
            ram_mb: 12288,
            has_armv9_sve2: true,
            has_hexagon_npu: true,
            has_vulkan_compute: true,
            one_ui_version: Some("6.1.1".to_string()),
            spen_supported: true,
            recommended_llm_model: "Qwen2.5-3B-Instruct".to_string(),
            recommended_quantization: "Q4_K_M".to_string(),
            recommended_embedding_dim: 768,
            max_context_tokens: 4096,
            max_concurrent_threads: 6,
        }
    }

    pub fn mid_range(name: &str) -> Self {
        Self {
            device_name: name.to_string(),
            tier: DeviceTier::MidRange,
            cpu_cores: 8,
            ram_mb: 8192,
            has_armv9_sve2: false,
            has_hexagon_npu: false,
            has_vulkan_compute: true,
            one_ui_version: None,
            spen_supported: false,
            recommended_llm_model: "Qwen2.5-1.5B-Instruct".to_string(),
            recommended_quantization: "Q4_0".to_string(),
            recommended_embedding_dim: 384,
            max_context_tokens: 2048,
            max_concurrent_threads: 4,
        }
    }

    pub fn desktop_workstation() -> Self {
        Self {
            device_name: "Host Workstation / Desktop host".to_string(),
            tier: DeviceTier::FlagshipHighEnd,
            cpu_cores: 8,
            ram_mb: 32768,
            has_armv9_sve2: false,
            has_hexagon_npu: false,
            has_vulkan_compute: true,
            one_ui_version: None,
            spen_supported: false,
            recommended_llm_model: "Qwen2.5-7B-Instruct".to_string(),
            recommended_quantization: "IQ4_XS".to_string(),
            recommended_embedding_dim: 768,
            max_context_tokens: 8192,
            max_concurrent_threads: 8,
        }
    }

    pub fn baseline() -> Self {
        Self {
            device_name: "Generic Baseline ARM64".to_string(),
            tier: DeviceTier::Baseline,
            cpu_cores: 4,
            ram_mb: 4096,
            has_armv9_sve2: false,
            has_hexagon_npu: false,
            has_vulkan_compute: false,
            one_ui_version: None,
            spen_supported: false,
            recommended_llm_model: "Qwen2.5-0.5B-Instruct".to_string(),
            recommended_quantization: "Q4_0".to_string(),
            recommended_embedding_dim: 384,
            max_context_tokens: 1024,
            max_concurrent_threads: 2,
        }
    }

    /// Returns whether this hardware tier can comfortably run full 7B/8B models locally without thermal throttle
    pub fn can_run_flagship_llm(&self) -> bool {
        self.tier == DeviceTier::FlagshipS25Ultra && self.ram_mb >= 12288
    }

    /// One UI 7 specific integrations available on S25 Ultra
    pub fn one_ui_capabilities(&self) -> Vec<&'static str> {
        if self.tier == DeviceTier::FlagshipS25Ultra {
            vec![
                "OneUI7_EdgePanel_QuickAction",
                "OneUI7_SPen_AirActions",
                "OneUI7_GalaxyAI_NativeBridge",
                "Hexagon_HTP_DirectEngine",
                "Adreno830_Vulkan13_Accelerated",
                "SuperFast_UFS4_DirectMmap",
            ]
        } else {
            vec![]
        }
    }
}

// SPDX-License-Identifier: BUSL-1.1

//! Which devices a model is loaded onto.
//!
//! Left to itself, llama.cpp offloads to every GPU it can see, and two of
//! those habits cost a lot:
//!
//! - An integrated GPU counts. On an Intel HD 630 the Vulkan build answered
//!   at 3.2 tokens/s where the CPU managed 8.2: the "GPU" shares the CPU's
//!   memory bus and is slower than the cores beside it.
//! - In the `gpu-auto` build an NVIDIA card is seen twice, once through CUDA
//!   and once through Vulkan, and the model is split between two "devices"
//!   that are one card.
//!
//! So: dedicated NVIDIA cards through CUDA when there are any, otherwise
//! dedicated cards through Vulkan, otherwise the CPU. Integrated graphics
//! only when asked for (`NEPHERAL_USE_IGPU=1`). Apple's Metal is left alone:
//! there the GPU is the fast path even though it shares memory.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Discrete,
    Integrated,
    /// The CPU itself, or an accelerator llama.cpp uses on its own terms.
    Other,
}

#[derive(Debug, Clone)]
pub struct Device {
    pub index: usize,
    pub kind: DeviceKind,
    /// The ggml backend's name: "CUDA", "Vulkan", "Metal", "CPU", ...
    pub backend: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// Leave the choice to llama.cpp.
    AllDevices,
    /// Offload to exactly these device indexes.
    Only(Vec<usize>),
    /// Keep every layer on the CPU.
    CpuOnly,
}

pub fn choose(devices: &[Device], allow_integrated: bool) -> Placement {
    let on = |d: &Device, backend: &str| d.backend.to_ascii_lowercase().starts_with(backend);
    if devices.iter().any(|d| on(d, "metal")) {
        return Placement::AllDevices;
    }
    let discrete = |backend: Option<&str>| -> Vec<usize> {
        devices
            .iter()
            .filter(|d| d.kind == DeviceKind::Discrete && backend.is_none_or(|b| on(d, b)))
            .map(|d| d.index)
            .collect()
    };
    for preferred in [Some("cuda"), Some("vulkan"), None] {
        let picked = discrete(preferred);
        if !picked.is_empty() {
            return Placement::Only(picked);
        }
    }
    if allow_integrated {
        let integrated: Vec<usize> = devices
            .iter()
            .filter(|d| d.kind == DeviceKind::Integrated)
            .map(|d| d.index)
            .collect();
        if !integrated.is_empty() {
            return Placement::Only(integrated);
        }
    }
    Placement::CpuOnly
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(index: usize, kind: DeviceKind, backend: &str) -> Device {
        Device {
            index,
            kind,
            backend: backend.into(),
        }
    }

    #[test]
    fn an_intel_igpu_alone_means_the_cpu() {
        let devices = [
            dev(0, DeviceKind::Integrated, "Vulkan"),
            dev(1, DeviceKind::Other, "CPU"),
        ];
        assert_eq!(choose(&devices, false), Placement::CpuOnly);
        assert_eq!(choose(&devices, true), Placement::Only(vec![0]));
    }

    #[test]
    fn an_nvidia_card_seen_twice_is_used_once_through_cuda() {
        let devices = [
            dev(0, DeviceKind::Discrete, "CUDA"),
            dev(1, DeviceKind::Discrete, "Vulkan"),
            dev(2, DeviceKind::Integrated, "Vulkan"),
            dev(3, DeviceKind::Other, "CPU"),
        ];
        assert_eq!(choose(&devices, false), Placement::Only(vec![0]));
    }

    #[test]
    fn an_amd_card_goes_through_vulkan_and_skips_the_igpu() {
        let devices = [
            dev(0, DeviceKind::Integrated, "Vulkan"),
            dev(1, DeviceKind::Discrete, "Vulkan"),
        ];
        assert_eq!(choose(&devices, false), Placement::Only(vec![1]));
    }

    #[test]
    fn a_rig_uses_every_cuda_card() {
        let devices = [
            dev(0, DeviceKind::Discrete, "CUDA"),
            dev(1, DeviceKind::Discrete, "CUDA"),
            dev(2, DeviceKind::Discrete, "Vulkan"),
            dev(3, DeviceKind::Discrete, "Vulkan"),
        ];
        assert_eq!(choose(&devices, false), Placement::Only(vec![0, 1]));
    }

    #[test]
    fn metal_is_left_to_llama_cpp() {
        let devices = [dev(0, DeviceKind::Integrated, "Metal")];
        assert_eq!(choose(&devices, false), Placement::AllDevices);
    }

    #[test]
    fn no_gpu_at_all_is_the_cpu() {
        assert_eq!(
            choose(&[dev(0, DeviceKind::Other, "CPU")], false),
            Placement::CpuOnly
        );
        assert_eq!(choose(&[], false), Placement::CpuOnly);
    }
}

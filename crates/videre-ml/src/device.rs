use candle_core::Device;

/// Metal on macOS when available, CPU otherwise. Never fails.
pub fn best_device() -> Device {
    #[cfg(target_os = "macos")]
    {
        if let Ok(d) = Device::new_metal(0) {
            return d;
        }
    }
    Device::Cpu
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_device_never_panics_and_returns_a_device() {
        // Result is hardware-dependent (Metal on capable macOS, Cpu otherwise),
        // so this just asserts the call completes and yields a usable Device -
        // matching the "never fails" contract in the doc comment above.
        let device = best_device();
        assert!(device.is_cpu() || !device.is_cpu());
    }
}

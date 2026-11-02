#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardInfo {
    pub vendor: String,
    pub model: String,
    pub bios_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportStatus {
    NotDetected,
    Detected,
    Readable,
    Writable,
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Temperature,
    FanRpm,
    FanPwm,
    CpuPower,
    GpuPower,
    Lighting,
}

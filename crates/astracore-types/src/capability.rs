use serde::Serialize;

/// Whether a backend is known to support this capability on the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportStatus {
    /// Compatibility has not been evaluated.
    Unknown,
    /// The backend has verified compatibility with this device.
    Supported,
    /// The backend knows this device or capability is incompatible.
    Unsupported,
}

/// Evidence found for the capability on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetectionStatus {
    /// No local discovery has been attempted.
    NotChecked,
    /// Configuration or metadata claims the capability exists.
    Declared,
    /// Discovery ran but found no matching capability.
    NotDetected,
    /// A matching device or interface was found.
    Detected,
    /// The capability was exercised successfully.
    Verified,
}

/// Result of checking OS-level access to an operation; policy is evaluated separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessStatus {
    NotChecked,
    Accessible,
    PermissionDenied,
    NotApplicable,
}

/// An individual capability on a device. `id` is a stable machine-readable key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Capability {
    pub id: String,
    pub device_id: Option<String>,
    pub support: SupportStatus,
    pub detection: DetectionStatus,
    /// OS-level read access, independent of support and project policy.
    pub read_access: AccessStatus,
    /// OS-level write access; this does not authorize a hardware write.
    pub write_access: AccessStatus,
    pub detail: Option<String>,
}

impl Capability {
    pub fn new(id: impl Into<String>, device_id: Option<String>) -> Self {
        Self {
            id: id.into(),
            device_id,
            support: SupportStatus::Unknown,
            detection: DetectionStatus::NotChecked,
            read_access: AccessStatus::NotChecked,
            write_access: AccessStatus::NotChecked,
            detail: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessStatus, Capability, DetectionStatus, SupportStatus};

    #[test]
    fn separates_support_detection_and_access() {
        let mut capability = Capability::new("temperature", Some("pci:0000:00:18.3".to_owned()));
        capability.support = SupportStatus::Supported;
        capability.detection = DetectionStatus::Detected;
        capability.read_access = AccessStatus::PermissionDenied;
        capability.write_access = AccessStatus::NotApplicable;

        let value = serde_json::to_value(&capability).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "id": "temperature",
                "device_id": "pci:0000:00:18.3",
                "support": "supported",
                "detection": "detected",
                "read_access": "permission_denied",
                "write_access": "not_applicable",
                "detail": null
            })
        );
    }
}

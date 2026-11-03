use asusio_types::{Capability, DetectionStatus, SupportStatus};

#[derive(Debug, Default)]
pub struct CapabilityRegistry {
    capabilities: Vec<Capability>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, capability: Capability) {
        if let Some(existing) = self
            .capabilities
            .iter_mut()
            .find(|item| item.id == capability.id && item.device_id == capability.device_id)
        {
            *existing = capability;
        } else {
            self.capabilities.push(capability);
        }
    }

    pub fn capabilities(&self) -> &[Capability] {
        &self.capabilities
    }

    pub fn find(&self, id: &str) -> Vec<&Capability> {
        self.capabilities
            .iter()
            .filter(|capability| capability.id == id)
            .collect()
    }

    pub fn has_supported(&self, id: &str) -> bool {
        self.capabilities
            .iter()
            .any(|capability| capability.id == id && capability.support == SupportStatus::Supported)
    }

    pub fn has_verified(&self, id: &str) -> bool {
        self.capabilities.iter().any(|capability| {
            capability.id == id && capability.detection == DetectionStatus::Verified
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_capability() {
        let mut registry = CapabilityRegistry::new();

        let mut capability = Capability::new("sensor.temperature.read", None);

        capability.support = SupportStatus::Supported;
        capability.detection = DetectionStatus::Verified;

        registry.register(capability);

        assert!(registry.has_supported("sensor.temperature.read"));
        assert!(registry.has_verified("sensor.temperature.read"));
    }

    #[test]
    fn replaces_existing_capability() {
        let mut registry = CapabilityRegistry::new();

        let first = Capability::new("asus.aura.hid", Some("aura-controller".into()));

        registry.register(first);

        let mut updated = Capability::new("asus.aura.hid", Some("aura-controller".into()));

        updated.detection = DetectionStatus::Detected;

        registry.register(updated);

        assert_eq!(registry.capabilities().len(), 1);
        assert_eq!(
            registry.capabilities()[0].detection,
            DetectionStatus::Detected
        );
    }

    #[test]
    fn keeps_same_capability_on_separate_devices() {
        let mut registry = CapabilityRegistry::new();
        registry.register(Capability::new(
            "sensor.temperature.read",
            Some("ssd-a".into()),
        ));
        registry.register(Capability::new(
            "sensor.temperature.read",
            Some("ssd-b".into()),
        ));

        assert_eq!(registry.find("sensor.temperature.read").len(), 2);
        assert_eq!(registry.capabilities().len(), 2);
    }
}

//! What AstraCore learns about the fans of the machine it runs on, without any board file:
//! which fan inputs follow which PWM output, whether an output drives a fan or a pump, and
//! a fingerprint that tells when the state file has moved to other hardware.
use crate::board::{BoardProfile, FanKind};
use astracore_types::BoardInfo;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A fan input this fast while the firmware runs its output at its own duty looks like a
/// pump: pumps run at 2000 RPM and above, case fans seldom do.
pub const PUMP_RPM_HINT: u64 = 2000;
/// An output the firmware already runs this fast cannot be tested by raising it, which is
/// also how pumps are usually run.
pub const FULL_SPEED_PERCENT: u8 = 90;
/// A fan input "follows" an output when its speed rises by at least this ratio and RPM.
pub const RESPONSE_RATIO: f64 = 0.20;
pub const RESPONSE_MIN_RPM: u64 = 150;

/// What is connected to a PWM output. There is deliberately no "unknown" value: an output
/// without a role is simply absent from the role map and cannot be given a curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanRole {
    Fan,
    /// A water pump: it is never driven by a curve.
    Pump,
}

/// Who decided a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleSource {
    /// The board profile (authoritative; the user cannot override it).
    Board,
    /// The user confirmed it.
    User,
}

/// A fan input's speed before and after one PWM output was raised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TachResponse {
    pub tach: u8,
    pub before_rpm: u64,
    pub after_rpm: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbedChannel {
    pub pwm: u8,
    /// The firmware's duty before the test, in percent.
    pub baseline_percent: u8,
    /// The output was raised and observed.
    pub tested: bool,
    /// Why the output was not tested, or how the test ended.
    pub note: Option<String>,
    /// Fan inputs that followed this output.
    pub tachs: Vec<TachResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanMap {
    pub taken_unix: u64,
    pub channels: Vec<ProbedChannel>,
}

/// Whether `after` is a clear response to a raised duty.
pub fn responds(before: u64, after: u64) -> bool {
    after >= before + RESPONSE_MIN_RPM && after as f64 >= before as f64 * (1.0 + RESPONSE_RATIO)
}

impl ProbedChannel {
    /// A suggestion for the user, never applied on its own. `None` means nothing was seen
    /// to follow the output (nothing connected, or no tachometer on it).
    pub fn suggested_role(&self) -> Option<FanRole> {
        if !self.tested {
            return (self.baseline_percent >= FULL_SPEED_PERCENT).then_some(FanRole::Pump);
        }
        if self.tachs.is_empty() {
            return None;
        }
        if self.tachs.iter().any(|t| t.before_rpm >= PUMP_RPM_HINT) {
            Some(FanRole::Pump)
        } else {
            Some(FanRole::Fan)
        }
    }
}

impl FanMap {
    pub fn channel(&self, pwm: u8) -> Option<&ProbedChannel> {
        self.channels.iter().find(|channel| channel.pwm == pwm)
    }

    /// PWM outputs that moved fan input `tach`, in output order.
    pub fn drivers_of(&self, tach: u8) -> Vec<u8> {
        self.channels
            .iter()
            .filter(|channel| channel.tachs.iter().any(|t| t.tach == tach))
            .map(|channel| channel.pwm)
            .collect()
    }
}

/// The role of every PWM output that has one. The board profile decides first and cannot be
/// overridden; for outputs it does not describe, the user's confirmation counts. If any
/// header on an output is a pump, the output is a pump. Outputs without a role are absent:
/// nothing may drive them with a curve.
pub fn effective_roles(
    profile: Option<&BoardProfile>,
    confirmed: &BTreeMap<u8, FanRole>,
) -> BTreeMap<u8, (FanRole, RoleSource)> {
    let mut roles: BTreeMap<u8, (FanRole, RoleSource)> = confirmed
        .iter()
        .map(|(pwm, role)| (*pwm, (*role, RoleSource::User)))
        .collect();
    let mut from_board: BTreeMap<u8, FanRole> = BTreeMap::new();
    for header in profile.iter().flat_map(|p| &p.fan_headers) {
        let (Some(pwm), role) = (
            header.pwm,
            match header.kind {
                FanKind::Fan => FanRole::Fan,
                FanKind::Pump => FanRole::Pump,
                FanKind::Unknown => continue,
            },
        ) else {
            continue;
        };
        let entry = from_board.entry(pwm).or_insert(role);
        if role == FanRole::Pump {
            *entry = FanRole::Pump;
        }
    }
    for (pwm, role) in from_board {
        roles.insert(pwm, (role, RoleSource::Board));
    }
    roles
}

/// Identifies the machine and its fan chip; stable across boots and BIOS updates. Roles and
/// detection results belong to one fingerprint, saved curves do not.
pub fn fingerprint(board: Option<&BoardInfo>, controller: &str) -> String {
    let text = format!(
        "{}|{}|{controller}",
        board.map_or("", |b| b.vendor.as_str()),
        board.map_or("", |b| b.model.as_str())
    );
    let hash = text.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(pwm: u8, baseline: u8, tested: bool, tachs: &[(u8, u64, u64)]) -> ProbedChannel {
        ProbedChannel {
            pwm,
            baseline_percent: baseline,
            tested,
            note: None,
            tachs: tachs
                .iter()
                .map(|&(tach, before_rpm, after_rpm)| TachResponse {
                    tach,
                    before_rpm,
                    after_rpm,
                })
                .collect(),
        }
    }

    #[test]
    fn a_response_needs_both_a_ratio_and_an_absolute_change() {
        assert!(responds(850, 1500));
        assert!(responds(0, 900), "a stopped fan that starts");
        assert!(!responds(850, 880), "noise");
        assert!(!responds(2800, 2900), "small change on a fast input");
        assert!(!responds(850, 800), "slower is not a response");
    }

    #[test]
    fn roles_are_only_suggested_from_what_was_observed() {
        let fan = channel(4, 35, true, &[(4, 746, 1400)]);
        assert_eq!(fan.suggested_role(), Some(FanRole::Fan));
        let fast = channel(1, 50, true, &[(1, 2400, 3600)]);
        assert_eq!(fast.suggested_role(), Some(FanRole::Pump), "fast input");
        let full = channel(7, 100, false, &[]);
        assert_eq!(
            full.suggested_role(),
            Some(FanRole::Pump),
            "already at full speed"
        );
        let empty = channel(3, 67, true, &[]);
        assert_eq!(empty.suggested_role(), None, "nothing followed it");
        let skipped = channel(6, 40, false, &[]);
        assert_eq!(
            skipped.suggested_role(),
            None,
            "not tested and not at full speed"
        );
    }

    #[test]
    fn shared_outputs_are_found_from_the_inputs_that_followed() {
        let map = FanMap {
            taken_unix: 0,
            channels: vec![
                channel(2, 35, true, &[(2, 856, 1500), (5, 770, 1450)]),
                channel(4, 35, true, &[(4, 746, 1400)]),
            ],
        };
        assert_eq!(map.drivers_of(5), [2]);
        assert_eq!(map.drivers_of(4), [4]);
        assert!(map.drivers_of(9).is_empty());
        assert_eq!(map.channel(2).unwrap().tachs.len(), 2);
    }

    #[test]
    fn the_board_decides_roles_and_a_pump_wins() {
        let profile = crate::board::profiles().remove(0);
        let confirmed: BTreeMap<u8, FanRole> =
            [(7, FanRole::Fan), (1, FanRole::Pump), (9, FanRole::Fan)].into();
        let roles = effective_roles(Some(&profile), &confirmed);
        assert_eq!(
            roles[&7],
            (FanRole::Pump, RoleSource::Board),
            "the user cannot demote a pump"
        );
        assert_eq!(roles[&2], (FanRole::Fan, RoleSource::Board));
        assert_eq!(
            roles[&1],
            (FanRole::Fan, RoleSource::Board),
            "the board knows pwm1 is a fan header"
        );
        assert_eq!(
            roles[&9],
            (FanRole::Fan, RoleSource::User),
            "outputs the board does not list"
        );
        let without = effective_roles(None, &confirmed);
        assert_eq!(without[&7], (FanRole::Fan, RoleSource::User));
        assert!(
            effective_roles(None, &BTreeMap::new()).is_empty(),
            "no role, no curve"
        );
    }

    #[test]
    fn the_fingerprint_follows_board_and_chip_only() {
        let board = |model: &str| BoardInfo {
            vendor: "ASUSTeK".into(),
            model: model.into(),
            bios_version: Some("1".into()),
        };
        let a = fingerprint(Some(&board("B850-A")), "nct6799");
        assert_eq!(a, fingerprint(Some(&board("B850-A")), "nct6799"));
        let mut newer_bios = board("B850-A");
        newer_bios.bios_version = Some("2".into());
        assert_eq!(
            a,
            fingerprint(Some(&newer_bios), "nct6799"),
            "BIOS updates keep it"
        );
        assert_ne!(a, fingerprint(Some(&board("B650-E")), "nct6799"));
        assert_ne!(a, fingerprint(Some(&board("B850-A")), "it8688"));
        assert_ne!(a, fingerprint(None, "nct6799"));
    }
}

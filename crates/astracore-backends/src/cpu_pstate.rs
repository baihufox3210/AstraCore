//! CPU performance controls of the `amd-pstate-epp` cpufreq driver in active mode.
//!
//! Only governor, energy-performance preference (EPP) and boost are handled. Every value
//! is checked against the policy's `*_available_*` list before writing, read back after
//! writing, and on any failure the previous values are restored (best effort). Other
//! drivers or modes are reported as unsupported rather than guessed.
use crate::util::{invalid_data, invalid_input};
use serde::{Deserialize, Serialize};
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

pub const DRIVER: &str = "amd-pstate-epp";
/// Required value of `amd_pstate/status`.
pub const ACTIVE: &str = "active";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Governor {
    Performance,
    Powersave,
}

/// Named EPP values accepted by this backend. The kernel also lists `custom`, which it
/// rejects on write, and raw numbers; neither is exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Epp {
    Default,
    Performance,
    BalancePerformance,
    BalancePower,
    Power,
}

impl Governor {
    pub const ALL: [Self; 2] = [Self::Performance, Self::Powersave];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Powersave => "powersave",
        }
    }
}

impl Epp {
    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::Performance,
        Self::BalancePerformance,
        Self::BalancePower,
        Self::Power,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Performance => "performance",
            Self::BalancePerformance => "balance_performance",
            Self::BalancePower => "balance_power",
            Self::Power => "power",
        }
    }
}

macro_rules! sysfs_names {
    ($type:ty) => {
        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.pad(self.as_str())
            }
        }

        impl FromStr for $type {
            type Err = io::Error;
            fn from_str(value: &str) -> io::Result<Self> {
                Self::ALL
                    .into_iter()
                    .find(|item| item.as_str() == value)
                    .ok_or_else(|| invalid_input(format!("unsupported value {value:?}")))
            }
        }
    };
}
sysfs_names!(Governor);
sysfs_names!(Epp);

/// One complete CPU performance setting, applied to every online policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub governor: Governor,
    pub epp: Epp,
    pub boost: bool,
}

impl Settings {
    /// The driver forces EPP to `performance` under the performance governor and
    /// rejects any other EPP with `EBUSY`, so such a combination is refused up front.
    pub fn validate(&self) -> io::Result<()> {
        if self.governor == Governor::Performance && self.epp != Epp::Performance {
            return Err(invalid_input(
                "the performance governor only allows EPP performance",
            ));
        }
        Ok(())
    }
}

/// Observed state of one cpufreq policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PolicyState {
    pub policy: u32,
    pub path: PathBuf,
    pub cpus: Vec<u32>,
    pub driver: String,
    pub governor: String,
    pub epp: String,
    /// Per-policy boost, when the kernel exposes it.
    pub boost: Option<bool>,
    pub available_governors: Vec<String>,
    pub available_epp: Vec<String>,
}

/// Observed CPU frequency state. Policies without online CPUs are omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CpuState {
    /// `amd_pstate/status`, if present.
    pub pstate_status: Option<String>,
    /// Global `cpufreq/boost`, if present.
    pub boost: Option<bool>,
    pub policies: Vec<PolicyState>,
}

impl CpuState {
    /// `Err(reason)` unless every policy is driven by active amd-pstate-epp.
    pub fn support(&self) -> Result<(), String> {
        if self.policies.is_empty() {
            return Err("no online cpufreq policies".into());
        }
        if let Some(policy) = self.policies.iter().find(|p| p.driver != DRIVER) {
            return Err(format!(
                "policy{} uses {:?}; only {DRIVER} is supported",
                policy.policy, policy.driver
            ));
        }
        if self.pstate_status.as_deref() != Some(ACTIVE) {
            return Err(format!(
                "amd_pstate status is {:?}; only {ACTIVE:?} is supported",
                self.pstate_status.as_deref().unwrap_or("missing")
            ));
        }
        if self.boost.is_none() {
            return Err("cpufreq/boost is not available".into());
        }
        Ok(())
    }

    /// The governor shared by every policy, or `None` when they differ.
    pub fn governor(&self) -> Option<&str> {
        uniform(self.policies.iter().map(|p| p.governor.as_str()))
    }

    pub fn epp(&self) -> Option<&str> {
        uniform(self.policies.iter().map(|p| p.epp.as_str()))
    }

    /// The current state as typed settings, when it is uniform and uses known values.
    pub fn settings(&self) -> Option<Settings> {
        Some(Settings {
            governor: self.governor()?.parse().ok()?,
            epp: self.epp()?.parse().ok()?,
            boost: self.boost?,
        })
    }

    /// Check that every policy offers the requested values before anything is written.
    /// The EPP list depends on the governor (under `performance` the kernel lists only
    /// `performance`), so it is checked here only for policies already on the target
    /// governor; the others are checked again right after their governor switch.
    fn check_available(&self, settings: &Settings) -> io::Result<()> {
        for policy in &self.policies {
            let governor = settings.governor.as_str();
            if !policy.available_governors.iter().any(|g| g == governor) {
                return Err(invalid_input(format!(
                    "policy{} does not offer governor {governor}",
                    policy.policy
                )));
            }
            if policy.governor == governor {
                check_epp(policy.policy, &policy.available_epp, settings.epp)?;
            }
        }
        Ok(())
    }
}

fn check_epp(policy: u32, available: &[String], epp: Epp) -> io::Result<()> {
    if available.iter().any(|value| value == epp.as_str()) {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "policy{policy} does not offer EPP {epp}"
        )))
    }
}

fn uniform<'a>(mut values: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let first = values.next()?;
    values.all(|value| value == first).then_some(first)
}

/// Attribute access, abstracted so tests can emulate driver behavior and failures.
trait Attrs {
    fn read(&self, path: &Path) -> io::Result<String>;
    fn write(&self, path: &Path, value: &str) -> io::Result<()>;
}

struct Sysfs;

impl Attrs for Sysfs {
    fn read(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
            .map(|text| text.trim().to_owned())
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
    }

    fn write(&self, path: &Path, value: &str) -> io::Result<()> {
        fs::write(path, value)
            .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
    }
}

fn parse_bool(text: &str, path: &Path) -> io::Result<bool> {
    match text {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(invalid_data(format!(
            "{}: unexpected boost value {text:?}",
            path.display()
        ))),
    }
}

fn parse_cpus(text: &str) -> Vec<u32> {
    text.split_whitespace()
        .filter_map(|cpu| cpu.parse().ok())
        .collect()
}

fn read_policy(io: &impl Attrs, path: &Path, policy: u32) -> io::Result<PolicyState> {
    let attr = |name: &str| io.read(&path.join(name));
    let words = |text: String| text.split_whitespace().map(str::to_owned).collect();
    Ok(PolicyState {
        policy,
        path: path.to_path_buf(),
        cpus: parse_cpus(&attr("affected_cpus")?),
        driver: attr("scaling_driver")?,
        governor: attr("scaling_governor")?,
        epp: attr("energy_performance_preference")?,
        boost: match attr("boost") {
            Ok(text) => Some(parse_bool(&text, &path.join("boost"))?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        },
        available_governors: words(attr("scaling_available_governors")?),
        available_epp: words(attr("energy_performance_available_preferences")?),
    })
}

fn optional(io: &impl Attrs, path: &Path) -> io::Result<Option<String>> {
    match io.read(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn inspect_with(io: &impl Attrs, root: &Path) -> io::Result<CpuState> {
    let cpufreq = root.join("cpufreq");
    let boost_path = cpufreq.join("boost");
    let mut policies = Vec::new();
    match fs::read_dir(&cpufreq) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let Some(number) = name
                    .to_str()
                    .and_then(|name| name.strip_prefix("policy"))
                    .and_then(|number| number.parse().ok())
                else {
                    continue;
                };
                // Offline policies have no scaling attributes and no affected CPUs.
                if optional(io, &entry.path().join("affected_cpus"))?
                    .is_none_or(|cpus| parse_cpus(&cpus).is_empty())
                {
                    continue;
                }
                policies.push(read_policy(io, &entry.path(), number)?);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    policies.sort_by_key(|policy| policy.policy);
    Ok(CpuState {
        pstate_status: optional(io, &root.join("amd_pstate/status"))?,
        boost: optional(io, &boost_path)?
            .map(|text| parse_bool(&text, &boost_path))
            .transpose()?,
        policies,
    })
}

/// Read the state under `root` (normally [`astracore_discovery::sysfs::CPU`]). Never writes.
pub fn inspect(root: &Path) -> io::Result<CpuState> {
    inspect_with(&Sysfs, root)
}

/// Write `value` and require that it reads back unchanged.
fn write_checked(io: &impl Attrs, path: &Path, value: &str) -> io::Result<()> {
    io.write(path, value)?;
    let actual = io.read(path)?;
    if actual != value {
        return Err(io::Error::other(format!(
            "{}: read back {actual:?} after writing {value:?}",
            path.display()
        )));
    }
    Ok(())
}

fn bool_text(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

/// Write `value` unless the attribute already holds it.
fn ensure(io: &impl Attrs, path: &Path, value: &str) -> io::Result<()> {
    if io.read(path)? == value {
        Ok(())
    } else {
        write_checked(io, path, value)
    }
}

/// Bring every policy in `before` back to its observed governor, EPP and boost.
/// The governor is restored first because switching it changes the effective EPP.
fn restore(io: &impl Attrs, root: &Path, before: &CpuState) -> Vec<String> {
    let mut failures = Vec::new();
    for policy in &before.policies {
        for (name, value) in [
            ("scaling_governor", &policy.governor),
            ("energy_performance_preference", &policy.epp),
        ] {
            if let Err(error) = ensure(io, &policy.path.join(name), value) {
                failures.push(error.to_string());
            }
        }
    }
    if let Some(boost) = before.boost
        && let Err(error) = ensure(io, &root.join("cpufreq/boost"), bool_text(boost))
    {
        failures.push(error.to_string());
    }
    failures
}

fn apply_changes(io: &impl Attrs, root: &Path, settings: &Settings) -> io::Result<()> {
    ensure(io, &root.join("cpufreq/boost"), bool_text(settings.boost))?;
    let state = inspect_with(io, root)?;
    for policy in &state.policies {
        if policy.boost.is_some_and(|value| value != settings.boost) {
            return Err(io::Error::other(format!(
                "policy{} boost did not follow the global setting",
                policy.policy
            )));
        }
        ensure(
            io,
            &policy.path.join("scaling_governor"),
            settings.governor.as_str(),
        )?;
        // After the governor: switching it changes the effective and available EPP.
        let available = io.read(&policy.path.join("energy_performance_available_preferences"))?;
        let available: Vec<_> = available.split_whitespace().map(str::to_owned).collect();
        check_epp(policy.policy, &available, settings.epp)?;
        ensure(
            io,
            &policy.path.join("energy_performance_preference"),
            settings.epp.as_str(),
        )?;
    }
    Ok(())
}

fn restore_outcome(failures: Vec<String>) -> String {
    if failures.is_empty() {
        "previous values restored".to_owned()
    } else {
        format!("restore also failed: {}", failures.join("; "))
    }
}

fn apply_with(io: &impl Attrs, root: &Path, settings: &Settings) -> io::Result<CpuState> {
    settings.validate()?;
    let before = inspect_with(io, root)?;
    before.support().map_err(|reason| {
        io::Error::new(io::ErrorKind::Unsupported, format!("unsupported: {reason}"))
    })?;
    before.check_available(settings)?;
    if let Err(error) = apply_changes(io, root, settings) {
        let outcome = restore_outcome(restore(io, root, &before));
        return Err(io::Error::new(error.kind(), format!("{error}; {outcome}")));
    }
    let after = inspect_with(io, root)?;
    if after.settings() != Some(*settings) {
        let outcome = restore_outcome(restore(io, root, &before));
        return Err(io::Error::other(format!(
            "final state does not match the request; {outcome}"
        )));
    }
    Ok(after)
}

/// Apply `settings` to every online policy under `root`, verifying each write.
/// On failure, the previously observed values are restored before returning the error.
pub fn apply(root: &Path, settings: &Settings) -> io::Result<CpuState> {
    apply_with(&Sysfs, root, settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        /// Two online policies and one offline policy, as observed on the Ryzen 5 7500F.
        fn new() -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("astracore-cpu-test-{}-{id}", std::process::id()));
            let write = |path: &str, value: &str| {
                let path = root.join(path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, format!("{value}\n")).unwrap();
            };
            write("amd_pstate/status", "active");
            write("cpufreq/boost", "1");
            for policy in 0..2 {
                let attr = |name: &str, value: &str| {
                    write(&format!("cpufreq/policy{policy}/{name}"), value)
                };
                attr("affected_cpus", &policy.to_string());
                attr("scaling_driver", DRIVER);
                attr("scaling_governor", "powersave");
                attr("energy_performance_preference", "balance_performance");
                attr("boost", "1");
                attr("scaling_available_governors", "performance powersave");
                attr(
                    "energy_performance_available_preferences",
                    "default performance balance_performance balance_power power custom ",
                );
            }
            fs::create_dir_all(root.join("cpufreq/policy7")).unwrap();
            Self(root)
        }

        fn read(&self, path: &str) -> String {
            fs::read_to_string(self.0.join(path))
                .unwrap()
                .trim()
                .to_owned()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    /// Emulates the driver: performance forces EPP, other EPP values are then `EBUSY`,
    /// the global boost propagates to every policy, and writes can be made to fail.
    #[derive(Default)]
    struct Driver {
        fail: Option<(&'static str, &'static str)>,
        ignore: Option<(&'static str, &'static str)>,
        writes: RefCell<Vec<String>>,
    }

    impl Attrs for Driver {
        fn read(&self, path: &Path) -> io::Result<String> {
            // Observed on the 7500F: under the performance governor only `performance`
            // is listed as available.
            if path.ends_with("energy_performance_available_preferences")
                && Sysfs.read(&path.with_file_name("scaling_governor"))? == "performance"
            {
                return Ok("performance".to_owned());
            }
            Sysfs.read(path)
        }

        fn write(&self, path: &Path, value: &str) -> io::Result<()> {
            let text = path.to_string_lossy().into_owned();
            let matches = |rule: Option<(&str, &str)>| {
                rule.is_some_and(|(suffix, written)| text.ends_with(suffix) && value == written)
            };
            if matches(self.fail) {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            self.writes.borrow_mut().push(format!(
                "{}={value}",
                text.rsplit_once("cpufreq/").unwrap().1
            ));
            if matches(self.ignore) {
                return Ok(());
            }
            let parent = path.parent().unwrap();
            if text.ends_with("energy_performance_preference")
                && value != "performance"
                && Sysfs.read(&parent.join("scaling_governor"))? == "performance"
            {
                return Err(io::Error::from_raw_os_error(libc::EBUSY));
            }
            fs::write(path, value)?;
            if text.ends_with("scaling_governor") && value == "performance" {
                fs::write(parent.join("energy_performance_preference"), "performance")?;
            }
            if text.ends_with("cpufreq/boost") {
                for entry in fs::read_dir(parent)?.flatten() {
                    if entry.path().join("boost").exists() {
                        fs::write(entry.path().join("boost"), value)?;
                    }
                }
            }
            Ok(())
        }
    }

    const TURBO: Settings = Settings {
        governor: Governor::Performance,
        epp: Epp::Performance,
        boost: true,
    };
    const SILENT: Settings = Settings {
        governor: Governor::Powersave,
        epp: Epp::Power,
        boost: false,
    };

    #[test]
    fn inspects_online_policies_and_reports_support() {
        let fixture = Fixture::new();
        let state = inspect(&fixture.0).unwrap();
        assert_eq!(state.policies.len(), 2, "offline policy7 is skipped");
        assert_eq!(state.support(), Ok(()));
        assert_eq!(
            state.settings(),
            Some(Settings {
                governor: Governor::Powersave,
                epp: Epp::BalancePerformance,
                boost: true,
            })
        );
        assert!(state.policies[0].available_epp.contains(&"custom".into()));
    }

    #[test]
    fn other_drivers_and_modes_are_unsupported() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("amd_pstate/status"), "passive").unwrap();
        let driver = Driver::default();
        let error = apply_with(&driver, &fixture.0, &SILENT).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(driver.writes.borrow().is_empty());
        fs::write(fixture.0.join("amd_pstate/status"), "active").unwrap();
        fs::write(
            fixture.0.join("cpufreq/policy1/scaling_driver"),
            "acpi-cpufreq",
        )
        .unwrap();
        let reason = inspect(&fixture.0).unwrap().support().unwrap_err();
        assert!(reason.contains("acpi-cpufreq"));
        assert_eq!(
            fixture.read("cpufreq/policy0/energy_performance_preference"),
            "balance_performance"
        );
    }

    #[test]
    fn rejects_values_outside_the_whitelist() {
        assert!("custom".parse::<Epp>().is_err());
        assert!("ondemand".parse::<Governor>().is_err());
        let invalid = Settings {
            governor: Governor::Performance,
            epp: Epp::Power,
            boost: true,
        };
        assert!(invalid.validate().is_err());
        let fixture = Fixture::new();
        fs::write(
            fixture
                .0
                .join("cpufreq/policy1/energy_performance_available_preferences"),
            "default performance",
        )
        .unwrap();
        let driver = Driver::default();
        assert!(apply_with(&driver, &fixture.0, &SILENT).is_err());
        assert!(driver.writes.borrow().is_empty(), "nothing written");
    }

    #[test]
    fn applies_and_round_trips_between_profiles() {
        let fixture = Fixture::new();
        let driver = Driver::default();
        let state = apply_with(&driver, &fixture.0, &TURBO).unwrap();
        assert_eq!(state.settings(), Some(TURBO));
        assert_eq!(
            *driver.writes.borrow(),
            [
                "policy0/scaling_governor=performance",
                "policy1/scaling_governor=performance"
            ],
            "boost already on; EPP follows the performance governor"
        );
        driver.writes.borrow_mut().clear();
        let state = apply_with(&driver, &fixture.0, &SILENT).unwrap();
        assert_eq!(state.settings(), Some(SILENT));
        assert_eq!(fixture.read("cpufreq/policy1/boost"), "0");
        assert_eq!(
            *driver.writes.borrow(),
            [
                "boost=0",
                "policy0/scaling_governor=powersave",
                "policy0/energy_performance_preference=power",
                "policy1/scaling_governor=powersave",
                "policy1/energy_performance_preference=power",
            ]
        );
        driver.writes.borrow_mut().clear();
        driver.writes.borrow_mut().clear();
        apply_with(&driver, &fixture.0, &SILENT).unwrap();
        assert!(
            driver.writes.borrow().is_empty(),
            "no-op when already applied"
        );
    }

    #[test]
    fn leaves_the_performance_governor_despite_its_reduced_epp_list() {
        let fixture = Fixture::new();
        let driver = Driver::default();
        apply_with(&driver, &fixture.0, &TURBO).unwrap();
        let state = inspect_with(&driver, &fixture.0).unwrap();
        assert_eq!(state.policies[0].available_epp, ["performance"]);
        let state = apply_with(&driver, &fixture.0, &SILENT).unwrap();
        assert_eq!(state.settings(), Some(SILENT));
    }

    #[test]
    fn epp_missing_after_the_governor_switch_is_rolled_back() {
        let fixture = Fixture::new();
        let driver = Driver::default();
        apply_with(&driver, &fixture.0, &TURBO).unwrap();
        // Writes happen with the performance governor active; policy1's powersave list
        // lacks `power`, which can only be seen after its governor switch.
        fs::write(
            fixture
                .0
                .join("cpufreq/policy1/energy_performance_available_preferences"),
            "default performance",
        )
        .unwrap();
        let error = apply_with(&driver, &fixture.0, &SILENT).unwrap_err();
        assert!(
            error.to_string().contains("does not offer EPP power"),
            "{error}"
        );
        assert!(
            error.to_string().contains("previous values restored"),
            "{error}"
        );
        assert_eq!(inspect(&fixture.0).unwrap().settings(), Some(TURBO));
    }

    #[test]
    fn partial_failure_restores_previous_values() {
        let fixture = Fixture::new();
        let driver = Driver {
            fail: Some(("policy1/energy_performance_preference", "power")),
            ..Driver::default()
        };
        let error = apply_with(&driver, &fixture.0, &SILENT).unwrap_err();
        assert!(
            error.to_string().contains("previous values restored"),
            "{error}"
        );
        let state = inspect(&fixture.0).unwrap();
        assert_eq!(state.boost, Some(true));
        assert_eq!(state.epp(), Some("balance_performance"));
        assert_eq!(state.governor(), Some("powersave"));
    }

    #[test]
    fn readback_mismatch_restores_governor_before_epp() {
        let fixture = Fixture::new();
        let driver = Driver {
            ignore: Some(("policy1/scaling_governor", "performance")),
            ..Driver::default()
        };
        let error = apply_with(&driver, &fixture.0, &TURBO).unwrap_err();
        assert!(error.to_string().contains("read back"), "{error}");
        let state = inspect(&fixture.0).unwrap();
        assert_eq!(state.governor(), Some("powersave"));
        // policy0 went to performance (EPP forced); restore must rewrite its EPP too.
        assert_eq!(state.epp(), Some("balance_performance"));
        let writes = driver.writes.borrow();
        let tail: Vec<_> = writes.iter().rev().take(2).rev().collect();
        assert_eq!(
            tail,
            [
                "policy0/scaling_governor=powersave",
                "policy0/energy_performance_preference=balance_performance"
            ]
        );
    }
}

//! Read-only NVIDIA GPU readings through NVML (`libnvidia-ml.so.1`), loaded at runtime so
//! the program does not depend on the NVIDIA driver being installed. Nothing is written:
//! no fan, clock or power control functions are resolved.
//!
//! Each reading is `None` when the driver reports it as unsupported or failing. NVML
//! offers no public core-voltage query, so GPU voltage is not available.
#![allow(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::{
    ffi::{CStr, CString, c_char, c_int, c_uint, c_void},
    io,
};

pub const LIBRARY: &str = "libnvidia-ml.so.1";

type Device = *mut c_void;
type Ret = c_int;
const SUCCESS: Ret = 0;
const TEMPERATURE_GPU: c_uint = 0;
const CLOCK_GRAPHICS: c_uint = 0;
const CLOCK_MEMORY: c_uint = 2;
const NAME_BUFFER: usize = 96;
/// `NVML_PSTATE_UNKNOWN`.
const PSTATE_UNKNOWN: c_uint = 32;

#[repr(C)]
#[derive(Default)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

#[repr(C)]
#[derive(Default)]
struct FanSpeedInfo {
    version: c_uint,
    fan: c_uint,
    speed: c_uint,
}

/// `NVML_STRUCT_VERSION(FanSpeedInfo, 1)`: size in the low bits, version 1 in bit 24.
const FAN_SPEED_INFO_V1: c_uint = std::mem::size_of::<FanSpeedInfo>() as c_uint | (1 << 24);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuFan {
    pub index: u32,
    /// Current speed as reported by the driver, in percent.
    pub percent: Option<u32>,
    /// Speed the driver is asking for, in percent.
    pub target_percent: Option<u32>,
    /// Measured speed, when the driver supports it.
    pub rpm: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuReading {
    pub index: u32,
    pub name: Option<String>,
    pub temperature_celsius: Option<u32>,
    pub utilization_percent: Option<u32>,
    pub memory_utilization_percent: Option<u32>,
    pub power_watts: Option<f64>,
    pub power_limit_watts: Option<f64>,
    pub graphics_clock_mhz: Option<u32>,
    pub max_graphics_clock_mhz: Option<u32>,
    pub memory_clock_mhz: Option<u32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    /// Performance state `P0`..`P15`; lower is faster.
    pub pstate: Option<u32>,
    pub pcie_generation: Option<u32>,
    pub pcie_width: Option<u32>,
    pub fans: Vec<GpuFan>,
}

type Fn0 = unsafe extern "C" fn() -> Ret;
type FnCount = unsafe extern "C" fn(*mut c_uint) -> Ret;
type FnHandle = unsafe extern "C" fn(c_uint, *mut Device) -> Ret;
type FnName = unsafe extern "C" fn(Device, *mut c_char, c_uint) -> Ret;
type FnU32 = unsafe extern "C" fn(Device, *mut c_uint) -> Ret;
type FnArgU32 = unsafe extern "C" fn(Device, c_uint, *mut c_uint) -> Ret;
type FnUtilization = unsafe extern "C" fn(Device, *mut Utilization) -> Ret;
type FnMemory = unsafe extern "C" fn(Device, *mut Memory) -> Ret;
type FnFanRpm = unsafe extern "C" fn(Device, *mut FanSpeedInfo) -> Ret;

struct Api {
    shutdown: Fn0,
    count: FnCount,
    handle: FnHandle,
    name: FnName,
    temperature: FnArgU32,
    utilization: FnUtilization,
    power: FnU32,
    power_limit: FnU32,
    clock: FnArgU32,
    max_clock: FnArgU32,
    memory: FnMemory,
    pstate: FnU32,
    pcie_generation: FnU32,
    pcie_width: FnU32,
    num_fans: FnU32,
    fan_speed: FnArgU32,
    target_fan_speed: Option<FnArgU32>,
    fan_rpm: Option<FnFanRpm>,
}

/// An initialized NVML session. Dropping it shuts NVML down and unloads the library.
pub struct Nvml {
    library: *mut c_void,
    api: Api,
}

// SAFETY: NVML is documented as thread-safe; the handle and function pointers are
// process-wide and only used through `&self`.
unsafe impl Send for Nvml {}

fn unavailable(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message.into())
}

/// Resolve `name` in `library` as a function pointer of type `T`.
///
/// # Safety
/// `T` must be the exact C signature of the symbol.
unsafe fn symbol<T: Copy>(library: *mut c_void, name: &str) -> Option<T> {
    let name = CString::new(name).ok()?;
    // SAFETY: library is a live dlopen handle and name is NUL-terminated.
    let pointer = unsafe { libc::dlsym(library, name.as_ptr()) };
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees T matches the symbol; T is a function pointer of
    // the same size as a data pointer on supported platforms.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&pointer) })
}

impl Nvml {
    /// Load NVML and initialize it. Fails with `Unsupported` when the driver is absent.
    pub fn open() -> io::Result<Self> {
        Self::open_library(LIBRARY)
    }

    fn open_library(path: &str) -> io::Result<Self> {
        let name = CString::new(path).map_err(|_| unavailable("invalid library name"))?;
        // SAFETY: name is NUL-terminated; flags are valid dlopen flags.
        let library = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if library.is_null() {
            return Err(unavailable(format!("{path} is not available")));
        }
        let close = |error: io::Error| {
            // SAFETY: library is the handle returned above and is not used afterwards.
            unsafe { libc::dlclose(library) };
            error
        };
        macro_rules! required {
            ($name:literal) => {
                // SAFETY: each binding below names the NVML signature it is used with.
                match unsafe { symbol(library, $name) } {
                    Some(function) => function,
                    None => return Err(close(unavailable(concat!("NVML lacks ", $name)))),
                }
            };
        }
        let init: Fn0 = required!("nvmlInit_v2");
        let api = Api {
            shutdown: required!("nvmlShutdown"),
            count: required!("nvmlDeviceGetCount_v2"),
            handle: required!("nvmlDeviceGetHandleByIndex_v2"),
            name: required!("nvmlDeviceGetName"),
            temperature: required!("nvmlDeviceGetTemperature"),
            utilization: required!("nvmlDeviceGetUtilizationRates"),
            power: required!("nvmlDeviceGetPowerUsage"),
            power_limit: required!("nvmlDeviceGetEnforcedPowerLimit"),
            clock: required!("nvmlDeviceGetClockInfo"),
            max_clock: required!("nvmlDeviceGetMaxClockInfo"),
            memory: required!("nvmlDeviceGetMemoryInfo"),
            pstate: required!("nvmlDeviceGetPerformanceState"),
            pcie_generation: required!("nvmlDeviceGetCurrPcieLinkGeneration"),
            pcie_width: required!("nvmlDeviceGetCurrPcieLinkWidth"),
            num_fans: required!("nvmlDeviceGetNumFans"),
            fan_speed: required!("nvmlDeviceGetFanSpeed_v2"),
            // SAFETY: optional symbols with their documented signatures.
            target_fan_speed: unsafe { symbol(library, "nvmlDeviceGetTargetFanSpeed") },
            fan_rpm: unsafe { symbol(library, "nvmlDeviceGetFanSpeedRPM") },
        };
        // SAFETY: nvmlInit_v2 takes no arguments and may be called once per session.
        let status = unsafe { init() };
        if status != SUCCESS {
            return Err(close(unavailable(format!(
                "nvmlInit_v2 failed with code {status}"
            ))));
        }
        Ok(Self { library, api })
    }

    /// Readings for every GPU; an empty list when there is none.
    pub fn read(&self) -> Vec<GpuReading> {
        let mut count: c_uint = 0;
        // SAFETY: count points to a valid c_uint.
        if unsafe { (self.api.count)(&mut count) } != SUCCESS {
            return Vec::new();
        }
        (0..count)
            .filter_map(|index| self.read_device(index))
            .collect()
    }

    fn read_device(&self, index: u32) -> Option<GpuReading> {
        let mut device: Device = std::ptr::null_mut();
        // SAFETY: index is below the reported count; device is a valid out pointer.
        if unsafe { (self.api.handle)(index, &mut device) } != SUCCESS {
            return None;
        }
        let api = &self.api;
        let u32_of = |function: FnU32| {
            let mut value: c_uint = 0;
            // SAFETY: device is a live handle; value is a valid out pointer.
            (unsafe { function(device, &mut value) } == SUCCESS).then_some(value)
        };
        let arg_of = |function: FnArgU32, argument: c_uint| {
            let mut value: c_uint = 0;
            // SAFETY: as above, with a documented enum or fan index argument.
            (unsafe { function(device, argument, &mut value) } == SUCCESS).then_some(value)
        };
        let name = {
            let mut buffer = [0 as c_char; NAME_BUFFER];
            // SAFETY: the buffer length is passed so NVML cannot overrun it.
            (unsafe { (api.name)(device, buffer.as_mut_ptr(), NAME_BUFFER as c_uint) } == SUCCESS)
                .then(|| {
                    // SAFETY: NVML NUL-terminates within the given length.
                    unsafe { CStr::from_ptr(buffer.as_ptr()) }
                        .to_string_lossy()
                        .into_owned()
                })
        };
        let mut utilization = Utilization::default();
        // SAFETY: utilization is a valid out pointer of the documented layout.
        let utilization = (unsafe { (api.utilization)(device, &mut utilization) } == SUCCESS)
            .then_some(utilization);
        let mut memory = Memory::default();
        // SAFETY: memory is a valid out pointer of the v1 layout.
        let memory = (unsafe { (api.memory)(device, &mut memory) } == SUCCESS).then_some(memory);
        let fans = u32_of(api.num_fans).unwrap_or(0);
        let fans = (0..fans)
            .map(|fan| GpuFan {
                index: fan,
                percent: arg_of(api.fan_speed, fan),
                target_percent: api.target_fan_speed.and_then(|f| arg_of(f, fan)),
                rpm: api.fan_rpm.and_then(|function| {
                    let mut info = FanSpeedInfo {
                        version: FAN_SPEED_INFO_V1,
                        fan,
                        speed: 0,
                    };
                    // SAFETY: info is a versioned v1 struct as NVML expects.
                    (unsafe { function(device, &mut info) } == SUCCESS).then_some(info.speed)
                }),
            })
            .collect();
        let watts = |milliwatts: Option<u32>| milliwatts.map(|mw| f64::from(mw) / 1000.0);
        Some(GpuReading {
            index,
            name,
            temperature_celsius: arg_of(api.temperature, TEMPERATURE_GPU),
            utilization_percent: utilization.as_ref().map(|u| u.gpu),
            memory_utilization_percent: utilization.as_ref().map(|u| u.memory),
            power_watts: watts(u32_of(api.power)),
            power_limit_watts: watts(u32_of(api.power_limit)),
            graphics_clock_mhz: arg_of(api.clock, CLOCK_GRAPHICS),
            max_graphics_clock_mhz: arg_of(api.max_clock, CLOCK_GRAPHICS),
            memory_clock_mhz: arg_of(api.clock, CLOCK_MEMORY),
            memory_used_bytes: memory.as_ref().map(|m| m.used),
            memory_total_bytes: memory.as_ref().map(|m| m.total),
            pstate: u32_of(api.pstate).filter(|state| *state != PSTATE_UNKNOWN),
            pcie_generation: u32_of(api.pcie_generation),
            pcie_width: u32_of(api.pcie_width),
            fans,
        })
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: NVML was initialized in open; the library handle is still live and is
        // not used after this point.
        unsafe {
            (self.api.shutdown)();
            libc::dlclose(self.library);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_nvml() {
        assert_eq!(std::mem::size_of::<Utilization>(), 8);
        assert_eq!(std::mem::size_of::<Memory>(), 24);
        assert_eq!(std::mem::size_of::<FanSpeedInfo>(), 12);
        assert_eq!(FAN_SPEED_INFO_V1, 0x0100_000c);
    }

    #[test]
    fn missing_library_is_unsupported() {
        let error = Nvml::open_library("libastracore-test-missing.so.1")
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn readings_serialize_with_explicit_nulls() {
        let reading = GpuReading {
            index: 0,
            name: Some("GPU".into()),
            temperature_celsius: None,
            utilization_percent: Some(5),
            memory_utilization_percent: None,
            power_watts: Some(47.5),
            power_limit_watts: None,
            graphics_clock_mhz: None,
            max_graphics_clock_mhz: None,
            memory_clock_mhz: None,
            memory_used_bytes: None,
            memory_total_bytes: None,
            pstate: Some(3),
            pcie_generation: None,
            pcie_width: None,
            fans: vec![GpuFan {
                index: 0,
                percent: Some(53),
                target_percent: None,
                rpm: Some(0),
            }],
        };
        let value = serde_json::to_value(&reading).unwrap();
        assert!(value["temperature_celsius"].is_null());
        assert_eq!(value["fans"][0]["rpm"], 0);
    }
}

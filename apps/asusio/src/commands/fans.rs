//! nct6799 fan PWM commands. Writes require `--execute`.
use crate::{
    cli::FanAction,
    output::{CliResult, preview_only, print_json, print_warnings},
};
use asusio_backends::fan_hwmon;
use asusio_discovery::sysfs;
use serde_json::json;
use std::path::Path;

pub fn run(action: FanAction) -> CliResult {
    let root = Path::new(sysfs::HWMON);
    match action {
        FanAction::Presets { socket, json } => return super::fan_presets::list(&socket, json),
        FanAction::Preset { socket, action } => return super::fan_presets::run(&socket, action),
        FanAction::Detect {
            socket,
            execute,
            json,
        } => return super::fan_presets::detect(&socket, execute, json),
        FanAction::Role { socket, pwm, role } => {
            return super::fan_presets::role(&socket, pwm, role);
        }
        FanAction::Reset {
            socket,
            execute,
            everything,
        } => return super::fan_presets::reset(&socket, execute, everything),
        FanAction::List { json } => {
            let report = fan_hwmon::discover_fan_controllers(root)?;
            if json {
                return print_json(json!({"devices": report.devices, "warnings": report.warnings}));
            }
            for controller in &report.devices {
                println!("{} ({})", controller.name, controller.sysfs_path.display());
                for fan in &controller.fan_speeds {
                    println!("  fan{}: {} RPM", fan.channel, fan.rpm);
                }
                for pwm in &controller.pwm_channels {
                    let mode = pwm
                        .mode
                        .map_or_else(|| "unknown".to_owned(), |mode| mode.to_string());
                    println!(
                        "  pwm{}: raw={} enable={} mode={mode}",
                        pwm.channel, pwm.duty, pwm.enable
                    );
                }
            }
            print_warnings(&report.warnings);
        }
        FanAction::Set {
            channel,
            percent,
            execute,
        } => {
            fan_hwmon::manual_duty(percent)?;
            fan_hwmon::validate_channel(root, channel)?;
            println!("nct6799 pwm{channel}: manual {percent}%");
            if execute {
                fan_hwmon::set_manual(root, channel, percent)?;
                println!("PWM duty applied");
            } else {
                preview_only("change the fan");
            }
        }
        FanAction::Auto { channel, execute } => {
            fan_hwmon::validate_channel(root, channel)?;
            println!("nct6799 pwm{channel}: firmware Smart Fan IV (enable=5)");
            if execute {
                fan_hwmon::restore_firmware_auto(root, channel, None)?;
                println!("Firmware automatic control restored");
            } else {
                preview_only("change the fan");
            }
        }
    }
    Ok(())
}

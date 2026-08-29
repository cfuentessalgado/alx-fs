use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

#[cfg(target_os = "macos")]
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use directories::BaseDirs;

use crate::ServerEndpoint;

pub const SERVICE_LABEL: &str = "com.alx.serve";

#[derive(Clone, Debug, Default)]
pub struct InstallOptions {
    pub bind: Option<String>,
    pub tailscale: bool,
}

fn executable_args(options: &InstallOptions) -> Vec<String> {
    let mut args = vec!["serve".to_owned()];
    if let Some(bind) = &options.bind {
        args.extend(["--bind".to_owned(), bind.clone()]);
    } else if options.tailscale {
        args.push("--tailscale".to_owned());
    }
    args
}

fn service_environment() -> Vec<(String, String)> {
    ["ALX_DB", "ALX_AUTH_FILE"]
        .into_iter()
        .filter_map(|name| env::var(name).ok().map(|value| (name.to_owned(), value)))
        .collect()
}

pub fn install(options: &InstallOptions) -> Result<()> {
    let executable = env::current_exe().context("failed to find the alx executable")?;
    install_with_executable(&executable, options)
}

fn install_with_executable(executable: &Path, options: &InstallOptions) -> Result<()> {
    let path = service_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create service directory {}", parent.display()))?;
    }
    let environment = service_environment();
    let contents = service_file_contents(executable, options, &environment)?;
    fs::write(&path, contents)
        .with_context(|| format!("failed to write service file {}", path.display()))?;

    #[cfg(target_os = "macos")]
    {
        // Replacing an existing agent makes repeated installs deterministic.
        let _ = launchctl(&["bootout", &domain_label()?]);
        let service_path = path
            .to_str()
            .ok_or_else(|| anyhow!("service path is not valid UTF-8"))?;
        run_command("launchctl", &["bootstrap", &domain(), service_path])?;
        run_command("launchctl", &["enable", &domain_label()?])?;
        run_command("launchctl", &["kickstart", "-k", &domain_label()?])?;
    }
    #[cfg(target_os = "linux")]
    {
        run_command("systemctl", &["--user", "daemon-reload"])?;
        run_command("systemctl", &["--user", "enable", "--now", "alx.service"])?;
        run_command("systemctl", &["--user", "restart", "alx.service"])?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        bail!("native alx services are supported only on macOS and Linux");
    }

    eprintln!("installed alx service at {}", path.display());
    Ok(())
}

pub fn status() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        run_command("launchctl", &["print", &domain_label()?])
    }
    #[cfg(target_os = "linux")]
    {
        run_command(
            "systemctl",
            &["--user", "status", "alx.service", "--no-pager"],
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    bail!("native alx services are supported only on macOS and Linux")
}

pub fn restart() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        run_command("launchctl", &["kickstart", "-k", &domain_label()?])
    }
    #[cfg(target_os = "linux")]
    {
        run_command("systemctl", &["--user", "restart", "alx.service"])
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    bail!("native alx services are supported only on macOS and Linux")
}

pub fn uninstall() -> Result<()> {
    let path = service_file_path()?;
    #[cfg(target_os = "macos")]
    {
        let _ = launchctl(&["bootout", &domain_label()?]);
    }
    #[cfg(target_os = "linux")]
    {
        let _ = run_command("systemctl", &["--user", "disable", "--now", "alx.service"]);
        run_command("systemctl", &["--user", "daemon-reload"])?;
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    bail!("native alx services are supported only on macOS and Linux");

    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to remove service file {}", path.display()))?;
    }
    eprintln!("uninstalled alx service");
    Ok(())
}

/// Resolve the endpoint configured for the installed service, if one exists.
///
/// The service file is the source of truth. Tailscale addresses are resolved each
/// time because they can change between service installation and use.
pub fn configured_endpoint() -> Result<Option<ServerEndpoint>> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let path = service_file_path()?;
        if !path.exists() {
            return Ok(None);
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read service file {}", path.display()))?;
        let address = if contains_argument(&contents, "--tailscale") {
            crate::parse_bind_address(None, true)?
        } else if let Some(bind) = argument_value(&contents, "--bind") {
            crate::parse_bind_address(Some(&bind), false)?
        } else {
            crate::parse_bind_address(None, false)?
        };
        Ok(Some(ServerEndpoint {
            address,
            scheme: "http",
            requires_auth: !address.ip().is_loopback(),
        }))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Ok(None)
}

fn contains_argument(contents: &str, argument: &str) -> bool {
    contents.contains(&format!("<string>{argument}</string>"))
        || contents.contains(&format!("\"{argument}\""))
}

fn argument_value(contents: &str, argument: &str) -> Option<String> {
    let xml_marker = format!("<string>{argument}</string>");
    if let Some(position) = contents.find(&xml_marker) {
        let remainder = &contents[position + xml_marker.len()..];
        let start = remainder.find("<string>")? + "<string>".len();
        let value = &remainder[start..];
        return Some(value.split_once("</string>")?.0.to_owned());
    }

    let marker = format!("\"{argument}\"");
    let position = contents.find(&marker)?;
    let remainder = &contents[position + marker.len()..];
    let start = remainder.find('"')? + 1;
    let value = &remainder[start..];
    Some(value.split_once('"')?.0.to_owned())
}

pub fn service_file_path() -> Result<PathBuf> {
    let dirs =
        BaseDirs::new().ok_or_else(|| anyhow!("could not determine the user home directory"))?;
    #[cfg(target_os = "macos")]
    {
        Ok(dirs
            .home_dir()
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{SERVICE_LABEL}.plist")))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(dirs
            .config_dir()
            .join("systemd")
            .join("user")
            .join("alx.service"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = dirs;
        bail!("native alx services are supported only on macOS and Linux")
    }
}

fn service_file_contents(
    executable: &Path,
    options: &InstallOptions,
    environment: &[(String, String)],
) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        Ok(launchd_plist(executable, options, environment))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(systemd_unit(executable, options, environment))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (executable, options, environment);
        bail!("native alx services are supported only on macOS and Linux")
    }
}

/// Generate a user-level systemd unit without putting the password in the unit.
#[cfg(target_os = "linux")]
pub fn systemd_unit(
    executable: &Path,
    options: &InstallOptions,
    environment: &[(String, String)],
) -> String {
    let mut command = vec![systemd_quote(executable.to_string_lossy().as_ref())];
    command.extend(
        executable_args(options)
            .iter()
            .map(|argument| systemd_quote(argument)),
    );
    let mut output =
        String::from("[Unit]\nDescription=alx web service\nAfter=default.target\n\n[Service]\n");
    output.push_str("ExecStart=");
    output.push_str(&command.join(" "));
    output.push_str("\nRestart=on-failure\nRestartSec=2\n");
    for (name, value) in environment {
        output.push_str("Environment=");
        output.push_str(&systemd_quote(&format!("{name}={value}")));
        output.push('\n');
    }
    output.push_str("\n[Install]\nWantedBy=default.target\n");
    output
}

#[cfg(target_os = "linux")]
fn systemd_quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '$' => quoted.push_str("$$"),
            '%' => quoted.push_str("%%"),
            character if character.is_control() => {
                quoted.push_str(&format!("\\x{:02x}", character as u32));
            }
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// Generate a launchd user agent without putting the password in the plist.
#[cfg(target_os = "macos")]
pub fn launchd_plist(
    executable: &Path,
    options: &InstallOptions,
    environment: &[(String, String)],
) -> String {
    let mut arguments = vec![executable.to_string_lossy().into_owned()];
    arguments.extend(executable_args(options));
    let mut output = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n",
    );
    output.push_str("  <key>Label</key>\n  <string>");
    output.push_str(&xml_escape(SERVICE_LABEL));
    output.push_str("</string>\n  <key>ProgramArguments</key>\n  <array>\n");
    for argument in arguments {
        output.push_str("    <string>");
        output.push_str(&xml_escape(&argument));
        output.push_str("</string>\n");
    }
    output.push_str(
        "  </array>\n  <key>RunAtLoad</key>\n  <true/>\n  <key>KeepAlive</key>\n  <true/>\n",
    );
    if !environment.is_empty() {
        output.push_str("  <key>EnvironmentVariables</key>\n  <dict>\n");
        for (name, value) in environment {
            output.push_str("    <key>");
            output.push_str(&xml_escape(name));
            output.push_str("</key>\n    <string>");
            output.push_str(&xml_escape(value));
            output.push_str("</string>\n");
        }
        output.push_str("  </dict>\n");
    }
    output.push_str("</dict>\n</plist>\n");
    output
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
fn domain_label() -> Result<String> {
    Ok(format!("{}/{}", domain(), SERVICE_LABEL))
}

#[cfg(target_os = "macos")]
fn launchctl(arguments: &[&str]) -> Result<()> {
    let mut command = Command::new("launchctl");
    command
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = command.status();
    Ok(())
}

fn run_command(program: &str, arguments: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(arguments)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} {} failed with {status}", arguments.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{argument_value, contains_argument};

    #[test]
    fn reads_tailscale_and_bind_arguments_from_service_formats() {
        let plist = "<string>serve</string><string>--tailscale</string>";
        assert!(contains_argument(plist, "--tailscale"));

        let unit = "ExecStart=\"/usr/bin/alx\" \"serve\" \"--bind\" \"192.0.2.10:3000\"";
        assert_eq!(
            argument_value(unit, "--bind"),
            Some("192.0.2.10:3000".to_owned())
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;

#[cfg(target_os = "macos")]
use alx::service::{InstallOptions, launchd_plist};
#[cfg(target_os = "linux")]
use alx::service::{InstallOptions, systemd_unit};

#[cfg(target_os = "linux")]
#[test]
fn systemd_unit_runs_normal_server_without_secrets() {
    let unit = systemd_unit(
        Path::new("/home/example/bin/alx"),
        &InstallOptions {
            bind: Some("192.168.1.10:3000".to_owned()),
            tailscale: false,
        },
        &[("ALX_DB".to_owned(), "/home/example/alx.db".to_owned())],
    );

    assert!(unit.contains(
        "ExecStart=\"/home/example/bin/alx\" \"serve\" \"--bind\" \"192.168.1.10:3000\""
    ));
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("WantedBy=default.target"));
    assert!(unit.contains("Environment=\"ALX_DB=/home/example/alx.db\""));
    assert!(!unit.contains("password"));
}

#[cfg(target_os = "macos")]
#[test]
fn launchd_plist_runs_normal_server_without_secrets() {
    let plist = launchd_plist(
        Path::new("/Users/example/bin/alx"),
        &InstallOptions {
            bind: Some("192.168.1.10:3000".to_owned()),
            tailscale: false,
        },
        &[],
    );
    assert!(plist.contains("<string>/Users/example/bin/alx</string>"));
    assert!(plist.contains("<string>serve</string>"));
    assert!(plist.contains("<string>--bind</string>"));
    assert!(plist.contains("<string>192.168.1.10:3000</string>"));
    assert!(plist.contains("<key>RunAtLoad</key>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(!plist.contains("password"));
}

#[cfg(target_os = "linux")]
#[test]
fn systemd_unit_defers_tailscale_lookup_to_server_start() {
    let unit = systemd_unit(
        Path::new("/home/example/bin/alx"),
        &InstallOptions {
            bind: None,
            tailscale: true,
        },
        &[],
    );
    assert!(unit.contains("\"serve\" \"--tailscale\""));
    assert!(!unit.contains("tailscale ip"));
}

//! OS user-service installation and lifecycle management.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;

use crate::state::{Dirs, create_private_dir, write_private_atomic};
use std::os::unix::fs::PermissionsExt as _;

const LAUNCHD_LABEL: &str = "com.alignment-hive.model-router";
const SYSTEMD_UNIT: &str = "model-router.service";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Platform {
    MacOs,
    Linux,
}

impl Platform {
    fn current() -> anyhow::Result<Self> {
        match std::env::consts::OS {
            "macos" => Ok(Self::MacOs),
            "linux" => Ok(Self::Linux),
            other => anyhow::bail!(
                "unsupported platform `{other}`: model-router services require macOS or Linux"
            ),
        }
    }
}

#[derive(Debug)]
struct LauncherSources {
    bootstrap: PathBuf,
    version: PathBuf,
}

impl LauncherSources {
    /// The files `bootstrap.sh` exports before exec'ing the binary.
    fn from_environment() -> anyhow::Result<Option<Self>> {
        let bootstrap = std::env::var_os("MODEL_ROUTER_BOOTSTRAP_SCRIPT");
        let version = std::env::var_os("MODEL_ROUTER_VERSION_FILE");
        match (bootstrap, version) {
            (Some(bootstrap), Some(version)) => Ok(Some(Self {
                bootstrap: bootstrap.into(),
                version: version.into(),
            })),
            (None, None) => Ok(None),
            _ => anyhow::bail!(
                "launcher sources are incomplete: set both MODEL_ROUTER_BOOTSTRAP_SCRIPT and \
                 MODEL_ROUTER_VERSION_FILE (bootstrap.sh exports them)"
            ),
        }
    }
}

#[derive(Debug)]
struct CommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

trait CommandRunner {
    fn output(&self, program: &str, args: &[OsString]) -> anyhow::Result<CommandOutput>;
}

struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn output(&self, program: &str, args: &[OsString]) -> anyhow::Result<CommandOutput> {
        let output = Command::new(program)
            .args(args)
            .output()
            .with_context(|| format!("failed to run {program}"))?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Installs and starts the current platform's user service.
///
/// Launcher sources come from the environment exported by `bootstrap.sh`;
/// an already-complete stable launcher is reused when they are absent.
///
/// # Errors
/// Returns an actionable error for unsupported platforms, missing launcher
/// sources, filesystem failures, or service-manager failures.
pub fn install(dirs: &Dirs) -> anyhow::Result<()> {
    let platform = Platform::current()?;
    let unit_path = unit_path(platform)?;
    let sources = LauncherSources::from_environment()?;
    install_at(
        dirs,
        platform,
        &unit_path,
        sources.as_ref(),
        &SystemCommandRunner,
    )?;
    println!(
        "Installed and started model-router service (unit: {}).",
        unit_path.display()
    );
    Ok(())
}

/// Refreshes the stable launcher from the current plugin and restarts the
/// installed service.
///
/// # Errors
/// Returns an actionable error when sources are unavailable, copying fails,
/// the platform is unsupported, or the service manager cannot restart.
pub fn refresh(dirs: &Dirs) -> anyhow::Result<()> {
    let platform = Platform::current()?;
    let sources = LauncherSources::from_environment()?.ok_or_else(|| {
        anyhow::anyhow!(
            "service refresh requires launcher sources: run it through bootstrap.sh, which \
             exports MODEL_ROUTER_BOOTSTRAP_SCRIPT and MODEL_ROUTER_VERSION_FILE"
        )
    })?;
    let unit_path = unit_path(platform)?;
    let version = refresh_at(dirs, platform, &unit_path, &sources, &SystemCommandRunner)?;
    println!("Refreshed launcher to {version} and restarted model-router service.");
    Ok(())
}

/// Restarts the current platform's installed user service.
///
/// # Errors
/// Returns an actionable error for unsupported platforms or service-manager
/// failures.
pub fn restart() -> anyhow::Result<()> {
    restart_with(Platform::current()?, &SystemCommandRunner)?;
    println!("Restarted model-router service.");
    Ok(())
}

/// Prints a concise service, unit-file, and launcher-version summary.
///
/// A stopped or unloaded service is reported normally rather than treated as
/// a command error.
///
/// # Errors
/// Returns an actionable error for unsupported platforms, service-manager
/// invocation failures, or unreadable launcher metadata.
pub fn status(dirs: &Dirs) -> anyhow::Result<()> {
    let platform = Platform::current()?;
    let unit_path = unit_path(platform)?;
    let state = status_with(platform, &SystemCommandRunner)?;
    println!("Service: {state}");
    println!(
        "Unit file: {} ({})",
        if unit_path.exists() {
            "present"
        } else {
            "absent"
        },
        unit_path.display()
    );
    if let Some(version) = launcher_version(dirs)? {
        println!("Launcher version: {version}");
    }
    Ok(())
}

/// Stops and removes the current platform's user service unit.
///
/// Launcher, configuration, state, logs, and caches are deliberately retained.
///
/// # Errors
/// Returns an actionable error for unsupported platforms, filesystem failures,
/// or service-manager invocation failures.
pub fn uninstall() -> anyhow::Result<()> {
    let platform = Platform::current()?;
    let unit_path = unit_path(platform)?;
    uninstall_at(platform, &unit_path, &SystemCommandRunner)?;
    println!("Stopped and removed model-router service unit; router data was retained.");
    Ok(())
}

fn unit_path(platform: Platform) -> anyhow::Result<PathBuf> {
    let home = crate::state::home_dir()
        .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot locate the user service unit"))?;
    Ok(match platform {
        Platform::MacOs => home
            .join("Library/LaunchAgents")
            .join(format!("{LAUNCHD_LABEL}.plist")),
        Platform::Linux => home.join(".config/systemd/user").join(SYSTEMD_UNIT),
    })
}

fn install_at(
    dirs: &Dirs,
    platform: Platform,
    unit_path: &Path,
    sources: Option<&LauncherSources>,
    runner: &dyn CommandRunner,
) -> anyhow::Result<()> {
    populate_launcher(dirs, sources)?;
    create_private_dir(&dirs.log_dir())?;
    write_unit_contents(unit_path, unit_contents(dirs, platform).as_bytes())?;
    load_unit(platform, unit_path, runner)?;
    if platform == Platform::Linux {
        require_success(
            &runner.output(
                "systemctl",
                &os_args(["--user", "enable", "--now", SYSTEMD_UNIT]),
            )?,
            "systemctl --user enable --now",
        )?;
    }
    Ok(())
}

/// Makes the service manager read the unit at `unit_path`: launchd has to
/// unload and reload it, systemd only reloads its unit files.
fn load_unit(
    platform: Platform,
    unit_path: &Path,
    runner: &dyn CommandRunner,
) -> anyhow::Result<()> {
    match platform {
        Platform::MacOs => {
            let _ = runner.output("launchctl", &os_args(["bootout", &launchd_service()]));
            require_success(
                &runner.output(
                    "launchctl",
                    &[
                        OsString::from("bootstrap"),
                        OsString::from(launchd_domain()),
                        unit_path.as_os_str().to_owned(),
                    ],
                )?,
                "launchctl bootstrap",
            )
        }
        Platform::Linux => require_success(
            &runner.output("systemctl", &os_args(["--user", "daemon-reload"]))?,
            "systemctl --user daemon-reload",
        ),
    }
}

/// Copies the launcher pair into the stable launcher dir and returns the
/// version written. Without sources, an already-complete launcher is kept.
fn populate_launcher(dirs: &Dirs, sources: Option<&LauncherSources>) -> anyhow::Result<String> {
    let launcher_dir = dirs.launcher_dir();
    let destination_bootstrap = launcher_dir.join("bootstrap.sh");
    let destination_version = dirs.launcher_version_file();

    let Some(sources) = sources else {
        if destination_bootstrap.is_file()
            && let Some(version) = launcher_version(dirs)?
        {
            return Ok(version);
        }
        anyhow::bail!(
            "stable launcher is not populated at {}; run this through bootstrap.sh, which \
             exports MODEL_ROUTER_BOOTSTRAP_SCRIPT and MODEL_ROUTER_VERSION_FILE",
            launcher_dir.display()
        );
    };

    // Read both first so a missing source cannot leave a half-refreshed pair.
    let bootstrap = fs::read(&sources.bootstrap).with_context(|| {
        format!(
            "failed to read bootstrap script {}",
            sources.bootstrap.display()
        )
    })?;
    let version = fs::read_to_string(&sources.version)
        .with_context(|| format!("failed to read version file {}", sources.version.display()))?;
    anyhow::ensure!(
        !version.trim().is_empty(),
        "version file {} is empty",
        sources.version.display()
    );

    create_private_dir(&launcher_dir)?;
    write_private_atomic(&destination_bootstrap, &bootstrap)?;
    fs::set_permissions(&destination_bootstrap, fs::Permissions::from_mode(0o755)).with_context(
        || {
            format!(
                "failed to make launcher executable {}",
                destination_bootstrap.display()
            )
        },
    )?;
    write_private_atomic(&destination_version, version.as_bytes())?;
    Ok(version.trim().to_string())
}

/// Returns the version the launcher now runs.
fn refresh_at(
    dirs: &Dirs,
    platform: Platform,
    unit_path: &Path,
    sources: &LauncherSources,
    runner: &dyn CommandRunner,
) -> anyhow::Result<String> {
    prefetch_binary(sources, runner)?;
    let version = populate_launcher(dirs, Some(sources))?;
    refresh_unit_if_stale(dirs, platform, unit_path, runner)?;
    restart_with(platform, runner)?;
    Ok(version)
}

/// Downloads the new version's binary via the source bootstrap's `prefetch`
/// mode before the launcher is switched or the service restarted, so a
/// version whose release is not yet published (the plugin update can land
/// minutes before the release assets) aborts the refresh while the current
/// service keeps serving.
fn prefetch_binary(sources: &LauncherSources, runner: &dyn CommandRunner) -> anyhow::Result<()> {
    require_success(
        &runner.output(
            "bash",
            &[
                sources.bootstrap.as_os_str().to_owned(),
                OsString::from("prefetch"),
            ],
        )?,
        "binary prefetch (the release may still be building; retried next session)",
    )
}

fn refresh_unit_if_stale(
    dirs: &Dirs,
    platform: Platform,
    path: &Path,
    runner: &dyn CommandRunner,
) -> anyhow::Result<()> {
    let contents = unit_contents(dirs, platform);
    let current = match fs::read(path) {
        Ok(current) => Some(current),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read service unit {}", path.display()));
        }
    };
    if current.as_deref() == Some(contents.as_bytes()) {
        return Ok(());
    }

    write_unit_contents(path, contents.as_bytes())?;
    load_unit(platform, path, runner)
}

fn unit_contents(dirs: &Dirs, platform: Platform) -> String {
    match platform {
        Platform::MacOs => launchd_plist(dirs),
        Platform::Linux => systemd_user_unit(dirs),
    }
}

/// The unit's parent (`~/Library/LaunchAgents`, `~/.config/systemd/user`)
/// is shared with other tools' units, so it is created but never chmod'd.
fn write_unit_contents(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("unit path {} has no parent", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    write_private_atomic(path, contents)
        .with_context(|| format!("failed to write service unit {}", path.display()))
}

fn restart_with(platform: Platform, runner: &dyn CommandRunner) -> anyhow::Result<()> {
    match platform {
        Platform::MacOs => require_success(
            &runner.output(
                "launchctl",
                &os_args(["kickstart", "-k", &launchd_service()]),
            )?,
            "launchctl kickstart",
        ),
        Platform::Linux => require_success(
            &runner.output("systemctl", &os_args(["--user", "restart", SYSTEMD_UNIT]))?,
            "systemctl --user restart",
        ),
    }
}

fn status_with(platform: Platform, runner: &dyn CommandRunner) -> anyhow::Result<String> {
    Ok(match platform {
        Platform::MacOs => {
            let output = runner.output("launchctl", &os_args(["print", &launchd_service()]))?;
            if output.success {
                "loaded"
            } else {
                "not loaded"
            }
            .to_string()
        }
        Platform::Linux => {
            let output =
                runner.output("systemctl", &os_args(["--user", "is-active", SYSTEMD_UNIT]))?;
            let state = output.stdout.trim();
            if state.is_empty() {
                if output.success { "active" } else { "inactive" }.to_string()
            } else {
                state.to_string()
            }
        }
    })
}

fn uninstall_at(
    platform: Platform,
    unit_path: &Path,
    runner: &dyn CommandRunner,
) -> anyhow::Result<()> {
    match platform {
        Platform::MacOs => {
            let _ = runner.output("launchctl", &os_args(["bootout", &launchd_service()]));
            remove_unit(unit_path)?;
        }
        Platform::Linux => {
            let disable = runner.output(
                "systemctl",
                &os_args(["--user", "disable", "--now", SYSTEMD_UNIT]),
            )?;
            if unit_path.exists() {
                require_success(&disable, "systemctl --user disable --now")?;
            }
            remove_unit(unit_path)?;
            require_success(
                &runner.output("systemctl", &os_args(["--user", "daemon-reload"]))?,
                "systemctl --user daemon-reload",
            )?;
        }
    }
    Ok(())
}

fn remove_unit(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

/// The version the installed service launcher runs; `None` without an
/// installed launcher (foreground/dev setups) or with an empty version file.
///
/// # Errors
/// Returns an error when the version file exists but cannot be read.
pub(crate) fn launcher_version(dirs: &Dirs) -> anyhow::Result<Option<String>> {
    let path = dirs.launcher_version_file();
    match fs::read_to_string(&path) {
        Ok(version) => Ok(Some(version.trim().to_string()).filter(|version| !version.is_empty())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn require_success(output: &CommandOutput, description: &str) -> anyhow::Result<()> {
    if output.success {
        return Ok(());
    }
    let detail = output.stderr.trim();
    if detail.is_empty() {
        anyhow::bail!("{description} failed")
    }
    anyhow::bail!("{description} failed: {detail}")
}

fn os_args<const N: usize>(args: [&str; N]) -> Vec<OsString> {
    args.into_iter().map(OsString::from).collect()
}

fn launchd_domain() -> String {
    // SAFETY: `geteuid` has no preconditions and no failure mode.
    let uid = unsafe { libc::geteuid() };
    format!("gui/{uid}")
}

/// The launchd service target: `gui/<uid>/<label>`.
fn launchd_service() -> String {
    format!("{}/{LAUNCHD_LABEL}", launchd_domain())
}

/// The launchd agent plist: the stable launcher, the log file, and the XDG
/// bases pinned.
fn launchd_plist(dirs: &Dirs) -> String {
    let bootstrap = xml_escape(&dirs.launcher_dir().join("bootstrap.sh").to_string_lossy());
    let log = xml_escape(&dirs.log_dir().join("router.log").to_string_lossy());
    let env_entries = xdg_env(dirs)
        .into_iter()
        .fold(String::new(), |mut entries, (key, value)| {
            use std::fmt::Write as _;
            let _ = write!(
                entries,
                "    <key>{key}</key>\n    <string>{}</string>\n",
                xml_escape(&value)
            );
            entries
        });
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LAUNCHD_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>{bootstrap}</string>
    <string>serve</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
{env_entries}  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ExitTimeOut</key>
  <integer>60</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#
    )
}

/// The service manager does not inherit the installing shell's environment,
/// so `XDG_*` overrides active during setup would silently diverge from what
/// the service resolves (different state dir → different ingress token and
/// auth). Pin the resolved bases into the unit instead.
fn xdg_env(dirs: &Dirs) -> Vec<(&'static str, String)> {
    [
        ("XDG_CONFIG_HOME", dirs.config_dir.parent()),
        ("XDG_STATE_HOME", dirs.state_dir.parent()),
        ("XDG_CACHE_HOME", dirs.cache_dir.parent()),
    ]
    .into_iter()
    .filter_map(|(key, base)| Some((key, base?.to_string_lossy().into_owned())))
    .collect()
}

/// The systemd user unit: the stable launcher, the log file, and the XDG
/// bases pinned.
fn systemd_user_unit(dirs: &Dirs) -> String {
    let bootstrap = systemd_escape(&dirs.launcher_dir().join("bootstrap.sh").to_string_lossy());
    let log = systemd_escape(&dirs.log_dir().join("router.log").to_string_lossy());
    let env_lines = xdg_env(dirs)
        .into_iter()
        .fold(String::new(), |mut lines, (key, value)| {
            use std::fmt::Write as _;
            let _ = writeln!(
                lines,
                "Environment={}",
                systemd_escape(&format!("{key}={value}"))
            );
            lines
        });
    format!(
        "[Unit]\n\
Description=Alignment Hive model router\n\
After=network-online.target\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart=/bin/bash {bootstrap} serve\n\
{env_lines}Restart=on-failure\n\
RestartSec=2\n\
TimeoutStopSec=60\n\
StandardOutput=append:{log}\n\
StandardError=append:{log}\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn systemd_escape(value: &str) -> String {
    // `%` is a systemd specifier in unit values regardless of quoting.
    let value = value.replace('%', "%%");
    if value
        .chars()
        .all(|character| !character.is_whitespace() && !matches!(character, '"' | '\\'))
    {
        return value;
    }
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::os::unix::fs::MetadataExt as _;

    use super::*;

    fn plugin_sources(root: &Path) -> LauncherSources {
        LauncherSources {
            bootstrap: root.join("scripts/bootstrap.sh"),
            version: root.join("binary-version"),
        }
    }

    #[derive(Default)]
    struct FakeRunner {
        calls: RefCell<Vec<(String, Vec<OsString>)>>,
    }

    impl CommandRunner for FakeRunner {
        fn output(&self, program: &str, args: &[OsString]) -> anyhow::Result<CommandOutput> {
            self.calls
                .borrow_mut()
                .push((program.to_string(), args.to_vec()));
            Ok(CommandOutput {
                success: true,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn launchd_plist_has_exact_launcher_policy_and_logs() {
        let plist = launchd_plist(&Dirs::under(Path::new("/root")));
        assert_eq!(
            plist,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.alignment-hive.model-router</string>
  <key>ProgramArguments</key>
  <array>
    <string>/bin/bash</string>
    <string>/root/state/launcher/bootstrap.sh</string>
    <string>serve</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>XDG_CONFIG_HOME</key>
    <string>/root</string>
    <key>XDG_STATE_HOME</key>
    <string>/root</string>
    <key>XDG_CACHE_HOME</key>
    <string>/root</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ExitTimeOut</key>
  <integer>60</integer>
  <key>StandardOutPath</key>
  <string>/root/state/logs/router.log</string>
  <key>StandardErrorPath</key>
  <string>/root/state/logs/router.log</string>
</dict>
</plist>
"#
        );
    }

    #[test]
    fn systemd_unit_has_exact_launcher_policy_and_logs() {
        let unit = systemd_user_unit(&Dirs::under(Path::new("/root")));
        assert_eq!(
            unit,
            "[Unit]\n\
Description=Alignment Hive model router\n\
After=network-online.target\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart=/bin/bash /root/state/launcher/bootstrap.sh serve\n\
Environment=XDG_CONFIG_HOME=/root\n\
Environment=XDG_STATE_HOME=/root\n\
Environment=XDG_CACHE_HOME=/root\n\
Restart=on-failure\n\
RestartSec=2\n\
TimeoutStopSec=60\n\
StandardOutput=append:/root/state/logs/router.log\n\
StandardError=append:/root/state/logs/router.log\n\
\n\
[Install]\n\
WantedBy=default.target\n"
        );
    }

    #[test]
    fn generators_escape_paths() {
        let plist = launchd_plist(&Dirs::under(Path::new("/A & B")));
        assert!(plist.contains("/A &amp; B/state/launcher/bootstrap.sh"));
        let unit = systemd_user_unit(&Dirs::under(Path::new("/A B")));
        assert!(unit.contains("ExecStart=/bin/bash \"/A B/state/launcher/bootstrap.sh\" serve"));
    }

    #[test]
    fn systemd_escape_doubles_percent_specifiers() {
        let unit = systemd_user_unit(&Dirs::under(Path::new("/dir%20name")));
        assert!(
            unit.contains("ExecStart=/bin/bash /dir%%20name/state/launcher/bootstrap.sh serve")
        );
    }

    #[test]
    fn macos_install_uses_injected_dirs_and_unit_path() {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(root.path());
        let plugin = root.path().join("plugin");
        fs::create_dir_all(plugin.join("scripts")).unwrap();
        fs::write(plugin.join("scripts/bootstrap.sh"), b"#!/bin/bash\n").unwrap();
        fs::write(plugin.join("binary-version"), b"1.2.3\n").unwrap();
        let unit = root.path().join("home/Library/LaunchAgents/router.plist");
        let runner = FakeRunner::default();

        install_at(
            &dirs,
            Platform::MacOs,
            &unit,
            Some(&plugin_sources(&plugin)),
            &runner,
        )
        .unwrap();

        assert_eq!(
            fs::read(dirs.launcher_dir().join("bootstrap.sh")).unwrap(),
            b"#!/bin/bash\n"
        );
        assert_eq!(
            fs::read_to_string(dirs.launcher_dir().join("binary-version")).unwrap(),
            "1.2.3\n"
        );
        let mode = fs::metadata(dirs.launcher_dir().join("bootstrap.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(
            fs::read_to_string(&unit)
                .unwrap()
                .contains(&dirs.launcher_dir().display().to_string())
        );
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "launchctl");
        assert_eq!(calls[0].1[0], "bootout");
        assert_eq!(calls[1].1[0], "bootstrap");
    }

    #[test]
    fn install_reuses_an_existing_launcher_without_sources() {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(root.path());
        create_private_dir(&dirs.launcher_dir()).unwrap();
        fs::write(dirs.launcher_dir().join("bootstrap.sh"), "old script").unwrap();
        fs::write(dirs.launcher_dir().join("binary-version"), "old version").unwrap();
        let unit = root.path().join("systemd/model-router.service");
        let runner = FakeRunner::default();

        install_at(&dirs, Platform::Linux, &unit, None, &runner).unwrap();

        assert_eq!(
            fs::read_to_string(dirs.launcher_dir().join("bootstrap.sh")).unwrap(),
            "old script"
        );
        // Reload, then enable and start: without the second the unit never runs.
        let calls = runner.calls.borrow();
        let commands: Vec<String> = calls
            .iter()
            .map(|(program, args)| {
                let args: Vec<_> = args.iter().map(|arg| arg.to_string_lossy()).collect();
                format!("{program} {}", args.join(" "))
            })
            .collect();
        assert_eq!(
            commands,
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now model-router.service",
            ]
        );
    }

    #[test]
    fn refresh_moves_launcher_to_a_new_plugin_root() {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(root.path());
        let old_plugin = root.path().join("plugin-v1");
        let new_plugin = root.path().join("plugin-v2");
        for (plugin, script, version) in [
            (
                &old_plugin,
                b"old script\n".as_slice(),
                b"1.0.0\n".as_slice(),
            ),
            (
                &new_plugin,
                b"new script\n".as_slice(),
                b"2.0.0\n".as_slice(),
            ),
        ] {
            fs::create_dir_all(plugin.join("scripts")).unwrap();
            fs::write(plugin.join("scripts/bootstrap.sh"), script).unwrap();
            fs::write(plugin.join("binary-version"), version).unwrap();
        }
        populate_launcher(&dirs, Some(&plugin_sources(&old_plugin))).unwrap();
        let runner = FakeRunner::default();
        let unit = root.path().join("home/Library/LaunchAgents/router.plist");
        write_unit_contents(&unit, unit_contents(&dirs, Platform::MacOs).as_bytes()).unwrap();

        refresh_at(
            &dirs,
            Platform::MacOs,
            &unit,
            &plugin_sources(&new_plugin),
            &runner,
        )
        .unwrap();
        fs::remove_dir_all(&old_plugin).unwrap();

        assert_eq!(
            fs::read_to_string(dirs.launcher_dir().join("bootstrap.sh")).unwrap(),
            "new script\n"
        );
        assert_eq!(launcher_version(&dirs).unwrap().as_deref(), Some("2.0.0"));
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "bash");
        assert_eq!(
            calls[0].1,
            vec![
                new_plugin.join("scripts/bootstrap.sh").into_os_string(),
                OsString::from("prefetch"),
            ]
        );
        assert_eq!(calls[1].1[0], "kickstart");
    }

    #[test]
    fn failed_prefetch_aborts_refresh_before_touching_the_launcher() {
        struct FailingPrefetchRunner;
        impl CommandRunner for FailingPrefetchRunner {
            fn output(&self, program: &str, _args: &[OsString]) -> anyhow::Result<CommandOutput> {
                assert_eq!(
                    program, "bash",
                    "no service commands may run after a failed prefetch"
                );
                Ok(CommandOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: "Failed to download".to_string(),
                })
            }
        }

        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(root.path());
        let plugin = root.path().join("plugin");
        fs::create_dir_all(plugin.join("scripts")).unwrap();
        fs::write(plugin.join("scripts/bootstrap.sh"), b"new script\n").unwrap();
        fs::write(plugin.join("binary-version"), b"2.0.0\n").unwrap();
        create_private_dir(&dirs.launcher_dir()).unwrap();
        fs::write(dirs.launcher_dir().join("bootstrap.sh"), "old script").unwrap();
        fs::write(dirs.launcher_dir().join("binary-version"), "1.0.0").unwrap();
        let unit = root.path().join("systemd/model-router.service");

        let error = refresh_at(
            &dirs,
            Platform::Linux,
            &unit,
            &plugin_sources(&plugin),
            &FailingPrefetchRunner,
        )
        .unwrap_err();

        assert!(error.to_string().contains("prefetch"));
        assert_eq!(
            fs::read_to_string(dirs.launcher_dir().join("bootstrap.sh")).unwrap(),
            "old script"
        );
        assert_eq!(launcher_version(&dirs).unwrap().as_deref(), Some("1.0.0"));
        assert!(!unit.exists());
    }

    #[test]
    fn refresh_rewrites_a_stale_unit_and_reloads_before_restart() {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(root.path());
        let plugin = root.path().join("plugin");
        fs::create_dir_all(plugin.join("scripts")).unwrap();
        fs::write(plugin.join("scripts/bootstrap.sh"), b"new script\n").unwrap();
        fs::write(plugin.join("binary-version"), b"2.0.0\n").unwrap();
        let unit = root.path().join("systemd/model-router.service");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, "stale unit").unwrap();
        let runner = FakeRunner::default();

        refresh_at(
            &dirs,
            Platform::Linux,
            &unit,
            &plugin_sources(&plugin),
            &runner,
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&unit).unwrap(), systemd_user_unit(&dirs));
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0, "bash");
        assert_eq!(calls[1].1, os_args(["--user", "daemon-reload"]));
        assert_eq!(calls[2].1, os_args(["--user", "restart", SYSTEMD_UNIT]));
    }

    #[test]
    fn refresh_leaves_a_current_unit_untouched() {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(root.path());
        let plugin = root.path().join("plugin");
        fs::create_dir_all(plugin.join("scripts")).unwrap();
        fs::write(plugin.join("scripts/bootstrap.sh"), b"new script\n").unwrap();
        fs::write(plugin.join("binary-version"), b"2.0.0\n").unwrap();
        let unit = root.path().join("systemd/model-router.service");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, unit_contents(&dirs, Platform::Linux)).unwrap();
        let inode_before = { fs::metadata(&unit).unwrap().ino() };
        let runner = FakeRunner::default();

        refresh_at(
            &dirs,
            Platform::Linux,
            &unit,
            &plugin_sources(&plugin),
            &runner,
        )
        .unwrap();

        assert_eq!(fs::metadata(&unit).unwrap().ino(), inode_before);
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "bash");
        assert_eq!(calls[1].1, os_args(["--user", "restart", SYSTEMD_UNIT]));
    }

    #[test]
    fn uninstall_removes_only_the_injected_unit() {
        let root = tempfile::tempdir().unwrap();
        let dirs = Dirs::under(root.path());
        create_private_dir(&dirs.config_dir).unwrap();
        create_private_dir(&dirs.state_dir).unwrap();
        create_private_dir(&dirs.cache_dir).unwrap();
        let unit = root.path().join("systemd/model-router.service");
        fs::create_dir_all(unit.parent().unwrap()).unwrap();
        fs::write(&unit, "unit").unwrap();
        let runner = FakeRunner::default();

        uninstall_at(Platform::Linux, &unit, &runner).unwrap();

        assert!(!unit.exists());
        assert!(dirs.config_dir.exists());
        assert!(dirs.state_dir.exists());
        assert!(dirs.cache_dir.exists());
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].1,
            os_args(["--user", "disable", "--now", SYSTEMD_UNIT])
        );
        assert_eq!(calls[1].1, os_args(["--user", "daemon-reload"]));
    }
}

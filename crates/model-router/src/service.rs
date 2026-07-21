//! OS user-service installation and lifecycle management.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;

use crate::state::{Dirs, create_private_dir, write_private_atomic};

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
    fn from_plugin_root(root: &Path) -> Self {
        Self {
            bootstrap: root.join("scripts/bootstrap.sh"),
            version: root.join("binary-version"),
        }
    }

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
                 MODEL_ROUTER_VERSION_FILE, or pass --plugin-root <dir>"
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
/// When `plugin_root` is absent, launcher sources come from the environment
/// exported by `bootstrap.sh`. An already-complete stable launcher may be
/// reused if neither source is available.
///
/// # Errors
/// Returns an actionable error for unsupported platforms, missing launcher
/// sources, filesystem failures, or service-manager failures.
pub fn install(dirs: &Dirs, plugin_root: Option<&Path>) -> anyhow::Result<()> {
    let platform = Platform::current()?;
    let unit_path = unit_path(platform)?;
    let sources = resolve_sources(plugin_root)?;
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
pub fn refresh(dirs: &Dirs, plugin_root: Option<&Path>) -> anyhow::Result<()> {
    let platform = Platform::current()?;
    let sources = resolve_sources(plugin_root)?.ok_or_else(|| {
        anyhow::anyhow!(
            "service refresh requires launcher sources; pass --plugin-root <dir> or set \
             MODEL_ROUTER_BOOTSTRAP_SCRIPT and MODEL_ROUTER_VERSION_FILE"
        )
    })?;
    refresh_at(dirs, platform, &sources, &SystemCommandRunner)?;
    println!(
        "Refreshed launcher to {} and restarted model-router service.",
        launcher_version(dirs)?
            .as_deref()
            .unwrap_or("unknown version")
    );
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

fn resolve_sources(plugin_root: Option<&Path>) -> anyhow::Result<Option<LauncherSources>> {
    plugin_root.map_or_else(LauncherSources::from_environment, |root| {
        Ok(Some(LauncherSources::from_plugin_root(root)))
    })
}

fn unit_path(platform: Platform) -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
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
    populate_launcher(dirs, sources, false)?;
    create_private_dir(&dirs.log_dir())?;
    write_unit(dirs, platform, unit_path)?;

    match platform {
        Platform::MacOs => {
            let domain = launchd_domain();
            let service = format!("{domain}/{LAUNCHD_LABEL}");
            let _ = run_command(runner, "launchctl", &os_args(["bootout", &service]));
            require_success(
                &run_command(
                    runner,
                    "launchctl",
                    &[
                        OsString::from("bootstrap"),
                        OsString::from(domain),
                        unit_path.as_os_str().to_owned(),
                    ],
                )?,
                "launchctl bootstrap",
            )?;
        }
        Platform::Linux => {
            require_success(
                &run_command(runner, "systemctl", &os_args(["--user", "daemon-reload"]))?,
                "systemctl --user daemon-reload",
            )?;
            require_success(
                &run_command(
                    runner,
                    "systemctl",
                    &os_args(["--user", "enable", "--now", SYSTEMD_UNIT]),
                )?,
                "systemctl --user enable --now",
            )?;
        }
    }
    Ok(())
}

fn populate_launcher(
    dirs: &Dirs,
    sources: Option<&LauncherSources>,
    require_sources: bool,
) -> anyhow::Result<()> {
    let launcher_dir = dirs.launcher_dir();
    let destination_bootstrap = launcher_dir.join("bootstrap.sh");
    let destination_version = launcher_dir.join("binary-version");

    let Some(sources) = sources else {
        if !require_sources && destination_bootstrap.is_file() && destination_version.is_file() {
            return Ok(());
        }
        anyhow::bail!(
            "stable launcher is not populated at {}; pass --plugin-root <dir> or set \
             MODEL_ROUTER_BOOTSTRAP_SCRIPT and MODEL_ROUTER_VERSION_FILE",
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
    let version = fs::read(&sources.version)
        .with_context(|| format!("failed to read version file {}", sources.version.display()))?;
    anyhow::ensure!(
        !version.iter().all(u8::is_ascii_whitespace),
        "version file {} is empty",
        sources.version.display()
    );

    create_private_dir(&launcher_dir)?;
    write_private_atomic(&destination_bootstrap, &bootstrap)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&destination_bootstrap, fs::Permissions::from_mode(0o755))
            .with_context(|| {
                format!(
                    "failed to make launcher executable {}",
                    destination_bootstrap.display()
                )
            })?;
    }
    write_private_atomic(&destination_version, &version)?;
    Ok(())
}

fn refresh_at(
    dirs: &Dirs,
    platform: Platform,
    sources: &LauncherSources,
    runner: &dyn CommandRunner,
) -> anyhow::Result<()> {
    populate_launcher(dirs, Some(sources), true)?;
    restart_with(platform, runner)
}

fn write_unit(dirs: &Dirs, platform: Platform, path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("unit path {} has no parent", path.display()))?;
    create_private_dir(parent)?;
    let contents = match platform {
        Platform::MacOs => launchd_plist(&dirs.launcher_dir(), &dirs.log_dir(), dirs),
        Platform::Linux => systemd_user_unit(&dirs.launcher_dir(), &dirs.log_dir(), dirs),
    };
    write_private_atomic(path, contents.as_bytes())
        .with_context(|| format!("failed to write service unit {}", path.display()))
}

fn restart_with(platform: Platform, runner: &dyn CommandRunner) -> anyhow::Result<()> {
    match platform {
        Platform::MacOs => require_success(
            &run_command(
                runner,
                "launchctl",
                &os_args([
                    "kickstart",
                    "-k",
                    &format!("{}/{LAUNCHD_LABEL}", launchd_domain()),
                ]),
            )?,
            "launchctl kickstart",
        ),
        Platform::Linux => require_success(
            &run_command(
                runner,
                "systemctl",
                &os_args(["--user", "restart", SYSTEMD_UNIT]),
            )?,
            "systemctl --user restart",
        ),
    }
}

fn status_with(platform: Platform, runner: &dyn CommandRunner) -> anyhow::Result<String> {
    let output = match platform {
        Platform::MacOs => run_command(
            runner,
            "launchctl",
            &os_args(["print", &format!("{}/{LAUNCHD_LABEL}", launchd_domain())]),
        )?,
        Platform::Linux => run_command(
            runner,
            "systemctl",
            &os_args(["--user", "is-active", SYSTEMD_UNIT]),
        )?,
    };
    Ok(match platform {
        Platform::MacOs if output.success => "loaded".to_string(),
        Platform::MacOs => "not loaded".to_string(),
        Platform::Linux => {
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
            let _ = run_command(
                runner,
                "launchctl",
                &os_args(["bootout", &format!("{}/{LAUNCHD_LABEL}", launchd_domain())]),
            );
            remove_unit(unit_path)?;
        }
        Platform::Linux => {
            let disable = run_command(
                runner,
                "systemctl",
                &os_args(["--user", "disable", "--now", SYSTEMD_UNIT]),
            )?;
            if unit_path.exists() {
                require_success(&disable, "systemctl --user disable --now")?;
            }
            remove_unit(unit_path)?;
            require_success(
                &run_command(runner, "systemctl", &os_args(["--user", "daemon-reload"]))?,
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

fn launcher_version(dirs: &Dirs) -> anyhow::Result<Option<String>> {
    let path = dirs.launcher_dir().join("binary-version");
    match fs::read_to_string(&path) {
        Ok(version) => Ok(Some(version.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

fn run_command(
    runner: &dyn CommandRunner,
    program: &str,
    args: &[OsString],
) -> anyhow::Result<CommandOutput> {
    runner.output(program, args)
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

#[cfg(unix)]
fn launchd_domain() -> String {
    // SAFETY: `geteuid` has no preconditions and no failure mode.
    let uid = unsafe { libc::geteuid() };
    format!("gui/{uid}")
}

#[cfg(not(unix))]
fn launchd_domain() -> String {
    unreachable!("launchd is only available on Unix")
}

/// Generates the launchd agent plist for the given stable launcher and log
/// directories.
#[must_use]
pub fn launchd_plist(launcher_dir: &Path, log_dir: &Path, dirs: &Dirs) -> String {
    let bootstrap = xml_escape(&launcher_dir.join("bootstrap.sh").to_string_lossy());
    let log = xml_escape(&log_dir.join("router.log").to_string_lossy());
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

/// Generates the systemd user unit for the given stable launcher and log
/// directories.
#[must_use]
pub fn systemd_user_unit(launcher_dir: &Path, log_dir: &Path, dirs: &Dirs) -> String {
    let bootstrap = systemd_escape(&launcher_dir.join("bootstrap.sh").to_string_lossy());
    let log = systemd_escape(&log_dir.join("router.log").to_string_lossy());
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

    use super::*;

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

    fn test_dirs(root: &Path) -> Dirs {
        Dirs {
            config_dir: root.join("config"),
            state_dir: root.join("state"),
            cache_dir: root.join("cache"),
        }
    }

    #[test]
    fn launchd_plist_has_exact_launcher_policy_and_logs() {
        let dirs = test_dirs(Path::new("/root"));
        let plist = launchd_plist(
            Path::new("/state/launcher"),
            Path::new("/state/logs"),
            &dirs,
        );
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
    <string>/state/launcher/bootstrap.sh</string>
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
  <key>StandardOutPath</key>
  <string>/state/logs/router.log</string>
  <key>StandardErrorPath</key>
  <string>/state/logs/router.log</string>
</dict>
</plist>
"#
        );
    }

    #[test]
    fn systemd_unit_has_exact_launcher_policy_and_logs() {
        let dirs = test_dirs(Path::new("/root"));
        let unit = systemd_user_unit(
            Path::new("/state/launcher"),
            Path::new("/state/logs"),
            &dirs,
        );
        assert_eq!(
            unit,
            "[Unit]\n\
Description=Alignment Hive model router\n\
After=network-online.target\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart=/bin/bash /state/launcher/bootstrap.sh serve\n\
Environment=XDG_CONFIG_HOME=/root\n\
Environment=XDG_STATE_HOME=/root\n\
Environment=XDG_CACHE_HOME=/root\n\
Restart=on-failure\n\
RestartSec=2\n\
StandardOutput=append:/state/logs/router.log\n\
StandardError=append:/state/logs/router.log\n\
\n\
[Install]\n\
WantedBy=default.target\n"
        );
    }

    #[test]
    fn generators_escape_paths() {
        let dirs = test_dirs(Path::new("/root"));
        let plist = launchd_plist(Path::new("/A & B"), Path::new("/logs"), &dirs);
        assert!(plist.contains("/A &amp; B/bootstrap.sh"));
        let unit = systemd_user_unit(Path::new("/A B"), Path::new("/logs"), &dirs);
        assert!(unit.contains("ExecStart=/bin/bash \"/A B/bootstrap.sh\" serve"));
    }

    #[test]
    fn systemd_escape_doubles_percent_specifiers() {
        let unit = systemd_user_unit(
            Path::new("/dir%20name/launcher"),
            Path::new("/logs"),
            &test_dirs(Path::new("/root")),
        );
        assert!(unit.contains("ExecStart=/bin/bash /dir%%20name/launcher/bootstrap.sh serve"));
    }

    #[test]
    fn macos_install_uses_injected_dirs_and_unit_path() {
        let root = tempfile::tempdir().unwrap();
        let dirs = test_dirs(root.path());
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
            Some(&LauncherSources::from_plugin_root(&plugin)),
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(dirs.launcher_dir().join("bootstrap.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
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
        let dirs = test_dirs(root.path());
        create_private_dir(&dirs.launcher_dir()).unwrap();
        fs::write(dirs.launcher_dir().join("bootstrap.sh"), "old script").unwrap();
        fs::write(dirs.launcher_dir().join("binary-version"), "old version").unwrap();
        let unit = root.path().join("systemd/model-router.service");

        install_at(&dirs, Platform::Linux, &unit, None, &FakeRunner::default()).unwrap();

        assert_eq!(
            fs::read_to_string(dirs.launcher_dir().join("bootstrap.sh")).unwrap(),
            "old script"
        );
    }

    #[test]
    fn refresh_requires_sources_even_when_launcher_exists() {
        let root = tempfile::tempdir().unwrap();
        let dirs = test_dirs(root.path());
        create_private_dir(&dirs.launcher_dir()).unwrap();
        fs::write(dirs.launcher_dir().join("bootstrap.sh"), "old script").unwrap();
        fs::write(dirs.launcher_dir().join("binary-version"), "old version").unwrap();

        let error = populate_launcher(&dirs, None, true).unwrap_err();
        assert!(error.to_string().contains("--plugin-root"));
    }

    #[test]
    fn refresh_moves_launcher_to_a_new_plugin_root() {
        let root = tempfile::tempdir().unwrap();
        let dirs = test_dirs(root.path());
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
        populate_launcher(
            &dirs,
            Some(&LauncherSources::from_plugin_root(&old_plugin)),
            true,
        )
        .unwrap();
        let runner = FakeRunner::default();

        refresh_at(
            &dirs,
            Platform::MacOs,
            &LauncherSources::from_plugin_root(&new_plugin),
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
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1[0], "kickstart");
    }

    #[test]
    fn uninstall_removes_only_the_injected_unit() {
        let root = tempfile::tempdir().unwrap();
        let dirs = test_dirs(root.path());
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

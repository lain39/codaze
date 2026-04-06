use anyhow::{Context, bail};
use std::net::SocketAddr;
use std::path::PathBuf;

const DEFAULT_LISTEN: &str = "127.0.0.1:18039";
const DEFAULT_ADMIN_LISTEN: &str = "127.0.0.1:18040";
const DEFAULT_UPSTREAM_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const DEFAULT_STREAM_TIMEOUT_SECONDS: u64 = 600;
const DEFAULT_REFRESH_SKEW_SECONDS: i64 = 8;
pub const DEFAULT_ACCOUNTS_SCAN_INTERVAL_SECONDS: u64 = 15;
pub const DEFAULT_CODEX_VERSION: &str = "0.118.0";
pub const DEFAULT_SHUTDOWN_GRACE_PERIOD_SECONDS: u64 = 10;
const DEFAULT_ACCOUNTS_DIR_NAME: &str = ".codaze";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingPolicy {
    RoundRobin,
    LeastInFlight,
    FillFirst,
}

impl RoutingPolicy {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw {
            "round_robin" => Ok(Self::RoundRobin),
            "least_in_flight" => Ok(Self::LeastInFlight),
            "fill_first" => Ok(Self::FillFirst),
            other => bail!(
                "unsupported routing policy `{other}`; expected one of: round_robin, least_in_flight, fill_first"
            ),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RoundRobin => "round_robin",
            Self::LeastInFlight => "least_in_flight",
            Self::FillFirst => "fill_first",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FingerprintMode {
    Normalize,
    Passthrough,
}

impl FingerprintMode {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw {
            "normalize" => Ok(Self::Normalize),
            "passthrough" => Ok(Self::Passthrough),
            other => bail!(
                "unsupported fingerprint mode `{other}`; expected one of: normalize, passthrough"
            ),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normalize => "normalize",
            Self::Passthrough => "passthrough",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub listen: String,
    pub admin_listen: String,
    pub accounts_dir: PathBuf,
    pub routing_policy: RoutingPolicy,
    pub fingerprint_mode: FingerprintMode,
    pub upstream_base_url: String,
    pub codex_version: String,
    pub stream_timeout_seconds: u64,
    pub refresh_skew_seconds: i64,
    pub accounts_scan_interval_seconds: u64,
    pub shutdown_grace_period_seconds: u64,
}

impl RuntimeConfig {
    pub fn from_args() -> anyhow::Result<Self> {
        Self::from_args_with_home_env(
            std::env::args().skip(1),
            current_home_platform_is_windows(),
            std::env::var_os("HOME"),
            std::env::var_os("USERPROFILE"),
            std::env::var_os("HOMEDRIVE"),
            std::env::var_os("HOMEPATH"),
        )
    }

    fn from_args_with_home_env<I>(
        args: I,
        is_windows: bool,
        home: Option<std::ffi::OsString>,
        userprofile: Option<std::ffi::OsString>,
        homedrive: Option<std::ffi::OsString>,
        homepath: Option<std::ffi::OsString>,
    ) -> anyhow::Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut listen = DEFAULT_LISTEN.to_string();
        let mut admin_listen = DEFAULT_ADMIN_LISTEN.to_string();
        let mut accounts_dir = None;
        let mut routing_policy = RoutingPolicy::LeastInFlight;
        let mut fingerprint_mode = FingerprintMode::Normalize;
        let mut codex_version = DEFAULT_CODEX_VERSION.to_string();
        let mut shutdown_grace_period_seconds = DEFAULT_SHUTDOWN_GRACE_PERIOD_SECONDS;

        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--listen" => {
                    let value = args.next().context("missing value after --listen")?;
                    listen = value;
                }
                "--admin-listen" => {
                    let value = args.next().context("missing value after --admin-listen")?;
                    admin_listen = value;
                }
                "--accounts-dir" => {
                    let value = args.next().context("missing value after --accounts-dir")?;
                    accounts_dir = Some(PathBuf::from(value));
                }
                "--routing-policy" => {
                    let value = args
                        .next()
                        .context("missing value after --routing-policy")?;
                    routing_policy = RoutingPolicy::parse(&value)?;
                }
                "--fingerprint-mode" => {
                    let value = args
                        .next()
                        .context("missing value after --fingerprint-mode")?;
                    fingerprint_mode = FingerprintMode::parse(&value)?;
                }
                "--codex-version" => {
                    codex_version = args.next().context("missing value after --codex-version")?;
                }
                "--shutdown-grace-period-seconds" => {
                    let value = args
                        .next()
                        .context("missing value after --shutdown-grace-period-seconds")?;
                    shutdown_grace_period_seconds = value.parse().with_context(|| {
                        format!("invalid value for --shutdown-grace-period-seconds: `{value}`")
                    })?;
                }
                "--help" | "-h" => {
                    print_usage_and_exit();
                }
                other => bail!("unknown argument `{other}`"),
            }
        }

        if codex_version.trim().is_empty() {
            bail!("--codex-version must not be empty");
        }
        if listen == admin_listen {
            bail!("--listen and --admin-listen must be different addresses");
        }
        let accounts_dir = match accounts_dir {
            Some(path) => path,
            None => {
                default_accounts_dir_with_env(is_windows, home, userprofile, homedrive, homepath)?
            }
        };

        Ok(Self {
            listen,
            admin_listen,
            accounts_dir,
            routing_policy,
            fingerprint_mode,
            upstream_base_url: DEFAULT_UPSTREAM_BASE_URL.to_string(),
            codex_version,
            stream_timeout_seconds: DEFAULT_STREAM_TIMEOUT_SECONDS,
            refresh_skew_seconds: DEFAULT_REFRESH_SKEW_SECONDS,
            accounts_scan_interval_seconds: DEFAULT_ACCOUNTS_SCAN_INTERVAL_SECONDS,
            shutdown_grace_period_seconds,
        })
    }
}

pub fn ensure_loopback_listener(bind_addr: SocketAddr) -> anyhow::Result<()> {
    if bind_addr.ip().is_loopback() {
        Ok(())
    } else {
        bail!("codaze is local-only; bind to 127.0.0.1 or ::1, got `{bind_addr}`")
    }
}

fn print_usage_and_exit() -> ! {
    eprintln!(
        "Usage: codaze [--accounts-dir DIR] [--codex-version VERSION] [--listen HOST:PORT] [--admin-listen HOST:PORT] [--routing-policy POLICY] [--fingerprint-mode MODE] [--shutdown-grace-period-seconds N]"
    );
    eprintln!("Defaults:");
    eprintln!(
        "  accounts-dir: {}",
        default_accounts_dir_help_text(
            current_home_platform_is_windows(),
            std::env::var_os("HOME"),
            std::env::var_os("USERPROFILE"),
            std::env::var_os("HOMEDRIVE"),
            std::env::var_os("HOMEPATH"),
        )
    );
    eprintln!("  codex-version: {DEFAULT_CODEX_VERSION}");
    eprintln!("  listen: {DEFAULT_LISTEN}");
    eprintln!("  admin-listen: {DEFAULT_ADMIN_LISTEN}");
    eprintln!("  shutdown-grace-period-seconds: {DEFAULT_SHUTDOWN_GRACE_PERIOD_SECONDS}");
    eprintln!("Policies: round_robin | least_in_flight | fill_first");
    eprintln!("Fingerprint modes: normalize | passthrough");
    std::process::exit(0);
}

fn default_accounts_dir_with_env(
    is_windows: bool,
    home: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
    homedrive: Option<std::ffi::OsString>,
    homepath: Option<std::ffi::OsString>,
) -> anyhow::Result<PathBuf> {
    let home_dir = if is_windows {
        windows_home_dir(userprofile, homedrive, homepath).context(
            "USERPROFILE/HOMEDRIVE/HOMEPATH are not set or are empty and --accounts-dir was not provided",
        )?
    } else {
        non_empty_os_string(home)
            .map(PathBuf::from)
            .context("HOME is not set or empty and --accounts-dir was not provided")?
    };

    Ok(home_dir.join(DEFAULT_ACCOUNTS_DIR_NAME))
}

fn default_accounts_dir_help_text(
    is_windows: bool,
    home: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
    homedrive: Option<std::ffi::OsString>,
    homepath: Option<std::ffi::OsString>,
) -> String {
    default_accounts_dir_with_env(is_windows, home, userprofile, homedrive, homepath)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| {
            format!(
                "{DEFAULT_ACCOUNTS_DIR_NAME} under the platform home dir (or pass --accounts-dir)"
            )
        })
}

fn windows_home_dir(
    userprofile: Option<std::ffi::OsString>,
    homedrive: Option<std::ffi::OsString>,
    homepath: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    if let Some(userprofile) = non_empty_os_string(userprofile) {
        return Some(PathBuf::from(userprofile));
    }

    let homedrive = non_empty_os_string(homedrive)?;
    let homepath = non_empty_os_string(homepath)?;
    let mut combined = homedrive;
    combined.push(homepath);
    Some(PathBuf::from(combined))
}

fn non_empty_os_string(value: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    value.filter(|value| !value.is_empty())
}

fn current_home_platform_is_windows() -> bool {
    #[cfg(windows)]
    {
        true
    }

    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_accounts_dir_does_not_require_home_env() {
        let config = RuntimeConfig::from_args_with_home_env(
            vec!["--accounts-dir".to_string(), "/tmp/codaze".to_string()],
            false,
            None,
            None,
            None,
            None,
        )
        .expect("config parses");

        assert_eq!(config.accounts_dir, PathBuf::from("/tmp/codaze"));
    }

    #[test]
    fn unix_default_accounts_dir_uses_home() {
        let path =
            default_accounts_dir_with_env(false, Some("/home/tester".into()), None, None, None)
                .expect("path resolves");

        assert_eq!(path, PathBuf::from("/home/tester/.codaze"));
    }

    #[test]
    fn windows_default_accounts_dir_uses_userprofile() {
        let path =
            default_accounts_dir_with_env(true, None, Some(r"C:\Users\tester".into()), None, None)
                .expect("path resolves");

        assert_eq!(path, PathBuf::from(r"C:\Users\tester").join(".codaze"));
    }

    #[test]
    fn windows_default_accounts_dir_falls_back_to_homedrive_homepath() {
        let path = default_accounts_dir_with_env(
            true,
            None,
            None,
            Some("C:".into()),
            Some(r"\Users\tester".into()),
        )
        .expect("path resolves");

        assert_eq!(path, PathBuf::from(r"C:\Users\tester").join(".codaze"));
    }

    #[test]
    fn missing_home_env_returns_actionable_error() {
        let error = default_accounts_dir_with_env(false, None, None, None, None)
            .expect_err("missing env should fail");

        assert!(
            error
                .to_string()
                .contains("HOME is not set or empty and --accounts-dir was not provided")
        );
    }

    #[test]
    fn empty_home_env_returns_actionable_error() {
        let error = default_accounts_dir_with_env(false, Some("".into()), None, None, None)
            .expect_err("empty env should fail");

        assert!(
            error
                .to_string()
                .contains("HOME is not set or empty and --accounts-dir was not provided")
        );
    }

    #[test]
    fn empty_windows_home_env_returns_actionable_error() {
        let error = default_accounts_dir_with_env(
            true,
            None,
            Some("".into()),
            Some("".into()),
            Some("".into()),
        )
        .expect_err("empty env should fail");

        assert!(error.to_string().contains(
            "USERPROFILE/HOMEDRIVE/HOMEPATH are not set or are empty and --accounts-dir was not provided"
        ));
    }

    #[test]
    fn help_text_does_not_hardcode_unix_tilde_path() {
        let text = default_accounts_dir_help_text(true, None, None, None, None);

        assert!(!text.contains("~/.codaze"));
        assert!(text.contains(".codaze"));
    }
}

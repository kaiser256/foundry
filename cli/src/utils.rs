use ethers::{
    abi::token::{LenientTokenizer, Tokenizer},
    prelude::TransactionReceipt,
    providers::Middleware,
    types::U256,
    utils::{format_units, to_checksum},
};
use eyre::Result;
use foundry_config::{Chain, Config};
use std::{
    ffi::OsStr,
    future::Future,
    ops::Mul,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    str::FromStr,
    time::Duration,
};
use tracing_error::ErrorLayer;
use tracing_subscriber::prelude::*;
use yansi::Paint;

// reexport all `foundry_config::utils`
#[doc(hidden)]
pub use foundry_config::utils::*;

/// The version message for the current program, like
/// `forge 0.1.0 (f01b232bc 2022-01-22T23:28:39.493201+00:00)`
pub(crate) const VERSION_MESSAGE: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("VERGEN_GIT_SHA"),
    " ",
    env!("VERGEN_BUILD_TIMESTAMP"),
    ")"
);

/// Deterministic fuzzer seed used for gas snapshots and coverage reports.
///
/// The keccak256 hash of "foundry rulez"
pub static STATIC_FUZZ_SEED: [u8; 32] = [
    0x01, 0x00, 0xfa, 0x69, 0xa5, 0xf1, 0x71, 0x0a, 0x95, 0xcd, 0xef, 0x94, 0x88, 0x9b, 0x02, 0x84,
    0x5d, 0x64, 0x0b, 0x19, 0xad, 0xf0, 0xe3, 0x57, 0xb8, 0xd4, 0xbe, 0x7d, 0x49, 0xee, 0x70, 0xe6,
];

/// Useful extensions to [`std::path::Path`].
pub trait FoundryPathExt {
    /// Returns true if the [`Path`] ends with `.t.sol`
    fn is_sol_test(&self) -> bool;

    /// Returns true if the  [`Path`] has a `sol` extension
    fn is_sol(&self) -> bool;

    /// Returns true if the  [`Path`] has a `yul` extension
    fn is_yul(&self) -> bool;
}

impl<T: AsRef<Path>> FoundryPathExt for T {
    fn is_sol_test(&self) -> bool {
        self.as_ref()
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.ends_with(".t.sol"))
            .unwrap_or_default()
    }

    fn is_sol(&self) -> bool {
        self.as_ref().extension() == Some(std::ffi::OsStr::new("sol"))
    }

    fn is_yul(&self) -> bool {
        self.as_ref().extension() == Some(std::ffi::OsStr::new("yul"))
    }
}

/// Initializes a tracing Subscriber for logging
#[allow(dead_code)]
pub fn subscriber() {
    tracing_subscriber::Registry::default()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(ErrorLayer::default())
        .with(tracing_subscriber::fmt::layer())
        .init()
}

/// parse a hex str or decimal str as U256
pub fn parse_u256(s: &str) -> Result<U256> {
    Ok(if s.starts_with("0x") { U256::from_str(s)? } else { U256::from_dec_str(s)? })
}

/// Returns a [RetryProvider](foundry_common::RetryProvider) instantiated using [Config]'s RPC URL
/// and chain.
///
/// Defaults to `http://localhost:8545` and `Mainnet`.
pub fn get_provider(config: &Config) -> Result<foundry_common::RetryProvider> {
    get_provider_builder(config)?.build()
}
/// Returns a [ProviderBuilder](foundry_common::ProviderBuilder) instantiated using [Config]'s RPC
/// URL and chain.
///
/// Defaults to `http://localhost:8545` and `Mainnet`.
pub fn get_provider_builder(config: &Config) -> Result<foundry_common::ProviderBuilder> {
    let url = config.get_rpc_url_or_localhost_http()?;
    let chain = config.chain_id.unwrap_or_default();
    Ok(foundry_common::ProviderBuilder::new(url.as_ref()).chain(chain))
}

pub async fn get_chain<M>(chain: Option<Chain>, provider: M) -> Result<Chain>
where
    M: Middleware,
    M::Error: 'static,
{
    match chain {
        Some(chain) => Ok(chain),
        None => Ok(Chain::Id(provider.get_chainid().await?.as_u64())),
    }
}

/// Parses an ether value from a string.
///
/// The amount can be tagged with a unit, e.g. "1ether".
///
/// If the string represents an untagged amount (e.g. "100") then
/// it is interpreted as wei.
pub fn parse_ether_value(value: &str) -> Result<U256> {
    Ok(if value.starts_with("0x") {
        U256::from_str(value)?
    } else {
        U256::from(LenientTokenizer::tokenize_uint(value)?)
    })
}

/// Parses a `Duration` from a &str
pub fn parse_delay(delay: &str) -> Result<Duration> {
    let delay = if delay.ends_with("ms") {
        let d: u64 = delay.trim_end_matches("ms").parse()?;
        Duration::from_millis(d)
    } else {
        let d: f64 = delay.parse()?;
        let delay = (d * 1000.0).round();
        if delay.is_infinite() || delay.is_nan() || delay.is_sign_negative() {
            eyre::bail!("delay must be finite and non-negative");
        }

        Duration::from_millis(delay as u64)
    };
    Ok(delay)
}

/// Runs the `future` in a new [`tokio::runtime::Runtime`]
#[allow(unused)]
pub fn block_on<F: Future>(future: F) -> F::Output {
    let rt = tokio::runtime::Runtime::new().expect("could not start tokio rt");
    rt.block_on(future)
}

/// Conditionally print a message
///
/// This macro accepts a predicate and the message to print if the predicate is tru
///
/// ```ignore
/// let quiet = true;
/// p_println!(!quiet => "message");
/// ```
macro_rules! p_println {
    ($p:expr => $($arg:tt)*) => {{
        if $p {
            println!($($arg)*)
        }
    }}
}
pub(crate) use p_println;

/// Loads a dotenv file, from the cwd and the project root, ignoring potential failure.
///
/// We could use `tracing::warn!` here, but that would imply that the dotenv file can't configure
/// the logging behavior of Foundry.
///
/// Similarly, we could just use `eprintln!`, but colors are off limits otherwise dotenv is implied
/// to not be able to configure the colors. It would also mess up the JSON output.
pub fn load_dotenv() {
    let load = |p: &Path| {
        dotenvy::from_path(p.join(".env")).ok();
    };

    // we only want the .env file of the cwd and project root
    // `find_project_root_path` calls `current_dir` internally so both paths are either both `Ok` or
    // both `Err`
    if let (Ok(cwd), Ok(prj_root)) = (std::env::current_dir(), find_project_root_path(None)) {
        load(&prj_root);
        if cwd != prj_root {
            // prj root and cwd can be identical
            load(&cwd);
        }
    };
}

/// Disables terminal colours if either:
/// - Running windows and the terminal does not support colour codes.
/// - Colour has been disabled by some environment variable.
/// - We are running inside a test
pub fn enable_paint() {
    let is_windows = cfg!(windows) && !Paint::enable_windows_ascii();
    let env_colour_disabled = std::env::var("NO_COLOR").is_ok();
    if is_windows || env_colour_disabled {
        Paint::disable();
    }
}

/// Prints parts of the receipt to stdout
pub fn print_receipt(chain: Chain, receipt: &TransactionReceipt) {
    let gas_used = receipt.gas_used.unwrap_or_default();
    let gas_price = receipt.effective_gas_price.unwrap_or_default();
    println!(
        "\n##### {chain}\n{status}Hash: {tx_hash:?}{caddr}\nBlock: {bn}\n{gas}\n",
        status = if receipt.status.map_or(true, |s| s.is_zero()) {
            "❌  [Failed]"
        } else {
            "✅  [Success]"
        },
        tx_hash = receipt.transaction_hash,
        caddr = if let Some(addr) = &receipt.contract_address {
            format!("\nContract Address: {}", to_checksum(addr, None))
        } else {
            String::new()
        },
        bn = receipt.block_number.unwrap_or_default(),
        gas = if gas_price.is_zero() {
            format!("Gas Used: {gas_used}")
        } else {
            let paid = format_units(gas_used.mul(gas_price), 18).unwrap_or_else(|_| "N/A".into());
            let gas_price = format_units(gas_price, 9).unwrap_or_else(|_| "N/A".into());
            format!(
                "Paid: {} ETH ({gas_used} gas * {} gwei)",
                paid.trim_end_matches('0'),
                gas_price.trim_end_matches('0').trim_end_matches('.')
            )
        },
    );
}

/// Useful extensions to [`std::process::Command`].
pub trait CommandUtils {
    /// Returns the command's output if execution is successful, otherwise, throws an error.
    fn exec(&mut self) -> Result<Output>;

    /// Returns the command's stdout if execution is successful, otherwise, throws an error.
    fn get_stdout_lossy(&mut self) -> Result<String>;
}

impl CommandUtils for Command {
    #[track_caller]
    fn exec(&mut self) -> Result<Output> {
        tracing::trace!(command=?self, "executing");

        let output = self.output()?;

        tracing::trace!(code=?output.status.code(), ?output);

        if output.status.success() {
            Ok(output)
        } else {
            let mut stderr = String::from_utf8_lossy(&output.stderr);
            let mut msg = stderr.trim();
            if msg.is_empty() {
                stderr = String::from_utf8_lossy(&output.stdout);
                msg = stderr.trim();
            }

            let mut name = self.get_program().to_string_lossy();
            if let Some(arg) = self.get_args().next() {
                let arg = arg.to_string_lossy();
                if !arg.starts_with('-') {
                    let name = name.to_mut();
                    name.push(' ');
                    name.push_str(&arg);
                }
            }

            let mut err = match output.status.code() {
                Some(code) => format!("{name} exited with code {code}"),
                None => format!("{name} terminated by a signal"),
            };
            if !msg.is_empty() {
                err.push_str(": ");
                err.push_str(msg);
            }
            Err(eyre::eyre!(err))
        }
    }

    #[track_caller]
    fn get_stdout_lossy(&mut self) -> Result<String> {
        let output = self.exec()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().into())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Git<'a> {
    pub root: &'a Path,
    pub quiet: bool,
    pub shallow: bool,
}

impl<'a> Git<'a> {
    #[inline]
    pub fn new(root: &'a Path) -> Self {
        Self { root, quiet: false, shallow: false }
    }

    #[inline]
    pub fn from_config(config: &'a Config) -> Self {
        Self::new(config.__root.0.as_path())
    }

    pub fn root_of(relative_to: &Path) -> Result<PathBuf> {
        let output = Self::cmd_no_root()
            .current_dir(relative_to)
            .args(["rev-parse", "--show-toplevel"])
            .get_stdout_lossy()?;
        Ok(PathBuf::from(output))
    }

    pub fn clone(
        shallow: bool,
        from: impl AsRef<OsStr>,
        to: Option<impl AsRef<OsStr>>,
    ) -> Result<()> {
        Self::cmd_no_root()
            .stderr(Stdio::inherit())
            .args(["clone", "--recurse-submodules"])
            .args(shallow.then_some("--depth=1"))
            .args(shallow.then_some("--shallow-submodules"))
            .arg(from)
            .args(to)
            .exec()
            .map(drop)
    }

    #[inline]
    pub fn root(self, root: &Path) -> Git<'_> {
        Git { root, ..self }
    }

    #[inline]
    pub fn quiet(self, quiet: bool) -> Self {
        Self { quiet, ..self }
    }

    /// True to perform shallow clones
    #[inline]
    pub fn shallow(self, shallow: bool) -> Self {
        Self { shallow, ..self }
    }

    pub fn checkout(self, recursive: bool, tag: impl AsRef<OsStr>) -> Result<()> {
        self.cmd()
            .arg("checkout")
            .args(recursive.then_some("--recurse-submodules"))
            .arg(tag)
            .exec()
            .map(drop)
    }

    pub fn init(self) -> Result<()> {
        self.cmd().arg("init").exec().map(drop)
    }

    #[allow(clippy::should_implement_trait)] // this is not std::ops::Add clippy
    pub fn add<I, S>(self, paths: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd().arg("add").args(paths).exec().map(drop)
    }

    pub fn rm<I, S>(self, force: bool, paths: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd().arg("rm").args(force.then_some("--force")).args(paths).exec().map(drop)
    }

    pub fn commit(self, msg: &str) -> Result<()> {
        let output = self
            .cmd()
            .args(["commit", "-m", msg])
            .args(cfg!(any(test, debug_assertions)).then_some("--no-gpg-sign"))
            .output()?;
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            // ignore "nothing to commit" error
            let msg = "nothing to commit, working tree clean";
            if !(stdout.contains(msg) || stderr.contains(msg)) {
                return Err(eyre::eyre!(
                    "failed to commit (code={:?}, stdout={:?}, stderr={:?})",
                    output.status.code(),
                    stdout.trim(),
                    stderr.trim()
                ))
            }
        }
        Ok(())
    }

    pub fn is_in_repo(self) -> std::io::Result<bool> {
        self.cmd().args(["rev-parse", "--is-inside-work-tree"]).status().map(|s| s.success())
    }

    pub fn is_clean(self) -> Result<bool> {
        self.cmd().args(["status", "--porcelain"]).exec().map(|out| out.stdout.is_empty())
    }

    pub fn has_branch(self, branch: impl AsRef<OsStr>) -> Result<bool> {
        self.cmd()
            .args(["branch", "--list", "--no-color"])
            .arg(branch)
            .get_stdout_lossy()
            .map(|stdout| !stdout.is_empty())
    }

    pub fn ensure_clean(self) -> Result<()> {
        if self.is_clean()? {
            Ok(())
        } else {
            Err(eyre::eyre!(
                "\
The target directory is a part of or on its own an already initialized git repository,
and it requires clean working and staging areas, including no untracked files.

Check the current git repository's status with `git status`.
Then, you can track files with `git add ...` and then commit them with `git commit`,
ignore them in the `.gitignore` file, or run this command again with the `--no-commit` flag.

If none of the previous steps worked, please open an issue at:
https://github.com/foundry-rs/foundry/issues/new/choose"
            ))
        }
    }

    pub fn commit_hash(self, short: bool) -> Result<String> {
        self.cmd().arg("rev-parse").args(short.then_some("--short")).arg("HEAD").get_stdout_lossy()
    }

    pub fn tag(self) -> Result<String> {
        self.cmd().arg("tag").get_stdout_lossy()
    }

    pub fn has_missing_dependencies<I, S>(self, paths: I) -> Result<bool>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd()
            .args(["submodule", "status"])
            .args(paths)
            .get_stdout_lossy()
            .map(|stdout| stdout.lines().any(|line| line.starts_with('-')))
    }

    pub fn submodule_add(
        self,
        force: bool,
        url: impl AsRef<OsStr>,
        path: impl AsRef<OsStr>,
    ) -> Result<()> {
        self.cmd()
            .stderr(self.stderr())
            .args(["submodule", "add"])
            .args(self.shallow.then_some("--depth=1"))
            .args(force.then_some("--force"))
            .arg(url)
            .arg(path)
            .exec()
            .map(drop)
    }

    pub fn submodule_update<I, S>(self, force: bool, remote: bool, paths: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.cmd()
            .stderr(self.stderr())
            .args(["submodule", "update", "--progress", "--init", "--recursive"])
            .args(self.shallow.then_some("--depth=1"))
            .args(force.then_some("--force"))
            .args(remote.then_some("--remote"))
            .args(paths)
            .exec()
            .map(drop)
    }

    pub fn cmd(self) -> Command {
        let mut cmd = Self::cmd_no_root();
        cmd.current_dir(self.root);
        cmd
    }

    pub fn cmd_no_root() -> Command {
        let mut cmd = Command::new("git");
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd
    }

    // don't set this in cmd() because it's not wanted for all commands
    fn stderr(self) -> Stdio {
        if self.quiet {
            Stdio::piped()
        } else {
            Stdio::inherit()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_cli_test_utils::tempfile::tempdir;
    use foundry_common::fs;
    use std::{env, fs::File, io::Write};

    #[test]
    fn foundry_path_ext_works() {
        let p = Path::new("contracts/MyTest.t.sol");
        assert!(p.is_sol_test());
        assert!(p.is_sol());
        let p = Path::new("contracts/Greeter.sol");
        assert!(!p.is_sol_test());
    }

    // loads .env from cwd and project dir, See [`find_project_root_path()`]
    #[test]
    fn can_load_dotenv() {
        let temp = tempdir().unwrap();
        Git::new(temp.path()).init().unwrap();
        let cwd_env = temp.path().join(".env");
        fs::create_file(temp.path().join("foundry.toml")).unwrap();
        let nested = temp.path().join("nested");
        fs::create_dir(&nested).unwrap();

        let mut cwd_file = File::create(cwd_env).unwrap();
        let mut prj_file = File::create(nested.join(".env")).unwrap();

        cwd_file.write_all("TESTCWDKEY=cwd_val".as_bytes()).unwrap();
        cwd_file.sync_all().unwrap();

        prj_file.write_all("TESTPRJKEY=prj_val".as_bytes()).unwrap();
        prj_file.sync_all().unwrap();

        let cwd = env::current_dir().unwrap();
        env::set_current_dir(nested).unwrap();
        load_dotenv();
        env::set_current_dir(cwd).unwrap();

        assert_eq!(env::var("TESTCWDKEY").unwrap(), "cwd_val");
        assert_eq!(env::var("TESTPRJKEY").unwrap(), "prj_val");
    }
}

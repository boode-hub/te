//! Help > Check for updates. te goes online only here, only when asked, on a thread of its own;
//! the checks that make an update safe are in [`crate::update`].

use crate::update::{self, Asset, Release, Version};
use eframe::egui::Context;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum State {
    Checking,
    UpToDate(Version),
    Available(Release),
    /// A newer version exists, but without a download te can install.
    NoBuild(Release),
    Downloading { version: Version, done: u64, total: u64 },
    /// Downloaded and verified; the new program is running its self-test.
    Testing(Version),
    /// In place; takes effect at the next start.
    Installed(Version),
    Failed(String),
}

#[derive(Clone)]
pub struct Updater(Arc<Mutex<State>>);

impl Updater {
    /// Asks GitHub for the latest release.
    pub fn check(ctx: &Context) -> Self {
        let u = Updater(Arc::new(Mutex::new(State::Checking)));
        let (me, ctx) = (u.clone(), ctx.clone());
        std::thread::spawn(move || {
            let state = match latest() {
                Ok(r) if r.version <= installed_version() => State::UpToDate(installed_version()),
                Ok(r) if r.zip().is_some() => State::Available(r),
                Ok(r) => State::NoBuild(r),
                Err(e) => State::Failed(e),
            };
            me.set(&ctx, state);
        });
        u
    }

    pub fn state(&self) -> State {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, ctx: &Context, s: State) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = s;
        ctx.request_repaint();
    }

    /// Downloads, verifies and installs `release` next to the running program.
    pub fn install(&self, ctx: &Context, release: Release) {
        let (me, ctx) = (self.clone(), ctx.clone());
        std::thread::spawn(move || {
            let v = release.version;
            let result = (|| {
                let exe = current_exe()?;
                let agent = agent();
                let mut fetch = |a: &Asset, limit: usize| {
                    get(&agent, &a.url, limit, &mut |done| {
                        if a.name.ends_with(".zip") {
                            me.set(&ctx, State::Downloading { version: v, done, total: a.size });
                        }
                    })
                };
                me.set(&ctx, State::Downloading { version: v, done: 0, total: 0 });
                let program = update::download(&release, &update::public_key(), &mut fetch)?;
                me.set(&ctx, State::Testing(v));
                update::install(&exe, &program, |new| self_test(new, v))
            })();
            me.set(
                &ctx,
                match result {
                    Ok(()) => {
                        *INSTALLED.lock().unwrap_or_else(|e| e.into_inner()) = Some(v);
                        State::Installed(v)
                    }
                    Err(e) => State::Failed(e),
                },
            );
        });
    }
}

static PROGRAM: OnceLock<Option<PathBuf>> = OnceLock::new();
/// Set once an update is in place, so checking again does not offer it twice.
static INSTALLED: Mutex<Option<Version>> = Mutex::new(None);

/// Records where this program was started from. Called first thing, before anything can rename it.
pub fn remember_program() {
    PROGRAM.get_or_init(|| std::env::current_exe().ok());
}

fn current_exe() -> Result<PathBuf, String> {
    PROGRAM.get().and_then(|p| p.clone()).ok_or_else(|| "te cannot find its own program file".to_string())
}

/// What a newer release is compared with: this version, or one already installed this session.
fn installed_version() -> Version {
    INSTALLED.lock().unwrap_or_else(|e| e.into_inner()).unwrap_or_else(Version::current)
}

/// Removes what an earlier update left beside the program, now that this version has started
/// properly. Not when this *is* a leftover (someone starting `te.old.exe` by hand to go back).
pub fn clean_up_after_update() {
    if let Ok(exe) = current_exe()
        && exe.file_name().is_some_and(|n| n.eq_ignore_ascii_case(update::PROGRAM))
    {
        update::clean_up(&exe);
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(600)))
        .user_agent(format!("te/{} (+{})", env!("CARGO_PKG_VERSION"), update::RELEASES_PAGE))
        .build()
        .into()
}

/// The latest release, as GitHub describes it. `TE_UPDATE_URL` points elsewhere (for testing);
/// whatever it serves must still carry te's signature to be installed.
pub fn latest() -> Result<Release, String> {
    // The headless UI test clicks menus at random; it must never go online.
    if !crate::dialogs() {
        return Err("offline (test mode)".into());
    }
    let url = std::env::var("TE_UPDATE_URL").unwrap_or_else(|_| update::LATEST_API.to_string());
    let json = get(&agent(), &url, update::MAX_SMALL, &mut |_| {})?;
    Release::parse(&json)
}

/// `te --install-update`: the whole update without a window. Returns what was done, or why nothing was.
pub fn install_from_command_line() -> Result<String, String> {
    let current = installed_version();
    let r = latest()?;
    if r.version <= current {
        return Ok(format!("te {current} is up to date (latest release: {})", r.version));
    }
    let exe = current_exe()?;
    let agent = agent();
    let mut fetch = |a: &Asset, limit: usize| get(&agent, &a.url, limit, &mut |_| {});
    let program = update::download(&r, &update::public_key(), &mut fetch)?;
    let v = r.version;
    update::install(&exe, &program, |new| self_test(new, v))?;
    Ok(format!("te {v} is installed in {}; it starts the next time te opens", exe.display()))
}

/// Fetches `url` into memory; more than `limit` bytes is an error, not a truncated file.
fn get(agent: &ureq::Agent, url: &str, limit: usize, progress: &mut dyn FnMut(u64)) -> Result<Vec<u8>, String> {
    let mut resp = agent.get(url).header("Accept", "application/vnd.github+json, application/octet-stream").call().map_err(describe)?;
    let mut r = resp.body_mut().with_config().limit(limit as u64 + 1).reader();
    let (mut out, mut buf, mut last) = (Vec::new(), vec![0u8; 64 * 1024], Instant::now());
    loop {
        let n = r.read(&mut buf).map_err(|e| format!("the download was cut off ({e})"))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if out.len() > limit {
            return Err("the download is larger than expected".into());
        }
        if last.elapsed() > Duration::from_millis(100) {
            progress(out.len() as u64);
            last = Instant::now();
        }
    }
    progress(out.len() as u64);
    Ok(out)
}

fn describe(e: ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(403 | 429) => "GitHub is limiting update checks from this network; try again in an hour".into(),
        ureq::Error::StatusCode(404) => "no release was found on GitHub".into(),
        ureq::Error::StatusCode(c) => format!("GitHub answered with an error ({c})"),
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Io(_) => "te could not reach GitHub. Check the internet connection and try again.".into(),
        ureq::Error::Timeout(_) => "GitHub took too long to answer; try again later".into(),
        other => format!("the update check failed ({other})"),
    }
}

/// Runs `new --self-test`: the new program must start, pass its checks, and report the version
/// it claims to be.
fn self_test(new: &Path, want: Version) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut child = Command::new(new)
        .arg("--self-test")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .spawn()
        .map_err(|e| format!("it would not start: {e}"))?;
    let (mut stdout, mut stderr) = (child.stdout.take(), child.stderr.take());
    let reader = std::thread::spawn(move || {
        let (mut o, mut e) = (String::new(), String::new());
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_string(&mut o);
        }
        if let Some(s) = stderr.as_mut() {
            let _ = s.read_to_string(&mut e);
        }
        (o, e)
    });
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("it did not finish within 90 seconds".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let (out, err) = reader.join().unwrap_or_default();
    let ok = format!("te {want} self-test ok");
    if status.success() && out.lines().any(|l| l.trim() == ok) {
        Ok(())
    } else {
        let why = err.lines().chain(out.lines()).find(|l| !l.trim().is_empty());
        Err(why.map_or_else(|| format!("exit status {status}"), |l| l.trim().to_string()))
    }
}

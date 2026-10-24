//! Updates the app from the repository's GitHub releases. CI publishes one
//! for every build of master that passes the tests: `build-<n>` (n is the
//! commit count) with a universal `Me-macos.zip` and its SHA-256.
//!
//! A release build checks shortly after launch and every few hours. A newer
//! build is downloaded in the background, checked (size, checksum, bundle
//! id, build number, code signature) and swapped in when the app quits, or
//! right away from "Restart to Update". The repository is private, so
//! requests carry a token: `github_token` in settings.json, `GH_TOKEN` /
//! `GITHUB_TOKEN`, or the GitHub CLI's (`gh auth token`).
//!
//! Development builds (no build number compiled in) and copies that can't
//! be replaced (not in a .app, translocated, read-only) never update.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use gpui::{App, AppContext, Context, Entity, Global, PromptLevel, Subscription, Task};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::actions::{CheckForUpdates, RestartToUpdate};
use crate::{app_state, settings};

const FIRST_CHECK_AFTER: Duration = Duration::from_secs(30);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
const ZIP_ASSET: &str = "Me-macos.zip";
const CHECKSUM_ASSET: &str = "Me-macos.zip.sha256";
const BUNDLE_ID: &str = "com.erkie.me";

/// This build's number, compiled in by `script/bundle-macos`; `None` for
/// development builds.
pub fn current_build() -> Option<u64> {
    option_env!("ME_BUILD")?.parse().ok()
}

/// "0.1.0 (build 57, 1a2b3c4)", or just the version for development builds.
pub fn version_label() -> String {
    let version = env!("CARGO_PKG_VERSION");
    match (current_build(), option_env!("ME_COMMIT")) {
        (Some(build), Some(commit)) => format!("{version} (build {build}, {commit})"),
        (Some(build), None) => format!("{version} (build {build})"),
        _ => format!("{version} (development build)"),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Downloading {
        build: u64,
    },
    /// Downloaded and checked, waiting for a restart.
    Ready {
        build: u64,
        notes: String,
    },
    Failed(String),
}

pub struct Updater {
    state: State,
    /// The new app, waiting to be swapped in.
    staged: Option<PathBuf>,
    _check: Option<Task<()>>,
    _poll: Option<Task<()>>,
    _quit: Subscription,
}

struct GlobalUpdater(Entity<Updater>);

impl Global for GlobalUpdater {}

pub fn init(cx: &mut App) {
    let updater = cx.new(Updater::new);
    cx.set_global(GlobalUpdater(updater));
    cx.on_action(|_: &CheckForUpdates, cx| {
        if let Some(updater) = entity(cx) {
            updater.update(cx, |updater, cx| updater.check(true, cx));
        }
    });
    cx.on_action(|_: &RestartToUpdate, cx| restart(cx));
}

pub fn entity(cx: &App) -> Option<Entity<Updater>> {
    cx.try_global::<GlobalUpdater>().map(|g| g.0.clone())
}

/// The build waiting for a restart, for the status bar.
pub fn ready_build(cx: &App) -> Option<u64> {
    match entity(cx)?.read(cx).state {
        State::Ready { build, .. } => Some(build),
        _ => None,
    }
}

/// Swaps in the downloaded build and starts it.
pub fn restart(cx: &mut App) {
    let Some(updater) = entity(cx) else { return };
    let staged = updater.update(cx, |updater, _| updater.staged.take());
    if let Some(app) = staged {
        match install(&app, true) {
            Ok(()) => app_state::quit(cx),
            Err(err) => {
                updater.update(cx, |updater, cx| {
                    updater.staged = Some(app);
                    updater.finish(State::Failed(format!("{err:#}")), true, cx);
                });
            }
        }
    }
}

impl Updater {
    fn new(cx: &mut Context<Self>) -> Self {
        // Quitting with a build downloaded installs it for next time.
        let quit = cx.on_app_quit(|this: &mut Self, _| {
            if let Some(app) = this.staged.take()
                && let Err(err) = install(&app, false)
            {
                eprintln!("me: could not install the update: {err:#}");
            }
            async {}
        });
        let mut this = Self {
            state: State::Idle,
            staged: None,
            _check: None,
            _poll: None,
            _quit: quit,
        };
        if unavailable(cx).is_none() {
            this.restore_staged();
            this._poll = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(FIRST_CHECK_AFTER).await;
                loop {
                    if this.update(cx, |this, cx| this.check(false, cx)).is_err() {
                        return;
                    }
                    cx.background_executor().timer(CHECK_EVERY).await;
                }
            }));
        }
        this
    }

    /// A build downloaded by an earlier run that didn't get to install it.
    fn restore_staged(&mut self) {
        let dir = updates_dir();
        let current = current_build().unwrap_or(0);
        match Staged::read(&dir) {
            Some(staged) if staged.build > current && staged.app.is_dir() => {
                self.state = State::Ready {
                    build: staged.build,
                    notes: staged.notes,
                };
                self.staged = Some(staged.app);
            }
            _ => {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }

    /// Looks for a newer build and downloads it. `manual` (Check for
    /// Updates…) says how it went in a dialog.
    fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        if let Some(reason) = unavailable(cx) {
            if manual {
                self.finish(State::Failed(reason), true, cx);
            }
            return;
        }
        match &self.state {
            State::Checking | State::Downloading { .. } => return,
            State::Ready { .. } => {
                if manual {
                    let state = self.state.clone();
                    self.finish(state, true, cx);
                }
                return;
            }
            _ => {}
        }
        let token_setting = settings::get(cx).github_token.clone();
        let current = current_build().unwrap_or(0);
        self.state = State::Checking;
        cx.notify();
        self._check = Some(cx.spawn(async move |this, cx| {
            let latest = cx
                .background_executor()
                .spawn(async move {
                    let source = Source::new(token_setting);
                    let release = source.latest()?;
                    anyhow::Ok((source, release))
                })
                .await;
            let (source, release) = match latest {
                Ok(found) => found,
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.finish(State::Failed(format!("{err:#}")), manual, cx)
                    })
                    .ok();
                    return;
                }
            };
            if release.build <= current {
                this.update(cx, |this, cx| this.finish(State::UpToDate, manual, cx))
                    .ok();
                return;
            }
            let build = release.build;
            this.update(cx, |this, cx| {
                this.state = State::Downloading { build };
                cx.notify();
            })
            .ok();
            let downloaded = cx
                .background_executor()
                .spawn(async move {
                    let dir = updates_dir();
                    let app = source.download(&release, &dir)?;
                    Staged {
                        build: release.build,
                        notes: release.notes.clone(),
                        app: app.clone(),
                    }
                    .write(&dir)?;
                    anyhow::Ok((app, release.notes))
                })
                .await;
            this.update(cx, |this, cx| match downloaded {
                Ok((app, notes)) => {
                    this.staged = Some(app);
                    this.finish(State::Ready { build, notes }, manual, cx);
                }
                Err(err) => this.finish(State::Failed(format!("{err:#}")), manual, cx),
            })
            .ok();
        }));
    }

    fn finish(&mut self, state: State, manual: bool, cx: &mut Context<Self>) {
        if let State::Failed(err) = &state {
            eprintln!("me: update check failed: {err}");
        }
        self.state = state.clone();
        cx.notify();
        if !manual {
            return;
        }
        let current = current_build().unwrap_or(0);
        let (level, title, detail, answers): (_, String, String, &[&str]) = match state {
            State::Ready { build, notes } => (
                PromptLevel::Info,
                format!("Me build {build} is ready"),
                format!(
                    "You have build {current}. Restart to switch; it's also installed when you quit.\n\n{}",
                    summarize(&notes)
                ),
                &["Restart Now", "Later"],
            ),
            State::UpToDate => (
                PromptLevel::Info,
                "Me is up to date".into(),
                format!("Build {current} is the latest."),
                &["OK"],
            ),
            State::Failed(err) => (
                PromptLevel::Warning,
                "Couldn't check for updates".into(),
                err,
                &["OK"],
            ),
            _ => return,
        };
        let Some(window) = cx
            .active_window()
            .or_else(|| cx.windows().into_iter().next())
        else {
            return;
        };
        let restart_offered = answers.len() > 1;
        window
            .update(cx, |_, window, cx| {
                let answer = window.prompt(level, &title, Some(&detail), answers, cx);
                cx.spawn(async move |cx| {
                    if answer.await == Ok(0) && restart_offered {
                        cx.update(restart);
                    }
                })
                .detach();
            })
            .ok();
    }
}

/// Why this copy of the app can't update itself, if it can't.
fn unavailable(cx: &App) -> Option<String> {
    if !settings::get(cx).auto_update {
        return Some("Updates are turned off (\"auto_update\" in settings.json).".into());
    }
    if current_build().is_none() {
        return Some("This is a development build; it doesn't update itself.".into());
    }
    let Some(bundle) = bundle() else {
        return Some("Me isn't running from an app bundle, so it can't replace itself.".into());
    };
    let writable = bundle.parent().is_some_and(|dir| {
        tempfile::Builder::new()
            .prefix(".me-update-check")
            .tempfile_in(dir)
            .is_ok()
    });
    if !writable {
        return Some(format!(
            "Can't replace {} (no permission to write there).",
            bundle.display()
        ));
    }
    None
}

/// The .app this process runs from, if it's one that can be replaced.
/// `ME_UPDATE_BUNDLE` overrides it (tests).
fn bundle() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ME_UPDATE_BUNDLE") {
        return Some(path.into());
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    // Me.app/Contents/MacOS/Me
    let bundle = exe.parent()?.parent()?.parent()?;
    let translocated = bundle.to_string_lossy().contains("/AppTranslocation/");
    (bundle.extension()? == "app" && !translocated).then(|| bundle.to_path_buf())
}

fn updates_dir() -> PathBuf {
    settings::data_dir().join("updates")
}

/// A downloaded build, remembered across runs (`ready.json`).
#[derive(Serialize, Deserialize)]
struct Staged {
    build: u64,
    notes: String,
    app: PathBuf,
}

impl Staged {
    fn read(dir: &Path) -> Option<Self> {
        serde_json::from_slice(&std::fs::read(dir.join("ready.json")).ok()?).ok()
    }

    fn write(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::write(dir.join("ready.json"), serde_json::to_vec(self)?)?;
        Ok(())
    }
}

/// The first lines of release notes, for a dialog.
fn summarize(notes: &str) -> String {
    let lines: Vec<&str> = notes.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut summary = lines
        .iter()
        .take(12)
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if lines.len() > 12 {
        summary.push_str(&format!("\n… and {} more", lines.len() - 12));
    }
    summary
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Asset {
    name: String,
    /// The API URL (works for private repositories, with a token).
    url: String,
    size: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct Release {
    build: u64,
    notes: String,
    zip: Asset,
    checksum: Option<Asset>,
}

fn parse_release(json: &[u8]) -> anyhow::Result<Release> {
    #[derive(Deserialize)]
    struct Json {
        tag_name: String,
        #[serde(default)]
        body: Option<String>,
        assets: Vec<Asset>,
    }
    let release: Json = serde_json::from_slice(json).context("unexpected answer from GitHub")?;
    let build = release
        .tag_name
        .strip_prefix("build-")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow!("the latest release ({}) isn't a build", release.tag_name))?;
    let asset = |name: &str| release.assets.iter().find(|a| a.name == name).cloned();
    Ok(Release {
        build,
        notes: release.body.clone().unwrap_or_default(),
        zip: asset(ZIP_ASSET).ok_or_else(|| anyhow!("build {build} has no {ZIP_ASSET}"))?,
        checksum: asset(CHECKSUM_ASSET),
    })
}

/// Where releases come from, and the token to read them with.
struct Source {
    api: String,
    repository: String,
    token: Option<String>,
}

impl Source {
    fn new(token_setting: Option<String>) -> Self {
        Self {
            api: std::env::var("ME_UPDATE_API").unwrap_or_else(|_| "https://api.github.com".into()),
            repository: repository(env!("CARGO_PKG_REPOSITORY")).to_string(),
            token: find_token(token_setting),
        }
    }

    fn latest(&self) -> anyhow::Result<Release> {
        let url = format!("{}/repos/{}/releases/latest", self.api, self.repository);
        let (status, body) = self.get(&url, "application/vnd.github+json", None)?;
        match status {
            200 => parse_release(&body),
            404 if self.token.is_none() => bail!(
                "{} is private: sign in with the GitHub CLI (gh auth login), or put a token in \
                 settings.json as \"github_token\".",
                self.repository
            ),
            404 => bail!(
                "No builds of {} yet, or the token can't read it.",
                self.repository
            ),
            401 | 403 => bail!("GitHub refused the token ({status})."),
            _ => bail!("GitHub answered {status}."),
        }
    }

    /// Downloads and checks a build; returns the unpacked app.
    fn download(&self, release: &Release, dir: &Path) -> anyhow::Result<PathBuf> {
        let build_dir = dir.join(format!("build-{}", release.build));
        let _ = std::fs::remove_dir_all(&build_dir);
        std::fs::create_dir_all(&build_dir)?;
        let zip = build_dir.join(ZIP_ASSET);
        let (status, _) = self.get(&release.zip.url, "application/octet-stream", Some(&zip))?;
        if status != 200 {
            bail!("Downloading build {} failed ({status}).", release.build);
        }
        let bytes = std::fs::read(&zip)?;
        if bytes.len() as u64 != release.zip.size {
            bail!("The download of build {} is incomplete.", release.build);
        }
        if let Some(checksum) = &release.checksum {
            let (status, body) = self.get(&checksum.url, "application/octet-stream", None)?;
            let expected = String::from_utf8_lossy(&body)
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if status != 200 || expected != sha256_hex(&bytes) {
                bail!("The download of build {} is corrupt.", release.build);
            }
        }
        let unpacked = build_dir.join("unpacked");
        unzip(&zip, &unpacked)?;
        let _ = std::fs::remove_file(&zip);
        let app = std::fs::read_dir(&unpacked)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or_else(|| anyhow!("Build {} has no app in it.", release.build))?;
        verify_app(&app, release.build)?;
        Ok(app)
    }

    /// GETs `url` with curl (always there on macOS; nothing to link) into
    /// `output`, or into memory. The token goes in on stdin, so it isn't on
    /// a command line; curl doesn't pass it on when GitHub redirects a
    /// download to its storage.
    fn get(
        &self,
        url: &str,
        accept: &str,
        output: Option<&Path>,
    ) -> anyhow::Result<(u16, Vec<u8>)> {
        let mut command = Command::new("curl");
        command
            .args(["--silent", "--show-error", "--location", "--retry", "2"])
            .args(["--connect-timeout", "20", "--max-time", "900"])
            .args(["--config", "-", "--write-out", "%{http_code}"])
            .args(["--header", &format!("Accept: {accept}")])
            .args(["--header", "X-GitHub-Api-Version: 2022-11-28"])
            .args(["--user-agent", &format!("Me/{}", env!("CARGO_PKG_VERSION"))]);
        if let Some(output) = output {
            command.arg("--output").arg(output);
        }
        let mut child = command
            .arg(url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("could not run curl")?;
        {
            let mut stdin = child.stdin.take().context("curl has no stdin")?;
            if let Some(token) = &self.token {
                writeln!(stdin, "header = \"Authorization: Bearer {token}\"")?;
            }
        }
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
        }
        // The body (when not saved to a file), then --write-out's status.
        let mut body = out.stdout;
        let status = body
            .len()
            .checked_sub(3)
            .map(|at| body.split_off(at))
            .and_then(|code| String::from_utf8(code).ok()?.parse().ok())
            .ok_or_else(|| anyhow!("no answer from {url}"))?;
        Ok((status, body))
    }
}

/// "owner/repo" from the package's repository URL.
fn repository(url: &str) -> &str {
    let path = url.trim_end_matches('/').trim_end_matches(".git");
    path.split_once("github.com/").map_or(path, |(_, p)| p)
}

/// A token from settings, the environment, or the GitHub CLI.
fn find_token(setting: Option<String>) -> Option<String> {
    let nonempty = |t: String| Some(t.trim().to_string()).filter(|t| !t.is_empty());
    setting
        .and_then(nonempty)
        .or_else(|| std::env::var("GH_TOKEN").ok().and_then(nonempty))
        .or_else(|| std::env::var("GITHUB_TOKEN").ok().and_then(nonempty))
        .or_else(|| {
            let gh = me_lsp::registry::which("gh")?;
            let out = Command::new(gh)
                .args(["auth", "token", "--hostname", "github.com"])
                .env("PATH", me_lsp::registry::search_path())
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .ok()?;
            out.status.success().then_some(())?;
            nonempty(String::from_utf8(out.stdout).ok()?)
        })
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn unzip(zip: &Path, dest: &Path) -> anyhow::Result<()> {
    let _ = std::fs::remove_dir_all(dest);
    let status = if cfg!(target_os = "macos") {
        // ditto keeps the bundle's symlinks, permissions and signature.
        Command::new("/usr/bin/ditto")
            .arg("-x")
            .arg("-k")
            .arg(zip)
            .arg(dest)
            .status()
    } else {
        Command::new("unzip")
            .arg("-q")
            .arg(zip)
            .arg("-d")
            .arg(dest)
            .status()
    }
    .context("could not unpack the update")?;
    if !status.success() {
        bail!("Unpacking the update failed.");
    }
    Ok(())
}

/// Checks that `app` is Me, the build we expect, and intact.
fn verify_app(app: &Path, build: u64) -> anyhow::Result<()> {
    let plist = std::fs::read_to_string(app.join("Contents/Info.plist"))
        .context("the update has no Info.plist")?;
    if plist_string(&plist, "CFBundleIdentifier") != Some(BUNDLE_ID) {
        bail!("The update isn't Me.");
    }
    if plist_string(&plist, "CFBundleVersion") != Some(build.to_string().as_str()) {
        bail!("The update isn't build {build}.");
    }
    if cfg!(target_os = "macos") {
        let status = Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict"])
            .arg(app)
            .stderr(Stdio::null())
            .status()
            .context("could not run codesign")?;
        if !status.success() {
            bail!("The update's signature doesn't check out.");
        }
    }
    Ok(())
}

/// A string value from an XML property list.
fn plist_string<'a>(plist: &'a str, key: &str) -> Option<&'a str> {
    let after = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = start + after[start..].find("</string>")?;
    Some(after[start..end].trim())
}

/// Starts a script that waits for this process to exit, puts `app` where
/// the running bundle is, and (with `relaunch`) opens it.
fn install(app: &Path, relaunch: bool) -> anyhow::Result<()> {
    let bundle = bundle().context("Me isn't running from an app bundle")?;
    let dir = app
        .ancestors()
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("build-"))
        })
        .unwrap_or(app)
        .to_path_buf();
    let script = install_script(app, &bundle, &dir, std::process::id(), relaunch);
    let path = updates_dir().join("install.sh");
    std::fs::write(&path, script)?;
    let _ = std::fs::remove_file(updates_dir().join("ready.json"));
    let mut command = Command::new("/bin/sh");
    command
        .arg(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command.spawn().context("could not start the installer")?;
    Ok(())
}

fn install_script(app: &Path, bundle: &Path, dir: &Path, pid: u32, relaunch: bool) -> String {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', r"'\''"));
    let after = if cfg!(target_os = "macos") {
        let register = "/System/Library/Frameworks/CoreServices.framework/Frameworks/\
                        LaunchServices.framework/Support/lsregister";
        let open = if relaunch { "open \"$bundle\"\n" } else { "" };
        format!("{register} -f \"$bundle\" >/dev/null 2>&1\n{open}")
    } else if relaunch {
        "\"$bundle/Contents/MacOS/Me\" >/dev/null 2>&1 &\n".to_string()
    } else {
        String::new()
    };
    format!(
        "#!/bin/sh\n\
         # Puts the new build of Me in place once the running one has quit.\n\
         while kill -0 {pid} 2>/dev/null; do sleep 0.2; done\n\
         new={new}\n\
         bundle={bundle}\n\
         old=\"$bundle.previous\"\n\
         rm -rf \"$old\"\n\
         if mv \"$bundle\" \"$old\"; then\n\
         \x20 if mv \"$new\" \"$bundle\"; then\n\
         \x20   rm -rf \"$old\"\n\
         \x20 else\n\
         \x20   mv \"$old\" \"$bundle\"\n\
         \x20 fi\n\
         fi\n\
         rm -rf {dir}\n\
         {after}",
        new = q(app),
        bundle = q(bundle),
        dir = q(dir),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_releases() {
        let json = br#"{
            "tag_name": "build-57", "name": "Me 0.1.0 (build 57)",
            "body": "Me 0.1.0, build 57\n\n- Ruby\n- Hover",
            "assets": [
                { "name": "Me-macos.zip", "url": "https://api.github.com/a/1", "size": 1234 },
                { "name": "Me-macos.zip.sha256", "url": "https://api.github.com/a/2", "size": 65 }
            ]
        }"#;
        let release = parse_release(json).unwrap();
        assert_eq!(release.build, 57);
        assert_eq!(release.zip.url, "https://api.github.com/a/1");
        assert_eq!(release.zip.size, 1234);
        assert!(release.checksum.is_some());
        assert!(parse_release(br#"{"tag_name": "v1.0", "assets": []}"#).is_err());
        assert!(parse_release(br#"{"tag_name": "build-3", "assets": []}"#).is_err());
    }

    #[test]
    fn finds_the_repository() {
        assert_eq!(repository("https://github.com/erkie/me"), "erkie/me");
        assert_eq!(repository("https://github.com/erkie/me.git/"), "erkie/me");
        assert_eq!(repository(env!("CARGO_PKG_REPOSITORY")), "erkie/me");
    }

    #[test]
    fn reads_plists() {
        let plist = "<dict>\n\t<key>CFBundleIdentifier</key>\n\t<string>com.erkie.me</string>\n\
                     \t<key>CFBundleVersion</key>\n\t<string>57</string>\n</dict>";
        assert_eq!(
            plist_string(plist, "CFBundleIdentifier"),
            Some("com.erkie.me")
        );
        assert_eq!(plist_string(plist, "CFBundleVersion"), Some("57"));
        assert_eq!(plist_string(plist, "Missing"), None);
    }

    #[test]
    fn checksums() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_script_swaps_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Apps/Me's.app");
        let staged_dir = dir.path().join("updates/build-9");
        let new = staged_dir.join("unpacked/Me.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(bundle.join("Contents/old"), "").unwrap();
        std::fs::create_dir_all(new.join("Contents")).unwrap();
        std::fs::write(new.join("Contents/new"), "").unwrap();
        // A pid that has exited, so the script doesn't wait.
        let mut child = Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        let script = install_script(&new, &bundle, &staged_dir, pid, false);
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(bundle.join("Contents/new").exists());
        assert!(!bundle.join("Contents/old").exists());
        assert!(!dir.path().join("Apps/Me's.app.previous").exists());
        assert!(!staged_dir.exists());
    }
}

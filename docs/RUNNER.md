# The Mac that runs CI

CI's macOS jobs (tests, and the release build that the app updates itself
from) run on our own always-on Mac as a GitHub Actions self-hosted runner:
no Actions minutes (macOS minutes count 10× on a private repository), and
builds stay warm between runs. The Linux job still runs on GitHub's
machines.

The workflow asks for a runner labelled `self-hosted` and `macOS`, which
every macOS runner has. Until one is online, those jobs wait in the queue
(GitHub gives up after 24 hours).

## Setting it up

On the Mac, as the user the builds should run as:

1. **Keep it running.** System Settings → Users & Groups → *Automatically
   log in as* that user (the runner starts at login; this needs FileVault
   off). System Settings → Energy: *Prevent automatic sleeping when the
   display is off* and *Start up automatically after a power failure*.

2. **Build tools.**
   - Xcode from the App Store (the full app: GPUI compiles its Metal
     shaders at build time, which the command line tools alone can't), then
     ```sh
     sudo xcode-select -s /Applications/Xcode.app
     sudo xcodebuild -license accept
     xcodebuild -downloadComponent MetalToolchain   # Xcode 26 and later only
     ```
   - [Homebrew](https://brew.sh), then `brew install gh` (the release step
     publishes with the GitHub CLI; it gets its token from the workflow, so
     no `gh auth login`).
   - Rust: `curl https://sh.rustup.rs -sSf | sh -s -- -y` (the workflow
     installs the toolchain and targets it needs).

3. **Register the runner.** On GitHub: the repository's *Settings → Actions
   → Runners → New self-hosted runner*, pick macOS and the Mac's
   architecture (x64 for Intel, ARM64 for Apple silicon), and run the
   commands it shows in Terminal: they download the runner into
   `~/actions-runner` and run `./config.sh` with a one-time token. Accept
   the default name and labels.

4. **Run it as a service**, so it starts by itself after a reboot:
   ```sh
   cd ~/actions-runner
   ./svc.sh install
   ./svc.sh start
   ```
   The Runners page should now show it as *Idle*. A push to master (or
   *Actions → CI → Run workflow*) runs on it.

## Looking after it

- Builds are kept in `~/actions-runner/_work/me/target` (a few GB, more with
  the universal release build). Delete it to start clean.
- Logs: `~/actions-runner/_diag/`, and `./svc.sh status`.
- Update Xcode and macOS as usual; the runner updates itself.
- Keep the repository private while this runner is attached: on a public
  repository anyone's pull request could run code on this Mac.

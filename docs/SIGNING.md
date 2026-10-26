# Signing and notarizing builds

Builds signed with a Developer ID and notarized by Apple open with a
double-click, without the "unidentified developer" warning. And once you
run a signed build, Me only installs updates signed by the same team, so
publishing an update takes the signing key, not just access to GitHub.

The keys never go to GitHub. CI builds Me.app and publishes it as a
**draft** release. A Mac of yours (an old one in a cupboard is fine) runs
`script/sign-release` every five minutes. The script:

1. Downloads the newest draft and checks it against its SHA-256.
2. Signs `Me.app` with your Developer ID.
3. Has Apple notarize it and staples the ticket.
4. Replaces the release's zip with the signed one and publishes the
   release.

If the Mac was off for a few builds, the script signs only the newest
one. It deletes the older drafts, because the newest one's notes list
their commits too.

## What the Mac needs

- macOS 11 (Big Sur) or later. It stays on and logged in, because the
  signing key is in the login keychain.
- The Xcode command line tools, which provide `codesign`, `notarytool`,
  `stapler` and `python3`. Install them with `xcode-select --install`.
- A paid Apple Developer Program membership, for the certificate.

## Setup

All of this is done on the Mac that will sign.

**1. A Developer ID certificate.** Create it on this Mac, so its private
key is made here and stays here.

1. In Keychain Access, choose Certificate Assistant → Request a
   Certificate From a Certificate Authority. Enter your email, choose
   "Saved to disk", and save the request.
2. Go to developer.apple.com → Certificates, IDs & Profiles →
   Certificates → +.
3. Choose **Developer ID Application**, upload the request, and download
   the certificate.
4. Double-click the certificate to add it to the login keychain.
5. Check that it's there:

```sh
security find-identity -v -p codesigning
#  1) 1A2B… "Developer ID Application: Your Name (ABCDE12345)"
```

**2. A notarization key.**

1. In App Store Connect, go to Users and Access → Integrations → App
   Store Connect API → Team Keys.
2. Generate a key with **Developer** access.
3. Download `AuthKey_XXXXXXXXXX.p8`. You can only download it once.
4. Note the Key ID and the Issuer ID shown on that page.
5. Store the key in the login keychain as a notarytool profile, then
   delete the file. Without `--keychain`, notarytool may try the
   "Local Items" keychain and fail with "User interaction is not
   allowed"; the script finds the profile in the login keychain without
   being told where it is.

```sh
xcrun notarytool store-credentials me-notary \
  --key ~/Downloads/AuthKey_XXXXXXXXXX.p8 --key-id XXXXXXXXXX --issuer <issuer id> \
  --keychain ~/Library/Keychains/login.keychain-db
rm ~/Downloads/AuthKey_XXXXXXXXXX.p8
```

**3. A GitHub token** that can publish releases of this repository and
nothing else.

1. On github.com, go to Settings → Developer settings → Personal access
   tokens → Fine-grained tokens → Generate new token.
2. Under Repository access, choose only `erkie/me`.
3. Under Permissions, set **Contents** to Read and write.
4. Generate the token, then store it in the keychain. The last `-w`
   makes `security` ask for the token instead of taking it on the command
   line:

```sh
security add-generic-password -s me-signer-github -a github -w
```

**4. Try it** on the latest published build, without changing anything:

```sh
git clone https://github.com/erkie/me && cd me
python3 script/sign-release --dry-run build-28   # the newest build-<n> on the releases page
```

macOS asks whether `codesign` may use the key. Enter your password and
click **Always Allow**, so the script can sign on its own later.
Notarizing takes a few minutes.

Then sign that build for real. It stays published and gets the signed
zip:

```sh
python3 script/sign-release build-28
```

Download it from the releases page in a browser and open it. It should
open without the warning.

**5. Run it every five minutes:**

```sh
python3 script/sign-release --install
```

This copies the script to `~/Library/Application Support/Me Signer/` and
starts a launchd agent that runs that copy. The log is in `~/Library/Logs/Me
Signer.log`.

**6. Turn on drafts.** In the repository on GitHub, go to Settings →
Secrets and variables → Actions → Variables. Add the repository variable
`SIGNED_RELEASES` with the value `true`. From the next push on, CI
publishes drafts and the Mac publishes them once they're signed.

Settings like a different identity or profile name go in
`~/Library/Application Support/Me Signer/config`. The top of
`script/sign-release` lists them.

## Day to day

There's nothing to do. A build shows up on the releases page a few
minutes after its CI run, once Apple has notarized it. If a build is
still a draft after an hour or so, the Mac is off or failing. Check the
log; the Mac also shows a notification when a run fails.

## Things to know

- **The Mac runs its own copy of the script**, not the repository's.
  Pushing to the repository can't make it run anything. To update the
  script, read the new version, then run `--install` again.
- **It signs whatever CI built from master**, like any signing setup
  would. Keep master protected.
- **It never runs the builds**; it only signs them.
- **Only the Mac build is signed.** The Linux and Windows files in the
  same release stay as CI built them; the script doesn't touch them.
- **The token can only change this repository's contents and
  releases.** When it expires, the log shows HTTP 401. Make a new one and
  store it with
  `security add-generic-password -U -s me-signer-github -a github -w`.
- **If a key leaks:**
  - Revoke the certificate at developer.apple.com and the API key in App
    Store Connect, then make new ones.
  - A new certificate from the same team keeps the Team ID, so installed
    copies keep updating.
- **Going back to unsigned builds:**
  - Delete the `SIGNED_RELEASES` variable and run `--uninstall`.
  - Copies already on a signed build won't install unsigned updates.
    They have to be replaced by hand.

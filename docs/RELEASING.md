# Releasing Rustgram

Releases are built by `.github/workflows/release.yml` on GitHub-hosted Windows runners and staged as
**draft** GitHub releases. A maintainer publishes each draft by hand. Installed copies only ever see
published releases.

## Versions and channels

| Tag | Example | GitHub release | Updater channel |
| --- | --- | --- | --- |
| Pre-release | `v0.2.0-alpha.1`, `v0.2.0-beta.2` | pre-release | beta |
| Stable | `v0.2.0` | release | stable |

- A stable install is offered stable releases only, unless "Install beta versions" is on in
  Settings > Advanced. A pre-release install is also offered pre-releases (from v0.2.0-alpha.1 on;
  v0.1.0-alpha.2 installs need the switch on to see v0.2.0-alpha.1).
- The updater never installs an older version than the one running (anti-downgrade), and only
  installs packages signed with the release key.
- An update staged in `tupdates` that is not newer than the running version, or whose version cannot
  be read, is deleted when Rustgram starts and never offered (tdesktop's `checkReadyUpdate`). This
  happens after a newer version is installed by hand over a copy that had already downloaded an older
  update. From v0.2.0-alpha.4 `rustgram-updater.exe` also refuses such a package: it installs nothing,
  deletes it, exits with code 1 and starts the installed version again. It compares with the version
  the app passes (`-appversion`), else with the installed `rustgram.exe`'s product version.
- A staged update that is newer is installed at the next start, like tdesktop: instead of opening,
  Rustgram starts `rustgram-updater.exe` (the same way as "Restart to update") and exits, and the
  updater starts the new version (TGB-3187; releases up to v0.2.0-alpha.6 install only on the
  button). `-noupdate` skips this. If the install fails, the updater still starts the installed
  version with `-noupdate`, and the next start does not try that version again
  (`tupdates/startup_attempt`); the menu keeps offering "Restart to update".

## The rules the workflow enforces

1. **Stable ships only a commit that already shipped as a pre-release.** A stable tag `vX.Y.Z` is
   refused unless the same commit carries a pre-release tag `vX.Y.Z-…` whose GitHub release is
   published (not a draft). Ship the commit as an alpha or beta first, let it soak, then tag the same
   commit `vX.Y.Z`. If the commit changed, cut another pre-release.
2. **Smoke test.** After the build, the workflow unpacks the portable zip and runs
   `rustgram.exe --smoke-test -workdir <scratch dir>` twice: on an empty working dir (first start) and
   on a copy of `crates/tg-app/tests/fixtures/smoke_profile_v0_1_0_alpha_2/profile`, a profile written
   by v0.1.0-alpha.2. The mode opens the local storage the way a normal start does (settings, accounts,
   saved authorization, media cache) without a window or network, writes `smoke_test_report.txt` and
   exits non-zero on any failure. The report is printed in the job log. The fixture is never
   regenerated; see its README.
3. **Release notes.** The release body is `.github/release-notes/v<version>.md`. A stable release
   without that file fails before the build. A pre-release without it gets a short default body. A
   notes file that still contains the `RELEASE-NOTES-DRAFT` marker fails the run.
4. **CI must be green.** A tag push, or a run dispatched on the tag, first checks the `CI` workflow
   runs of the tagged commit: it waits while one is still running (up to 90 minutes) and fails unless
   one of them succeeded. Tag only a commit whose CI on `main` is green. Test builds skip the check.
5. **The update path is tested.** After the smoke test, the step "Update the previous published
   release with this package and smoke-test it" runs `cargo run --release -p tg-xtask --
   update-path-check --from-github <repo> --package dist/<.tupdate> ...`. It downloads the portable
   zip of the highest published release below the new version, stages the new package the way that
   release's app does (signature and version checks, unpack into `tupdates/temp`, copy the bundled
   `rustgram-updater.exe` into the install folder), runs the install folder's `rustgram-updater.exe
   -update -justupdate`, and then `rustgram.exe --smoke-test` on the updated copy with the
   v0.1.0-alpha.2 profile. The run fails if the updater fails, if an installed file differs from the
   package, or if the updated copy does not start (`0xC0000135` = a DLL it needs is missing), fails
   the smoke test or reports another version. The child processes get a system-only `PATH`: Windows
   also loads DLLs from `PATH`, and the runner has FFmpeg there. A test build numbered below every
   release (`0.0.0-test.1`) skips this step with a notice; give a test build a version above the
   latest published release (for example `0.2.0-alpha.5.test.1`) to run it. The same command runs
   locally with `--old-zip <zip> --old-version <v>` and `--public-key <hex>` instead of
   `--from-github` (TGB-3180: v0.2.0-alpha.4's package lacked its new FFmpeg DLLs, and copies that
   updated to it did not start).
6. **Test builds never become releases.** A run dispatched on a branch builds, smoke-tests and uploads
   the files as a 7-day workflow artifact. It creates no release (not even a draft) and no attestation.
   Only a tag push, or a run dispatched on the tag itself, creates the draft.

## Checklist

### Every release

1. CI is green on the commit you want to ship.
2. Write `.github/release-notes/v<version>.md` from `.github/release-notes/TEMPLATE.md`: 5 to 7
   highlights in plain words, "Plus N more fixes", the full list in `<details>`. No internal ticket
   ids, no local paths, no account data. Delete the `RELEASE-NOTES-DRAFT` line once reviewed.
3. Push the tag: `git tag v<version> <commit>` and `git push origin v<version>`.
4. Watch the **Release** run. Check the "Smoke test the packaged app" step log: both cases end with
   `result: PASS`. Check the "Update the previous published release" step log: it names the release
   it updated from, lists the staged files (every `.exe` and `.dll` of the portable zip) and ends with
   `PASS: v<old> updated to v<new> starts and opens the profile`. A release that changes runtime files
   (an FFmpeg update renames its DLLs, a new helper exe) ships only with this step green; never
   publish a draft whose run skipped or failed it. Check the "Check FFmpeg and dav1d build ids" step log: it prints the FFmpeg and
   dav1d build ids of the libraries that go into the package (pinned in
   `.github/actions/setup-ffmpeg/action.yml`) and fails the run if they are not the pinned build.
5. Open the draft and check:
   - four files: `Rustgram-Setup-<v>-win-x64.exe`, `rustgram-<v>-win-x64-portable.zip`,
     `rustgram-<v>-win-x64.tupdate`, `SHA256SUMS.txt`;
   - the pre-release box matches the tag (ticked for `-alpha` / `-beta`);
   - the notes render correctly;
   - provenance: download a file and run
     `gh attestation verify <file> -R murmaider227/rustgram`.
6. Publish: `gh release edit v<version> --draft=false` (add `--prerelease` for a pre-release if the
   box was not ticked).
7. The **VirusTotal** workflow runs on publish and appends the scan links to the notes. Check that the
   links appear and look at the detections. If it did not run, start it with
   `gh workflow run virustotal.yml -f tag=v<version>`.
8. Watch new issues and Discussions for a few days before the next step.

### Stable release

1. The commit has a **published** pre-release `vX.Y.Z-…` and no open regression reports against it.
2. Write `.github/release-notes/vX.Y.Z.md` (required).
3. Tag the **same commit**: `git tag vX.Y.Z vX.Y.Z-beta.N^{}` and push the tag.
4. Continue from step 4 above.

### Re-running a failed release build

Dispatch the workflow on the tag, with the same version:
`gh workflow run release.yml --ref v<version> -f version=<version>`. Delete a half-created draft first.

### Test build

`gh workflow run release.yml --ref <branch> -f version=0.0.0-test.1` builds and smoke-tests a branch.
The files are in the run's artifacts; no release is created.

## What the update package contains

`tg-xtask dist` packs every file of the portable folder into the `.tupdate` except the documents
(`LICENSE`, `LEGAL`, `THIRD_PARTY_NOTICES.txt`, `PRIVACY.txt`), and fails if the portable folder holds
an `.exe` or `.dll` that the package lacks. It also fails when the FFmpeg folder holds two versions of
a DLL (`avcodec-62.dll` and `avcodec-63.dll`): point `--ffmpeg-bin` at the build the app was linked
against.

The updater only copies files; it never deletes one. When a release stops shipping a runtime file,
add its name to `OBSOLETE_INSTALL_FILES` in `crates/tg-updater/src/install_files.rs`: the app deletes
those exact names next to `rustgram.exe` at startup, and the installer deletes them too
(`[InstallDelete]`), like tdesktop's `DeleteMyModules`. Nothing else in the folder is touched. `dist`
refuses to ship a file listed there.

## Pull a bad release

Installed copies with automatic updates on check every few hours (8 by default), so act quickly.

1. **Stop new downloads.** Turn the release back into a draft:
   `gh release edit v<version> --draft=true`. Drafts are not visible through the public releases API
   the updater uses, so no client is offered it any more. The tag and files stay for investigation.
   (Deleting the release, `gh release delete v<version>`, has the same effect but loses the files and
   the VirusTotal links.) Clients are then offered the newest remaining published release that is newer
   than what they run.
2. **Tell users.** Pin an issue or post in Discussions: which version, what is wrong, what to do.
3. **Clients that downloaded it but have not restarted** have it staged in `tupdates` inside their
   working folder and it installs at the next restart. It cannot be recalled remotely. Users avoid it by
   quitting Rustgram and deleting the `tupdates` folder in the working folder before starting it again.
4. **Clients that already installed it** stay on it: the updater never downgrades. Fix forward:
   ship a new version with the fix (the next pre-release for a bad pre-release, `X.Y.Z+1` for a bad
   stable release, through a pre-release first as usual). Installed copies pick it up automatically.
   Users who need the old version now can install the previous release's installer over it by hand;
   before recommending that, check that the old version still opens a profile the bad version wrote.
5. **Never re-use the version number** of a pulled release. Leave the tag in place (or delete it) and
   ship the fix under a new number.
6. If the release key may be compromised, also follow `SECURITY.md`: a release signed with a leaked key
   cannot be pulled from clients that trust that key.

## Users stuck on a broken updater

Releases up to and including **v0.2.0-alpha.1** start `rustgram-updater.exe` in the folder of
`rustgram.exe` and do not tell it where the update was downloaded. The update waits in
`tupdates` inside the **working folder**, so whenever the working folder is not the program folder
the updater finds nothing: "Restart to update" reopens the same version and the update button comes
back. The fix shipped in v0.2.0-alpha.2.

Who is affected: a portable copy without its own data folder (working folder `%APPDATA%\Rustgram`), a
portable copy with `RustgramForcePortable` (working folder `<program folder>\RustgramForcePortable`),
and an installed copy moved out of the installer's default folder. The installer's default folder is
`%APPDATA%\Rustgram`, the same as the working folder, so default installs update normally.

**Nothing downloaded yet** (an affected v0.2.0-alpha.1 that has not shown "New version is ready"):
no action. The update package carries the new `rustgram-updater.exe`, and the old app copies it into
the program folder before the restart. The new updater looks in the default working folder by itself
when its current folder holds no update, so "Restart to update" installs v0.2.0-alpha.2 or later.

**An update already downloaded by an affected build** (for example v0.1.0-alpha.2 showing "New
version is ready · v0.2.0-alpha.1"): that update's updater is the broken one. Install it by hand once,
starting the updater in the working folder. In PowerShell:

```powershell
# 1. Quit Rustgram (also from the tray icon). This must print nothing:
Get-Process rustgram -ErrorAction SilentlyContinue

# 2. The folder that holds rustgram.exe, and the working folder
#    (%APPDATA%\Rustgram, or "$app\RustgramForcePortable" if that folder exists):
$app  = "C:\path\to\Rustgram"
$work = "$env:APPDATA\Rustgram"

# 3. An update is staged when this prints True:
Test-Path "$work\tupdates\temp\ready"

# 4. Install it. Exit code 0 means installed; Rustgram then starts again by itself.
$p = Start-Process -FilePath "$app\rustgram-updater.exe" -ArgumentList '-update' `
     -WorkingDirectory $work -Wait -PassThru
$p.ExitCode
```

Exit code 1 means the updater found no update in `$work` (check the two paths), or, with an updater
from v0.2.0-alpha.4 on, that the staged update is not newer than the installed version (the updater
then deletes it). Rustgram now runs the
version that was waiting. That version still has the old updater, but the next update it downloads
(v0.2.0-alpha.2 or later) brings the new updater along and installs the usual way.

Alternative without PowerShell: quit Rustgram, delete the `tupdates` folder in the working folder and
start Rustgram again. It then downloads the newest release (v0.1.0-alpha.x needs **Settings >
Advanced > Install beta versions** on), whose updater installs it at the next restart.

**If neither works**, replace the files by hand: download `rustgram-<version>-win-x64-portable.zip`
from the newest release, quit Rustgram, unzip it over the program folder (replace all files), delete
the `tupdates` folder in the working folder, and start `rustgram.exe`. The profile in the working
folder is not touched.

## An older update offered as ready (v0.2.0-alpha.2)

v0.2.0-alpha.2 offers whatever is staged in `tupdates`, even an older version. After installing
v0.2.0-alpha.2 by hand over a copy that had already downloaded v0.2.0-alpha.1, its menu says
"New version is ready · v0.2.0-alpha.1", and "Restart to update" would install the older version.
Tell users not to restart for it: quit Rustgram, delete the `tupdates` folder in the working folder
(`%APPDATA%\Rustgram` by default) and start Rustgram again. From v0.2.0-alpha.4 the app deletes such
a leftover at start and the updater refuses it, so this needs no action there
(`crates/tg-updater/tests/e2e_tgb3113_workdir.rs`, case `tgb3117_…`).

The steps for stuck users above were tested against the released v0.2.0-alpha.1
`rustgram-updater.exe` with
`crates/tg-updater/tests/e2e_tgb3113_workdir.rs`
(`RUSTGRAM_E2E_RELEASED_UPDATER=<path to that exe> cargo test -p tg-updater --test e2e_tgb3113_workdir`).

## A copy that updated to v0.2.0-alpha.4 does not start

v0.2.0-alpha.4 was published for about 15 minutes on 2026-11-27. Its update package held only the
three executables, while the new `rustgram.exe` needs the new FFmpeg DLLs (`avutil-61.dll` and the
others), so a copy that installed it shows "avutil-61.dll was not found" and does not start.

- **Broken copy:** unzip `rustgram-<newest>-win-x64-portable.zip` over the folder that holds
  `rustgram.exe` (replace all files), or run the newest installer for a copy installed with it. The
  profile is not touched. The new version deletes the old FFmpeg DLLs at its first start.
- **v0.2.0-alpha.4 downloaded but not installed** (the menu says "New version is ready ·
  v0.2.0-alpha.4"): do not restart for it. Quit Rustgram, delete the `tupdates` folder in the working
  folder and start it again; it then downloads the newest release. The releases that could have
  staged it (up to v0.2.0-alpha.6) install a staged update only by "Restart to update", so it stays
  harmless until then.
- Every other copy updates to v0.2.0-alpha.5 or later by itself: from v0.2.0-alpha.5 the package
  carries every runtime file, and the release workflow tests the update from the previous release.

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
4. **Test builds never become releases.** A run dispatched on a branch builds, smoke-tests and uploads
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
   `result: PASS`.
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

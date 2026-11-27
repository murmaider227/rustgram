# Code signing policy

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by
[SignPath Foundation](https://signpath.org).

## What is signed

Only binaries built from this repository's source code by its GitHub Actions release workflow
(`.github/workflows/release.yml`): `rustgram.exe`, `rustgram-updater.exe`, `rustgram-widget-helper.exe` and the
`Rustgram-Setup-<version>-win-x64.exe` installer. Every release is approved by hand before it is signed.
Only runs started by a release tag produce release files; test runs of the workflow on a branch are never
signed or published. A release passes a packaged-app smoke test before its draft is created, and a stable
release ships only a commit that was already published as a pre-release (see
[docs/RELEASING.md](docs/RELEASING.md)).

Third-party libraries shipped inside the installer (for example FFmpeg) are listed in [LEGAL](LEGAL) and are not
signed by this project.

## Team roles

| Role | Members |
|---|---|
| Authors (commit without further review) | [@murmaider227](https://github.com/murmaider227) |
| Reviewers (review outside contributions) | [@murmaider227](https://github.com/murmaider227) |
| Approvers (approve each signing request) | [@murmaider227](https://github.com/murmaider227) |

All members use multi-factor authentication on GitHub and SignPath.

## Privacy

Rustgram sends no data to anyone other than Telegram, GitHub (update checks, which can be turned off) and the
websites you choose to open. See [PRIVACY.md](PRIVACY.md).

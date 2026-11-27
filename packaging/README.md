# Package manager manifests

Prepared manifests for [winget](https://github.com/microsoft/winget-pkgs) and
[Scoop](https://scoop.sh). **Nothing here has been submitted.** Submitting a package is a public
pull request to another project's repository and needs the maintainer's explicit decision first.

| Folder | Package | Release file |
| --- | --- | --- |
| `windows/` | Inno Setup script used for the installer (see also `installer/tgfork.iss`) | `Rustgram-Setup-<version>-win-x64.exe` |
| `winget/manifests/r/Rustgram/Rustgram/<version>/` | winget `Rustgram.Rustgram`, the installer | `Rustgram-Setup-<version>-win-x64.exe` |
| `scoop/rustgram.json` | Scoop `rustgram`, the portable zip | `rustgram-<version>-win-x64-portable.zip` |

Both currently point at **v0.1.0-alpha.2**, with the SHA-256 values GitHub reports for its assets
(the same values as the release's `SHA256SUMS.txt`).

## Before the first submission

Decide these first; each one changes what users get:

1. **Pre-release status.** Every release so far is an alpha. winget and Scoop accept pre-release
   versions, but their users expect a working program: consider waiting for the first beta or
   stable release, or submitting to a Scoop bucket of our own first (below).
2. **Unsigned binaries.** The installer and `rustgram.exe` are not Authenticode-signed yet. The
   winget validation pipeline scans installers, and unsigned ones are more likely to be flagged
   or held for manual review. Signing is tracked in `CODE_SIGNING.md`.
3. **The `tg://` handler.** The installer's "Register tg:// protocol handler" task is selected on
   first install, and a silent winget install takes the defaults, so `winget install` would make
   Rustgram the `tg://` handler. The app itself does not take the handler unless the user opts in.
   To match that, add to the installer entry in `Rustgram.Rustgram.installer.yaml`:

   ```yaml
       InstallerSwitches:
         Custom: /MERGETASKS="!registertg"
   ```

   and drop `Protocols: [tg]`.
4. **Two updaters.** Rustgram updates itself from GitHub releases. With winget that is fine
   (the installer's registry entry is updated, so `winget upgrade` sees the new version). With
   Scoop, the app updates the files inside Scoop's version folder, and `scoop update` later
   installs the release again; harmless, but `scoop list` can show an older version than the one
   running.
5. **Identifier.** `Rustgram.Rustgram` (winget) and `rustgram` (Scoop) are proposals. A winget
   identifier cannot be renamed later without a migration, so settle it before the first PR.

## Updating for a new release

The release workflow publishes `Rustgram-Setup-<v>-win-x64.exe`,
`rustgram-<v>-win-x64-portable.zip` and `SHA256SUMS.txt` under the tag `v<v>`. Get the hashes
without downloading anything:

```sh
gh api repos/murmaider227/rustgram/releases/tags/v<v> --jq '.assets[] | [.name, .digest] | @tsv'
```

**winget**: copy the version folder to `<v>`, then change `PackageVersion` in all three files,
`InstallerUrl`, `InstallerSha256` (upper case, without the `sha256:` prefix), `ReleaseDate` and
`ReleaseNotesUrl`. Or let [wingetcreate](https://github.com/microsoft/winget-create) do it:
`wingetcreate update Rustgram.Rustgram --version <v> --urls <installer URL>` (without `--submit`
until the maintainer decides to submit). Check the result with
`winget validate --manifest packaging/winget/manifests/r/Rustgram/Rustgram/<v>` and, on a test
machine or Windows Sandbox, `winget install --manifest <folder>`.

**Scoop**: `checkver` and `autoupdate` are set, so
`<scoop>/apps/scoop/current/bin/checkver.ps1 -App rustgram -Dir packaging/scoop -Update` rewrites
the version, URL and hash from the newest release and its `SHA256SUMS.txt`. Test with
`scoop install packaging/scoop/rustgram.json`.

## How to submit (only after the maintainer's yes)

**winget**: fork `microsoft/winget-pkgs`, copy `winget/manifests/r/Rustgram/Rustgram/<v>/` to the
same path there, and open a pull request (or run `wingetcreate submit <folder>`). Follow the
[contribution guide](https://github.com/microsoft/winget-pkgs/blob/master/CONTRIBUTING.md); a bot
validates the manifest and installs the package in a sandbox. Later versions can be updated by
the same command or by the community.

**Scoop**: two options.

- Our own bucket (fastest, no review): a public repository named for example
  `murmaider227/scoop-rustgram` with `bucket/rustgram.json`. Users run
  `scoop bucket add rustgram https://github.com/murmaider227/scoop-rustgram` and
  `scoop install rustgram/rustgram`. A scheduled Excavator workflow there can run `checkver`
  automatically.
- The [Extras bucket](https://github.com/ScoopInstaller/Extras): open a pull request adding
  `bucket/rustgram.json`. Check its contribution rules first; Extras is selective about
  pre-release software.

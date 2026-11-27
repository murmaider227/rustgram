<!-- RELEASE-NOTES-DRAFT: delete this line after review. The release workflow refuses notes that still contain it. -->
<!--
Copy to .github/release-notes/v<version>.md (e.g. v0.2.0-alpha.2.md); the release workflow uses it as
the release body. Stable releases must have one.

Rules: plain words a user understands; what changed for them, not how. No internal ticket ids, branch
names, local paths, account names or phone numbers. 5 to 7 highlights, then "Plus N more fixes" where
N is the number of items in the full list that are not highlights. The full list goes in <details>.
-->

One sentence on what this release is about.

## Highlights

- **Short title.** One sentence on what users notice.
- **Short title.** One sentence on what users notice.
- **Short title.** One sentence on what users notice.
- **Short title.** One sentence on what users notice.
- **Short title.** One sentence on what users notice.

Plus N more fixes.

<details>
<summary>All changes</summary>

### New

- …

### Fixed

- …

### Under the hood

- …

</details>

## Install or update

- **Installer:** `Rustgram-Setup-<version>-win-x64.exe`.
- **Portable:** unzip `rustgram-<version>-win-x64-portable.zip` and run `rustgram.exe`.
- Installed copies update themselves (pre-releases: beta channel only).
- Check a download: `gh attestation verify <file> -R murmaider227/rustgram`, or compare it with
  `SHA256SUMS.txt`.

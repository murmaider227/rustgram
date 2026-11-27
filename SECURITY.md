# Security policy

Rustgram implements Telegram's MTProto protocol and its cryptography itself, so security reports are
especially welcome.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Use GitHub's
[private vulnerability reporting](../../security/advisories/new) for this repository instead, and
include:

- what is affected (crate, file, function) and how it can be triggered;
- the impact you expect (key or message disclosure, impersonation, crash, local data exposure);
- a proof of concept or test case, if you have one.

We will acknowledge the report, work on a fix, and credit you in the release notes unless you prefer
otherwise.

## Scope

In scope: the MTProto implementation (`tg-crypto`, `tg-transport`, `tg-mtproto`), session and
login handling, local storage and passcode encryption (`tg-storage`), the updater and its signature
checks (`tg-updater`), Mini App and Instant View sandboxing (`tg-webview`, `tg-iv`), and wallet
signing (`tg-wallet`).

Problems in Telegram's servers or official apps should be reported to Telegram through
[their own program](https://core.telegram.org/bug-bounty).

## Threat model

This section is for people and agents auditing Rustgram or triaging a report. It says what Rustgram
trusts, what is intended, and which boundaries a real finding crosses.

### The model in one paragraph

Rustgram is a desktop Telegram client running as the Windows user who started it. It talks to
Telegram's servers over MTProto 2.0, keeps the session and caches in a local profile folder, and
updates itself from this repository's GitHub releases. It is not a sandbox against its own user or
against other programs running as that user. Security here is about what **the network**, **the
Telegram server**, **other Telegram users and bots** (through messages, media, links, Mini Apps and
Instant View pages) and **the update channel** can make Rustgram do, and whether the local profile
is protected as described below.

### Intended behaviour

**Local profile.** The profile ("working dir") is `-workdir <dir>` if given, otherwise
`RustgramForcePortable\` or a `tdata` folder next to `rustgram.exe` (portable mode), otherwise
`%APPDATA%\Rustgram`. Session keys, settings and caches live in its `tdata` folder in Telegram
Desktop's format. Without a local passcode, the files are encrypted with a key derived from an
empty passcode, exactly as in Telegram Desktop: anyone who can read the folder can use the session.
With a local passcode, the key is derived from the passcode and the session cannot be opened
without it. Logs (`log.txt`, `DebugLogs\`) and crash reports (`tdata\dumps\crash_*.txt`) are plain
text.

**Updater.** Release builds check this repository's GitHub releases (drafts are ignored). An update
is the `.tupdate` file of a release. Before anything is decompressed or written, Rustgram checks
the optional SHA-256 from the release's `SHA256SUMS.txt`, then the package's **Ed25519 signature**
(strict verification) against a public key compiled into the binary, then that the package targets
this OS and architecture, that its channel is allowed (pre-releases only when beta updates are on
or the running build is itself a pre-release), and that its version is newer than the running one
(no downgrades). Only then is it unpacked, with every file name checked against path traversal and
reserved Windows names, into `tupdates\temp` inside the profile. A build without a configured key
does not update at all. The repository and key overrides (`RUSTGRAM_UPDATE_REPO`,
`RUSTGRAM_UPDATE_PUBKEY`) work in debug builds only. `SHA256SUMS.txt` comes from the same release,
so it guards against corruption; the signature is the security boundary. The installer and portable
zip are not covered by the update signature: check them against `SHA256SUMS.txt` and the build
provenance attestation (`gh attestation verify <file> -R murmaider227/rustgram`).

**Telegram server.** As with any Telegram client, the server sees cloud chats and metadata, and
decides what to send. It can send any valid (or invalid) TL object. It must not be able to make
Rustgram run code, read or write files outside what the user chose to send or save, open the local
passcode, or install an update: updates never come from the Telegram server. A network attacker
who is not the Telegram server must not be able to read or forge MTProto traffic: messages are
checked by `msg_key`, session ID and `msg_id` (duplicates and stale IDs are rejected), the server's
Diffie-Hellman parameters are validated, and the server RSA keys are built in.

**tg:// links.** Rustgram does not make itself the system handler for `tg://` and `tonsite://`
by default. The app registers only when the `take_url_schemes` startup preference is set, and
never in `-testmode`, `--demo` or custom `-workdir` runs. The installer offers a "Register tg://
protocol handler" task, which is selected on first install. A link that reaches Rustgram is data:
opening it may show a chat, a bot or a confirmation, as in Telegram Desktop, but must never run a
command or send a message without the user's action. A second Rustgram process forwards links and
`-sendpath` files to the running one over a local named pipe, and a mixed message that tries to
smuggle a file next to a URL is rejected, as in Telegram Desktop.

**Web content.** Mini Apps, Instant View pages, link previews and message entities are untrusted
content from other users, bots and websites. They must not run native code, read local files, or
call Mini App bridge methods beyond what the protocol allows.

### Where the checks live

- MTProto encryption and `msg_key` checks: `tg-crypto` (`auth_key.rs`), used by `tg-mtproto`
  (`message.rs`).
- Sessions, `msg_id` / salts / duplicate handling: `tg-mtproto` (`session.rs`, `received_ids.rs`,
  `salts.rs`).
- Auth key creation, Diffie-Hellman validation, built-in RSA keys: `tg-crypto` (`dh.rs`, `rsa.rs`),
  `tg-mtproto` (`auth_key.rs`).
- Two-step verification (SRP) and Passport secrets: `tg-crypto` (`srp.rs`).
- Local storage, passcode and key wrapping: `tg-storage` (`domain.rs`, `tdf.rs`), `tg-crypto`
  (`local.rs`).
- Updater: `tg-updater` (`package.rs`: `verify_package`, `is_safe_archive_path`,
  `unpack_verified_package`; `checker.rs`: `download_and_stage`; `github.rs`), and the wiring in
  `tg-app` (`startup/updater.rs`: release key and repository).
- Profile folder selection: `tg-core` (`launcher.rs`: `resolve_working_dir`).
- `tg://` registration and single-instance pipe: `tg-app` (`startup/plan.rs`, `startup/ipc.rs`),
  `tg-platform`; installer: `installer/tgfork.iss`.
- Mini App bridge: `tg-webview` (`bridge.rs`); Instant View: `tg-iv`.

### Severity guide

Rate a finding by what the attacker must already have, not by the worst outcome you can name.

- **Critical**: code execution or session/key theft that needs no user action, from the network,
  the Telegram server, another user's message, or the update channel. Examples: a crafted message
  or media file that runs code; an update accepted without a valid signature.
- **High**: the same outcomes with one ordinary user action (opening a chat, clicking a link,
  opening a Mini App), or reading or forging MTProto traffic as a network attacker, or a downgrade
  or path traversal through the updater.
- **Medium**: disclosure of private data (messages, contacts, phone number, local files) to another
  user, a bot or a website; bypass of the local passcode; a link or Mini App acting without the
  confirmation Telegram Desktop shows.
- **Low**: crashes or freezes from remote input without further impact, leaks into local logs or
  crash reports, or problems that need an unusual setup.

Not vulnerabilities on their own: anything that requires code already running as the same Windows
user, or write access to the profile or install folder (the profile without a passcode is readable
by design, see above); what the Telegram server can see as described above; and the user's own
choices (a custom `-workdir`, debug builds with update overrides, registering the `tg://` handler).
Such issues can still be ordinary bugs: report them in public issues.

## Supported versions

Rustgram is pre-release. Only the latest commit on the default branch is supported.

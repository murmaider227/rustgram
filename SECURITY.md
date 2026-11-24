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

## Supported versions

Rustgram is pre-release. Only the latest commit on the default branch is supported.

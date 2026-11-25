# Privacy policy

Rustgram is a Telegram client. It has no servers of its own, no analytics, no telemetry, no advertising
identifiers and no crash-report uploads. It sends data only to the services listed below, and only to do
what you ask it to do.

## Telegram

Everything you do in a Telegram client (signing in, messages, media, calls, contacts, settings) is sent to
Telegram's servers, the same way the official Telegram apps do it. Telegram's own privacy policy applies:
<https://telegram.org/privacy>.

When Telegram's servers cannot be reached directly, Rustgram asks public DNS-over-HTTPS resolvers
(`dns.google.com`, `mozilla.cloudflare-dns.com`) for Telegram's fallback connection settings, as Telegram Desktop
does. Only a fixed Telegram domain name is sent.

## Update checks

Rustgram checks GitHub Releases for this repository (`api.github.com`) for new versions. The request
contains no personal data; GitHub sees your IP address and the client's version. GitHub's privacy statement
applies: <https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement>.

You can turn update checks off in the installer and at any time in **Settings → Advanced → Update
automatically**.

## Websites you open

Links, Instant View pages and Mini Apps you open load content from the websites and bots that provide them.
Their own privacy policies apply.

## Local data

Your session, cache and settings are stored on your computer in Telegram Desktop's `tdata` format,
encrypted with a local key (and your local passcode, if you set one). Uninstalling Rustgram removes the
program but keeps this data folder, so you can delete it yourself.

## Contact

Questions about this policy: open an issue in this repository. Security problems: see
[SECURITY.md](SECURITY.md).

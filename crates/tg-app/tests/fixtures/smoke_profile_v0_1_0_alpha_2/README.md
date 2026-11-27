# Smoke-test profile from v0.1.0-alpha.2 — never regenerate

`profile/` is a Rustgram working folder written by the storage code that shipped as
**v0.1.0-alpha.2** (the `tg-storage`, `tg-core` and `tg-crypto` crates of that release, with
rusqlite 0.32.1 / its bundled SQLite). The release workflow and the tests open a copy of it with the
current code through `rustgram --smoke-test -workdir <copy>`, which proves that a new release still
reads a profile left by an old one.

**Do not regenerate, re-save or "fix" these files.** A profile written by today's code proves nothing
about reading old profiles. If a storage change breaks this fixture, the change must learn to read it
(that is the migration users need). To cover a newer format, add a second fixture next to this one.
`fixture_is_frozen` in `crates/tg-app/src/startup/smoke_test_tests.rs` pins the SHA-256 of every file,
and `.gitattributes` keeps git from changing line endings.

## Contents

All values are synthetic. The MTProto keys are byte patterns, not real keys; the user id is an
arbitrary number; there are no messages, contacts or phone numbers.

| Path | What | Written by |
| --- | --- | --- |
| `tdata/settingss` | global settings: scale 125%, core settings with song volume 0.5 and adaptive layout off | `LocalStorage`, `CoreSettings::save_to_local_storage` |
| `tdata/key_datas` | domain key data, no passcode, one account in slot 0 | `TdataDomainStorage` |
| `tdata/D877F783D5D3EF8Cs` | slot 0 MTProto authorization: user 1000001, main DC 2, keys for DC 2 and DC 4 | `write_account_mtp_auth` |
| `tdata/D877F783D5D3EF8C/` | slot 0 account map and preferences (media cache generation `a1b2c3d4e5f60718`) | `AccountStorage::write_prefs` |
| `tdata/media_cache/D877F783D5D3EF8C-a1b2c3d4e5f60718/` | encrypted media cache (schema v2): `photo:1001` (19 bytes) and `sticker:2002` (4096 bytes of `7`) | `MediaCacheIndex::open_with_key` + `put_blob` |
| `tdata/startup_prefs.json` | startup preferences, "Install beta versions" on | `StartupPrefs` JSON layout of that release |
| `shell_state.json` | window and theme state, night theme | `ShellSettings` JSON layout of that release |

What the smoke test expects from it: one signed-in account (slot 0, main DC 2, 2 auth keys) whose media
cache still holds 2 entries after opening, and both JSON files parse without falling back to defaults.

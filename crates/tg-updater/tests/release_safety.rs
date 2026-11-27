//! Release safety: what a published GitHub release can and cannot push to installed clients.
//!
//! - Clients list releases through the public, unauthenticated releases API. GitHub returns draft
//!   releases only to authenticated users with push access, so drafts staged by the release
//!   workflow are invisible to clients; a draft that shows up anyway is skipped.
//! - The signed package must carry exactly the version of the release it was listed under, so a
//!   wrong file attached to a release is refused before anything is staged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use tempfile::TempDir;
use tg_updater::UpdateError;
use tg_updater::checker::{UpdateChecker, UpdateConfig, has_ready_update};
use tg_updater::github::{DEFAULT_GITHUB_API_BASE, HttpClient};
use tg_updater::package::pack_update;
use tg_updater::version::{Target, UpdateChannel, Version};

const REPO: &str = "example/rustgram";

/// Every request seen: URL and headers.
type RequestLog = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;

/// In-memory GitHub: fixed responses per URL, every request recorded with its headers.
#[derive(Default)]
struct FakeGitHub {
    routes: HashMap<String, Vec<u8>>,
    requests: RequestLog,
}

impl HttpClient for FakeGitHub {
    fn get_bytes(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        max_bytes: usize,
    ) -> Result<Vec<u8>, UpdateError> {
        if let Ok(mut log) = self.requests.lock() {
            log.push((
                url.to_owned(),
                headers
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            ));
        }
        let body = self
            .routes
            .get(url)
            .ok_or_else(|| UpdateError::Network(format!("404 {url}")))?;
        if body.len() > max_bytes {
            return Err(UpdateError::ResponseTooLarge { limit: max_bytes });
        }
        Ok(body.clone())
    }
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn releases_url() -> String {
    format!("{DEFAULT_GITHUB_API_BASE}/repos/{REPO}/releases?per_page=20")
}

fn release_json(tag: &str, draft: bool, prerelease: bool, asset: &str, url: &str) -> String {
    format!(
        r#"{{"tag_name": "{tag}", "name": "Rustgram {tag}", "draft": {draft}, "prerelease": {prerelease},
            "body": "notes", "assets": [{{"name": "{asset}", "browser_download_url": "{url}", "size": 100}}]}}"#
    )
}

fn config(workdir: &std::path::Path, current: &str) -> UpdateConfig {
    UpdateConfig {
        github_repo: REPO.into(),
        api_base_url: None,
        current_version: Version::parse(current).expect("version"),
        allow_beta: true,
        target: Target::WIN_X64,
        working_dir: workdir.join("wd"),
        install_dir: workdir.join("install"),
        verifying_key: signing_key().verifying_key(),
    }
}

#[test]
fn checker_requests_carry_no_credentials() {
    let gh = FakeGitHub {
        routes: HashMap::from([(releases_url(), b"[]".to_vec())]),
        ..FakeGitHub::default()
    };
    let requests = Arc::clone(&gh.requests);
    let tmp = TempDir::new().expect("tempdir");
    let checker = UpdateChecker::new(gh, config(tmp.path(), "0.1.0"));
    checker.check().expect("check");
    let log = requests.lock().expect("log");
    assert_eq!(log.len(), 1);
    let (url, headers) = &log[0];
    assert_eq!(url, &releases_url(), "only the public releases listing");
    assert!(
        headers
            .iter()
            .all(|(k, _)| !k.eq_ignore_ascii_case("authorization")),
        "an authenticated listing would include drafts: {headers:?}"
    );
}

#[test]
fn a_draft_in_the_listing_is_never_offered() {
    let tmp = TempDir::new().expect("tempdir");
    let asset = format!("rustgram-0.9.0-{}.tupdate", Target::WIN_X64.asset_suffix());
    let listing = format!(
        "[{}]",
        release_json("v0.9.0", true, false, &asset, "https://example.invalid/a")
    );
    let gh = FakeGitHub {
        routes: HashMap::from([(releases_url(), listing.into_bytes())]),
        ..FakeGitHub::default()
    };
    let checker = UpdateChecker::new(gh, config(tmp.path(), "0.1.0"));
    assert_eq!(checker.check().expect("check"), None);
}

#[test]
fn package_signed_for_another_version_is_refused_before_staging() {
    let tmp = TempDir::new().expect("tempdir");
    let target = Target::WIN_X64;
    // Release v0.3.0 carries a validly signed package that declares 0.2.5 (a wrong file attached).
    let pkg = pack_update(
        UpdateChannel::Stable,
        target,
        &Version::parse("0.2.5").expect("version"),
        1_760_000_000,
        [("rustgram.exe", b"wrong-release-binary".as_slice())],
        &signing_key(),
    )
    .expect("pack");
    let asset = format!("rustgram-0.3.0-{}.tupdate", target.asset_suffix());
    let pkg_url = "https://example.invalid/download/v0.3.0.tupdate";
    let listing = format!(
        "[{}]",
        release_json("v0.3.0", false, false, &asset, pkg_url)
    );
    let gh = FakeGitHub {
        routes: HashMap::from([
            (releases_url(), listing.into_bytes()),
            (pkg_url.to_owned(), pkg),
        ]),
        ..FakeGitHub::default()
    };
    let cfg = config(tmp.path(), "0.1.0");
    let workdir = cfg.working_dir.clone();
    let checker = UpdateChecker::new(gh, cfg);
    let err = checker.check_and_stage().expect_err("must be refused");
    assert!(
        matches!(err, UpdateError::ReleaseVersionMismatch { ref release, ref package }
            if release == "0.3.0" && package == "0.2.5"),
        "got {err:?}"
    );
    assert!(!has_ready_update(&workdir));
}

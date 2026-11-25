//! Window-level security policy helpers for hidden links and external file launches.
//!
//! Port origin:
//! - `Telegram/SourceFiles/core/click_handler_types.cpp:504-655` (`HiddenUrlClickHandler::Open`)
//! - `Telegram/SourceFiles/data/data_document_resolver.cpp:91-207` (`Data::LaunchWithWarning`,
//!   `Data::LauncherWouldWarn`)

use std::collections::BTreeSet;

use tg_app_core::media::backend::MediaItemId;
use tg_app_info::boxes::security_confirm::{
    HiddenLinkConfirmBoxState, LaunchFileConfirmBoxState, LaunchWarningKind,
};
use tg_media::document::{NameType, detect_name_type, file_extension, is_ip_revealing_filename};
use tg_platform::file_safety::sanitize_filename;
use tg_platform::url_policy::{
    extract_url_host_range, find_suspicious_host_ranges, hidden_url_requires_confirmation,
    validate_external_url,
};

/// Outcome of evaluating an external URL before queuing [`WebViewHostRequest::OpenExternalUrl`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalLinkAction {
    /// Allowed URL that may open immediately without confirmation.
    OpenNow(String),
    /// Allowed hidden/external URL that requires the `"Open this link?"` confirmation modal.
    Confirm {
        /// Validated destination URL.
        url: String,
        /// Byte range of host inside `url` for bolding.
        host_range: Option<(usize, usize)>,
        /// Suspicious mixed-script ranges inside `url`'s host.
        suspicious_ranges: Vec<(usize, usize)>,
    },
    /// Disallowed scheme, local path, or UNC share — must NEVER be opened.
    Rejected,
}

/// Evaluates an external URL against the shared URL policy (`tg_platform::url_policy`).
///
/// - When `require_confirmation` is `false`, allowed URLs return [`ExternalLinkAction::OpenNow`].
/// - When `require_confirmation` is `true` and `force_confirm` is `false`, trusted Telegram
///   domains (`telegram.org`, `telegra.ph`, `graph.org`, `telesco.pe`, `fragment.com`) over
///   `http`/`https` return `OpenNow`, while all other allowed URLs return `Confirm`.
/// - When `force_confirm` is `true` (e.g. sandboxed widget `LinkPolicyDecision::ConfirmExternal`),
///   every allowed URL returns `Confirm`.
/// - Any disallowed scheme (`file:`, `ms-msdt:`, `javascript:`, etc.), UNC path (`\\host\share`),
///   or local path (`C:\...`) ALWAYS returns [`ExternalLinkAction::Rejected`].
#[must_use]
pub fn evaluate_external_link(
    raw_url: &str,
    require_confirmation: bool,
    force_confirm: bool,
) -> ExternalLinkAction {
    let Ok(validated) = validate_external_url(raw_url) else {
        return ExternalLinkAction::Rejected;
    };
    if force_confirm || (require_confirmation && hidden_url_requires_confirmation(&validated)) {
        let host_range = extract_url_host_range(&validated);
        let suspicious_ranges = find_suspicious_host_ranges(&validated);
        ExternalLinkAction::Confirm {
            url: validated,
            host_range,
            suspicious_ranges,
        }
    } else {
        ExternalLinkAction::OpenNow(validated)
    }
}

/// Builds a [`HiddenLinkConfirmBoxState`] for a validated URL.
#[must_use]
pub fn build_hidden_link_confirm_box(
    url: String,
    host_range: Option<(usize, usize)>,
    suspicious_ranges: Vec<(usize, usize)>,
) -> HiddenLinkConfirmBoxState {
    HiddenLinkConfirmBoxState::new(url, host_range, suspicious_ranges)
}

/// Classifies whether launching `file_name` externally requires a user confirmation box
/// (`Data::LaunchWithWarning` + `Data::LauncherWouldWarn`).
///
/// - Extensionless files ALWAYS warn (and cannot be suppressed).
/// - IP-revealing extensions (`.svg`, `.html`, `.m3u`, ...) warn when `ip_reveal_warning` is `true`.
/// - Executable (`NameType::Executable`) and unknown (`NameType::Unknown`) extensions warn unless
///   present in `no_warning_extensions`.
#[must_use]
pub fn classify_launch_warning(
    file_name: &str,
    no_warning_extensions: &BTreeSet<String>,
    ip_reveal_warning: bool,
) -> Option<LaunchWarningKind> {
    let sanitized = sanitize_filename(file_name);
    let ext = file_extension(&sanitized).to_ascii_lowercase();
    if ext.is_empty() {
        return Some(LaunchWarningKind::Extensionless);
    }
    if is_ip_revealing_filename(&sanitized) && ip_reveal_warning {
        return Some(LaunchWarningKind::IpRevealing { extension: ext });
    }
    match detect_name_type(&sanitized) {
        NameType::Executable => {
            if no_warning_extensions.contains(&ext) {
                None
            } else {
                Some(LaunchWarningKind::Executable { extension: ext })
            }
        }
        NameType::Unknown => {
            if no_warning_extensions.contains(&ext) {
                None
            } else {
                Some(LaunchWarningKind::UnknownExtension { extension: ext })
            }
        }
        NameType::Image
        | NameType::Video
        | NameType::Audio
        | NameType::Document
        | NameType::Archive
        | NameType::ThemeFile
        | NameType::OtherBenign => None,
    }
}

/// Builds a [`LaunchFileConfirmBoxState`] with a sanitized display filename.
#[must_use]
pub fn build_launch_file_confirm_box(
    item_id: MediaItemId,
    file_name: &str,
    kind: LaunchWarningKind,
) -> LaunchFileConfirmBoxState {
    let safe_name = sanitize_filename(file_name);
    LaunchFileConfirmBoxState::new(item_id, safe_name, kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_link_policy_rejects_dangerous_and_confirms_hidden() {
        assert_eq!(
            evaluate_external_link("file:///C:/Windows/calc.exe", true, false),
            ExternalLinkAction::Rejected
        );
        assert_eq!(
            evaluate_external_link(r"\\host\share\x.exe", true, false),
            ExternalLinkAction::Rejected
        );
        assert_eq!(
            evaluate_external_link("ms-msdt:/id PCWDiagnostic", true, false),
            ExternalLinkAction::Rejected
        );
        assert!(matches!(
            evaluate_external_link("https://evil.example/login", true, false),
            ExternalLinkAction::Confirm { .. }
        ));
        assert_eq!(
            evaluate_external_link("https://telegram.org/faq", true, false),
            ExternalLinkAction::OpenNow("https://telegram.org/faq".to_owned())
        );
        // Widget `ConfirmExternal` forces confirmation even for allowlisted hosts.
        assert!(matches!(
            evaluate_external_link("https://telegram.org/faq", true, true),
            ExternalLinkAction::Confirm { .. }
        ));
    }

    #[test]
    fn launch_warning_classification_matches_tdesktop() {
        let empty = BTreeSet::new();
        assert_eq!(
            classify_launch_warning("x.exe", &empty, true),
            Some(LaunchWarningKind::Executable {
                extension: "exe".to_owned()
            })
        );
        assert_eq!(
            classify_launch_warning(r"..\..\Startup\run.bat", &empty, true),
            Some(LaunchWarningKind::Executable {
                extension: "bat".to_owned()
            })
        );
        assert_eq!(
            classify_launch_warning("README", &empty, true),
            Some(LaunchWarningKind::Extensionless)
        );
        assert_eq!(
            classify_launch_warning("chart.svg", &empty, true),
            Some(LaunchWarningKind::IpRevealing {
                extension: "svg".to_owned()
            })
        );
        assert_eq!(classify_launch_warning("photo.jpg", &empty, true), None);

        let mut trusted = BTreeSet::new();
        trusted.insert("exe".to_owned());
        assert_eq!(classify_launch_warning("x.exe", &trusted, true), None);
        assert_eq!(
            classify_launch_warning("README", &trusted, true),
            Some(LaunchWarningKind::Extensionless)
        );
    }
}

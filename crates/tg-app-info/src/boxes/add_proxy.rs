//! "Add Proxy" confirmation dialog box (`ProxiesBoxController::ShowApplyConfirmation`).
//!
//! Port origin:
//! - `Telegram/SourceFiles/boxes/connection_box.cpp` (`ShowApplyConfirmation`)
//! - `Telegram/SourceFiles/core/local_url_handlers.cpp` (`ApplySocksProxy`, `ApplyMtprotoProxy`,
//!   `ApplyWebProxy`)
//!
//! Opened when the user clicks or launches a `tg://proxy?...`, `tg://socks?...`,
//! `tg://webproxy?...`, `https://t.me/proxy?...`, `https://t.me/socks?...`, or
//! `https://t.me/webproxy?...` link.

use egui::Ui;
use tg_core::CoreSettings;
use tg_core::local_url::{LocalUrlRoute, parse_local_url};
use tg_core::settings_proxy::{
    ProxyData as CoreProxyData, ProxySettingsMode, ProxyType as CoreProxyType,
};
use tg_lang::tr;
use tg_transport::{ProxyData as TransportProxyData, ProxyStatus};
use tg_ui::fonts::font_id_from_style;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::Colors;
use tg_ui::tg_style::st;

use crate::boxes::BoxKind;
use tg_app_core::core_types::misc::{core_to_transport_proxy, transport_to_core_proxy};
use tg_app_core::settings::backend::{ProxyCheckStatus, SettingsBackend};

/// State for the modal "Add Proxy" confirmation box (`ShowApplyConfirmation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddProxyBoxState {
    /// Parsed proxy parameters.
    pub proxy: CoreProxyData,
    /// Original URL that opened this box (for display/debugging).
    pub source_url: String,
    /// Validation error when the link has missing/invalid host, port, or secret.
    pub validation_error: Option<String>,
    /// Optional reachability check status if probed while the dialog is open.
    pub check_status: ProxyCheckStatus,
}

impl AddProxyBoxState {
    /// Constructs an [`AddProxyBoxState`] from a parsed [`CoreProxyData`], validating its
    /// fields through `tg-transport`'s `ProxyData::status()`.
    #[must_use]
    pub fn from_proxy(proxy: CoreProxyData, source_url: impl Into<String>) -> Self {
        let tp = core_to_transport_proxy(&proxy);
        let validation_error = match tp.status() {
            ProxyStatus::Valid => None,
            ProxyStatus::IncorrectSecret => {
                Some("Invalid MTProto proxy secret (expected valid hex or base64url).".to_owned())
            }
            ProxyStatus::Unsupported => Some(if proxy.proxy_type == CoreProxyType::Web {
                "Web proxy links do not support fake-TLS (ee) or unknown secret formats.".to_owned()
            } else {
                "This proxy link uses a secret format that is not supported by this version."
                    .to_owned()
            }),
            ProxyStatus::Invalid => Some(if proxy.proxy_type == CoreProxyType::Web {
                "This web proxy link is missing a valid HTTPS domain, base path, or secret."
                    .to_owned()
            } else {
                "This proxy link is missing a valid server address or port.".to_owned()
            }),
        };
        Self {
            proxy,
            source_url: source_url.into(),
            validation_error,
            check_status: ProxyCheckStatus::NotTested,
        }
    }

    /// Parses a `tg://proxy`, `tg://socks`, `tg://webproxy`, `t.me/proxy`, `t.me/socks`, or
    /// `t.me/webproxy` link into an [`AddProxyBoxState`]. Returns `None` if `url` is not a proxy link.
    #[must_use]
    pub fn from_url(url: &str) -> Option<Self> {
        if let Ok(tp) = TransportProxyData::from_link(url) {
            let core = transport_to_core_proxy(&tp);
            return Some(Self::from_proxy(core, url));
        }
        let route = parse_local_url(url)?;
        match route {
            LocalUrlRoute::Proxy(parsed) => {
                let proxy = CoreProxyData {
                    proxy_type: parsed.proxy_type,
                    host: parsed.host,
                    port: parsed.port,
                    user: parsed.user,
                    password: parsed.password,
                };
                Some(Self::from_proxy(proxy, url))
            }
            _ => None,
        }
    }

    /// Returns `true` when this proxy link passed validation and can be connected.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.validation_error.is_none()
    }

    /// Synchronizes `self.check_status` with `backend` (and kicks off a reachability probe
    /// if the proxy is valid and not yet tested).
    pub fn sync_check_status(&mut self, backend: &mut dyn SettingsBackend) {
        if !self.is_valid() {
            return;
        }
        let current = backend.proxy_check_status(&self.proxy);
        if matches!(current, ProxyCheckStatus::NotTested) {
            backend.check_proxy(&self.proxy);
            self.check_status = backend.proxy_check_status(&self.proxy);
        } else {
            self.check_status = current;
        }
    }

    /// Returns the human-readable protocol label (`"SOCKS5"`, `"MTProto"`, `"HTTP"`, `"Web"`).
    #[must_use]
    pub const fn type_label(&self) -> &'static str {
        match self.proxy.proxy_type {
            CoreProxyType::Socks5 => "SOCKS5",
            CoreProxyType::Mtproto => "MTProto",
            CoreProxyType::Http => "HTTP",
            CoreProxyType::Web => "Web",
            CoreProxyType::None => "Direct",
        }
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor matching tdesktop's `ShowApplyConfirmation`.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        let title = match self.proxy.proxy_type {
            CoreProxyType::Mtproto => "MTProto Proxy",
            CoreProxyType::Socks5 => "SOCKS5 Proxy",
            CoreProxyType::Http => "HTTP Proxy",
            CoreProxyType::Web => "Web Proxy",
            CoreProxyType::None => "Proxy Server",
        };
        let mut modal = BoxModal::new(id, title).width(360.0);
        if self.is_valid() {
            modal = modal
                .add_button(BoxButton::default_btn("cancel", tr::lng_cancel()))
                .add_button(BoxButton::default_btn("connect", "Connect Proxy"));
        } else {
            modal = modal.add_button(BoxButton::default_btn("close", tr::lng_close()));
        }
        modal
    }

    /// The details table rows (`label`, `value`) shown in the box body.
    #[must_use]
    pub fn detail_rows(&self) -> Vec<(&'static str, String)> {
        let server_display =
            if self.proxy.proxy_type == CoreProxyType::Web && !self.proxy.user.is_empty() {
                format!("{}/{}", self.proxy.host, self.proxy.user)
            } else {
                self.proxy.host.clone()
            };
        let mut v = vec![
            ("Type", self.type_label().to_owned()),
            ("Server", server_display),
            ("Port", self.proxy.port.to_string()),
        ];
        match self.proxy.proxy_type {
            CoreProxyType::Socks5 | CoreProxyType::Http => {
                if !self.proxy.user.is_empty() {
                    v.push(("Username", self.proxy.user.clone()));
                }
                if !self.proxy.password.is_empty() {
                    let masked = "•".repeat(self.proxy.password.chars().count().min(12));
                    v.push(("Password", masked));
                }
            }
            CoreProxyType::Mtproto | CoreProxyType::Web => {
                if self.proxy.proxy_type == CoreProxyType::Web && !self.proxy.user.is_empty() {
                    v.push(("Path", format!("/{}/", self.proxy.user)));
                }
                if !self.proxy.password.is_empty() {
                    // The secret comes from a link and need not be ASCII: cut on characters.
                    let preview = match self.proxy.password.char_indices().nth(24) {
                        Some((cut, _)) => format!("{}…", &self.proxy.password[..cut]),
                        None => self.proxy.password.clone(),
                    };
                    v.push(("Secret", preview));
                }
            }
            CoreProxyType::None => {}
        }
        if !matches!(self.check_status, ProxyCheckStatus::NotTested) {
            v.push(("Status", self.check_status.format_label(false)));
        }
        v
    }

    /// Renders the proxy details table and optional validation warning inside the modal body.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) {
        let normal = font_id_from_style(st::normal_font);
        let semibold = font_id_from_style(st::semibold_font);
        let fg = colors.color("windowFg");
        let sub_fg = colors.color("windowSubTextFg");
        let err_fg = colors.color("boxTextFgError");

        if let Some(err) = &self.validation_error {
            ui.label(egui::RichText::new(err).font(normal.clone()).color(err_fg));
            ui.add_space(8.0);
        } else {
            ui.label(
                egui::RichText::new(
                    "Do you want to add and connect to the following proxy server?",
                )
                .font(normal.clone())
                .color(sub_fg),
            );
            ui.add_space(10.0);
        }

        let rows = self.detail_rows();

        for (label, value) in rows {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{label}:"))
                        .font(semibold.clone())
                        .color(sub_fg),
                );
                ui.add_space(6.0);
                ui.label(egui::RichText::new(value).font(normal.clone()).color(fg));
            });
            ui.add_space(4.0);
        }
    }
}

/// Routes a `tg://proxy`, `tg://socks`, `https://t.me/proxy`, or `https://t.me/socks` URL into
/// an [`AddProxyBoxState`] modal ready to push onto [`crate::boxes::BoxHost`].
#[must_use]
pub fn route_proxy_link(url: &str) -> Option<BoxKind> {
    AddProxyBoxState::from_url(url).map(BoxKind::AddProxy)
}

/// Applies a proxy confirmed by the user in [`AddProxyBoxState`] to `core` and `backend`
/// (adds to list if not present, selects it, enables custom proxy mode, persists & reconnects).
pub fn apply_confirmed_proxy(
    proxy: CoreProxyData,
    core: &mut CoreSettings,
    backend: &mut dyn SettingsBackend,
) -> Result<(), String> {
    if core.proxy.index_in_list(&proxy).is_none() {
        core.proxy.add_to_list(proxy.clone());
    }
    core.proxy.set_selected(proxy);
    core.proxy.set_settings(ProxySettingsMode::Enabled);
    backend.apply_proxy_settings(&core.proxy)
}

#[cfg(test)]
#[path = "add_proxy_tests.rs"]
#[cfg(test)]
mod tests;

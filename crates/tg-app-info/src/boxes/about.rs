//! About Rustgram modal box displaying version, GPL-3.0 notice, and link to Telegram Desktop.
//!
//! Port origin: `Telegram/SourceFiles/boxes/about_box.h`
//! and `Telegram/SourceFiles/boxes/about_box.cpp`.

use egui::Ui;
use tg_lang::tr;
use tg_ui::layers::{BoxButton, BoxModal};
use tg_ui::stand_in::{Colors, FlatLabelStyle, LinkButtonStyle};
use tg_ui::text::RichText;
use tg_ui::tg_style::st;
use tg_ui::widgets::buttons::LinkButton;
use tg_ui::widgets::labels::FlatLabel;

use crate::boxes::strings;

pub use tg_app_core::core_types::misc::{
    GPL3_LICENSE_URL, TELEGRAM_API_URL, TGFORK_VERSION, UPSTREAM_TDESKTOP_URL,
};

/// State for the About Rustgram modal (`AboutBox`).
#[derive(Debug, Clone, Default)]
pub struct AboutBoxState {
    /// Last clicked URL (surfaced to the host for toast/open handling).
    pub last_clicked_url: Option<String>,
}

impl AboutBoxState {
    /// Creates a new About Rustgram modal state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the formatted Rustgram version string.
    #[must_use]
    pub fn version_line() -> String {
        format!(
            "{} {}",
            tr::lng_about_version(TGFORK_VERSION),
            strings::ABOUT_VERSION_SUFFIX
        )
    }

    /// Builds the `tg-ui` [`BoxModal`] descriptor.
    #[must_use]
    pub fn to_modal(&self, id: &str) -> BoxModal {
        BoxModal::new(id, strings::ABOUT_TITLE)
            .width((st::about_width.0 as f32).max(st::box_wide_width.0 as f32 + 20.0))
            .add_button(BoxButton::default_btn("close", tr::lng_close()))
    }

    /// Renders the About box contents and returns any clicked link URL.
    pub fn show_body(&mut self, ui: &mut Ui, colors: &dyn Colors) -> Option<String> {
        let mut clicked = None;
        let link_st = LinkButtonStyle::box_link(colors);

        if LinkButton::new(&Self::version_line(), link_st)
            .show(ui)
            .clicked()
        {
            clicked = Some(UPSTREAM_TDESKTOP_URL.to_owned());
        }

        ui.add_space(12.0);
        let p1 = RichText::new()
            .push_plain(strings::ABOUT_INTRO_PREFIX)
            .push_link(strings::ABOUT_TDESKTOP_NAME, UPSTREAM_TDESKTOP_URL)
            .push_plain(strings::ABOUT_INTRO_MID)
            .push_link(&tr::lng_about_text1_api(), TELEGRAM_API_URL)
            .push_plain(strings::ABOUT_INTRO_SUFFIX);
        let r1 = FlatLabel::rich(p1, FlatLabelStyle::box_label(colors))
            .id_salt("about_p1")
            .show(ui);
        if let Some(url) = r1.clicked_link {
            clicked = Some(url);
        }

        ui.add_space(10.0);
        let p2 = RichText::new()
            .push_plain(strings::ABOUT_LICENSE_PREFIX)
            .push_link(strings::ABOUT_GPL_LABEL, GPL3_LICENSE_URL)
            .push_plain(strings::ABOUT_LICENSE_MID)
            .push_link(strings::ABOUT_GITHUB_LABEL, UPSTREAM_TDESKTOP_URL)
            .push_plain(".");
        let r2 = FlatLabel::rich(p2, FlatLabelStyle::box_label(colors))
            .id_salt("about_p2")
            .show(ui);
        if let Some(url) = r2.clicked_link {
            clicked = Some(url);
        }

        ui.add_space(10.0);
        FlatLabel::plain(strings::ABOUT_NOTICE, FlatLabelStyle::sub_text(colors))
            .id_salt("about_p3")
            .show(ui);

        if clicked.is_some() {
            self.last_clicked_url = clicked.clone();
        }
        clicked
    }
}

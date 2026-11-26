//! Whether a message's web page media is drawn as a link preview block.
//!
//! Port origin: `CreateMedia` (`history/history_item.cpp`), which creates no media for
//! `webPageEmpty`, and `HistoryView::WebPage::countOptimalSize`
//! (`history/view/media/history_view_web_page.cpp`), which gives a pending page an empty size.
//! The site name and title lines follow `WebPageData::displayedSiteName` (`data/data_web_page.cpp`)
//! and the `title` / `author` choice in `WebPage::countOptimalSize`.

use tg_data::{DocumentKind, WebPageData};

/// Site-name line of the preview block (`WebPageData::displayedSiteName`): the page's own site
/// name, "Chat Wallpaper" for a wallpaper page, or empty, in which case no site-name line is drawn.
#[must_use]
pub fn displayed_site_name(page: &WebPageData) -> String {
    if page
        .document
        .as_ref()
        .is_some_and(|doc| doc.kind == DocumentKind::WallPaper)
    {
        return tg_lang::tr::lng_media_chat_background();
    }
    page.site_name.clone().unwrap_or_default()
}

/// Title line of the preview block: the page title, else its author, else empty (no title line).
#[must_use]
pub fn displayed_title(page: &WebPageData) -> String {
    page.title
        .as_deref()
        .filter(|t| !t.is_empty())
        .or(page.author.as_deref())
        .map(tg_text::single_line)
        .unwrap_or_default()
}

/// `true` when tdesktop draws a preview block for `page`.
///
/// A pending page (the server is still preparing it, `WebPageData::pendingTill`) draws nothing
/// until `updateWebPage` replaces it. A page with no id and no url is `webPageEmpty` as older
/// stored messages kept it: no media at all.
#[must_use]
pub fn web_page_is_drawn(page: &WebPageData) -> bool {
    if page.is_pending() {
        return false;
    }
    page.id.0 != 0 || !page.url.is_empty()
}

#[cfg(test)]
mod tests {
    use tg_data::{
        Message, MessageContent, MessageMedia, MsgId, PeerId, Session, TextWithEntities, UserId,
        WebPageData, WebPageId,
    };

    use super::web_page_is_drawn;
    use crate::convert::message_to_history_message;

    fn link_message(page: WebPageData) -> Message {
        let peer = PeerId::from_user(UserId(7));
        let mut msg = Message::new_text(
            peer,
            MsgId(10),
            Some(peer),
            1_700_000_000,
            TextWithEntities::plain("https://t.me/c/1234567890/42"),
        );
        if let MessageContent::Regular { media, .. } = &mut msg.content {
            *media = MessageMedia::WebPage(Box::new(page));
        }
        msg
    }

    fn real_page() -> WebPageData {
        WebPageData {
            id: WebPageId(900),
            url: "https://example.org/post".to_owned(),
            display_url: "example.org/post".to_owned(),
            kind: "article".to_owned(),
            site_name: Some("Example".to_owned()),
            title: Some("Title".to_owned()),
            ..WebPageData::default()
        }
    }

    #[test]
    fn empty_and_pending_pages_are_not_drawn() {
        assert!(!web_page_is_drawn(&WebPageData::default()));
        let pending = WebPageData {
            id: WebPageId(555),
            url: "https://t.me/c/1234567890/42".to_owned(),
            kind: WebPageData::KIND_PENDING.to_owned(),
            ..WebPageData::default()
        };
        assert!(!web_page_is_drawn(&pending));
        assert!(web_page_is_drawn(&real_page()));
    }

    #[test]
    fn a_message_with_an_empty_or_pending_page_gets_no_link_preview() {
        let session = Session::new(UserId(1));
        let empty = message_to_history_message(&session, &link_message(WebPageData::default()));
        assert!(empty.extra.link_preview.is_none());

        let pending = WebPageData {
            id: WebPageId(555),
            url: "https://t.me/c/1234567890/42".to_owned(),
            kind: WebPageData::KIND_PENDING.to_owned(),
            ..WebPageData::default()
        };
        let pending = message_to_history_message(&session, &link_message(pending));
        assert!(pending.extra.link_preview.is_none());

        let real = message_to_history_message(&session, &link_message(real_page()));
        assert!(real.extra.link_preview.is_some());
    }

    /// TGB-2960: a page without a site name or title gets no invented "Link" / display URL / URL
    /// text; the title falls back to the author only, as `WebPage::countOptimalSize` does.
    #[test]
    fn missing_site_name_and_title_stay_empty() {
        let session = Session::new(UserId(1));
        let bare = WebPageData {
            site_name: None,
            title: None,
            description: Some("Body".to_owned()),
            ..real_page()
        };
        let lp = message_to_history_message(&session, &link_message(bare))
            .extra
            .link_preview
            .expect("real page is drawn");
        assert_eq!(lp.site_name, "");
        assert_eq!(lp.title, "");
        assert_eq!(lp.description, "Body");

        let authored = WebPageData {
            title: Some(String::new()),
            author: Some("Page\nAuthor".to_owned()),
            ..real_page()
        };
        let lp = message_to_history_message(&session, &link_message(authored))
            .extra
            .link_preview
            .expect("real page is drawn");
        assert_eq!(lp.site_name, "Example");
        assert_eq!(lp.title, "Page Author");
    }
}

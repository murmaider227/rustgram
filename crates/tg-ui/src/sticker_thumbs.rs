//! Sticker pictures for widgets that show a pack (the sticker set box) by document id.
//!
//! tdesktop's `StickerSetBox::Inner` asks each sticker's `DocumentMedia` for its image and the
//! document loads it from the pack (`Data::FileOriginStickerSet`), repainting the box when it
//! lands. The session installs a [`StickerThumbSource`] on the `egui::Context`; widgets call
//! [`sticker_thumb`] and keep their placeholder while it returns `None`.

use std::sync::Arc;

use egui::{Context, Id};

/// A decoded sticker picture: unpremultiplied RGBA8, `width * height * 4` bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerThumb {
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// RGBA8 pixels.
    pub rgba: Vec<u8>,
}

/// Supplies sticker pictures by document id; `None` while unknown or loading.
///
/// Called from the UI thread on every paint, so implementations must not block: start the
/// download on the first call and ask the UI to repaint when it finishes.
pub trait StickerThumbSource: Send + Sync {
    /// Returns the picture of sticker `document_id`, starting its download if needed.
    fn sticker_thumb(&self, document_id: u64) -> Option<Arc<StickerThumb>>;
}

#[derive(Clone)]
struct SourceSlot(Arc<dyn StickerThumbSource>);

fn source_id() -> Id {
    Id::unique("tg_ui_sticker_thumb_source")
}

/// Installs (or replaces) the sticker picture source used by [`sticker_thumb`] on `ctx`.
pub fn install_sticker_thumb_source(ctx: &Context, source: Arc<dyn StickerThumbSource>) {
    ctx.data_mut(|d| d.insert_temp(source_id(), SourceSlot(source)));
}

/// Removes the sticker picture source from `ctx` (logout).
pub fn remove_sticker_thumb_source(ctx: &Context) {
    ctx.data_mut(|d| d.remove::<SourceSlot>(source_id()));
}

/// Returns sticker `document_id`'s picture from the source installed on `ctx`, if loaded.
#[must_use]
pub fn sticker_thumb(ctx: &Context, document_id: u64) -> Option<Arc<StickerThumb>> {
    if document_id == 0 {
        return None;
    }
    let source = ctx.data(|d| d.get_temp::<SourceSlot>(source_id()))?;
    source.0.sticker_thumb(document_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct One;

    impl StickerThumbSource for One {
        fn sticker_thumb(&self, document_id: u64) -> Option<Arc<StickerThumb>> {
            (document_id == 7).then(|| {
                Arc::new(StickerThumb {
                    width: 1,
                    height: 1,
                    rgba: vec![1, 2, 3, 4],
                })
            })
        }
    }

    #[test]
    fn the_installed_source_answers() {
        let ctx = Context::default();
        assert!(sticker_thumb(&ctx, 7).is_none(), "no source yet");
        install_sticker_thumb_source(&ctx, Arc::new(One));
        assert_eq!(
            sticker_thumb(&ctx, 7).map(|t| t.rgba.clone()),
            Some(vec![1, 2, 3, 4])
        );
        assert!(sticker_thumb(&ctx, 8).is_none());
        remove_sticker_thumb_source(&ctx);
        assert!(sticker_thumb(&ctx, 7).is_none());
    }
}

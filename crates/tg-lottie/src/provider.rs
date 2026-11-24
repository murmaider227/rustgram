//! Lottie animation instances and frame providers (direct, cached, multi-cache, shared).
//!
//! Port of `lottie/details/lottie_instance.h`, `lottie/details/lottie_instance.cpp`,
//! `lottie/details/lottie_frame_provider.h`, `lottie/details/lottie_frame_provider_direct.cpp`,
//! `lottie/details/lottie_frame_provider_cached.cpp`,
//! `lottie/details/lottie_frame_provider_cached_multi.cpp`, and
//! `lottie/details/lottie_frame_provider_shared.cpp`.

use std::sync::{Arc, Mutex};

use crate::cache::{CACHE_SIZE_ROUNDING, Cache, CacheReadContext};
use crate::color::apply_color_replacements;
use crate::common::{
    ColorReplacements, FrameBuffer, FrameRenderResult, FrameRequest, Information, MAX_FILE_SIZE,
    Quality, Size, content_error, read_utf8, unpack_gzip, validate_information,
};
use crate::error::Error;
use crate::model::Composition;
use crate::render::render_composition_into;

/// Size rounding used by `FrameProviderDirect` (`FrameProviderDirect::sizeRounding()`).
pub const DIRECT_SIZE_ROUNDING: u32 = 2;

/// One parsed Lottie animation instance ready to render frames (`Lottie::Instance`).
#[derive(Debug, Clone)]
pub struct Instance {
    composition: Composition,
    size: Size,
    frame_rate: f64,
    frames_count: u32,
}

impl Instance {
    /// Parses a UTF-8 Lottie JSON string and applies optional color replacements.
    pub fn from_json(json: &str, replacements: Option<&ColorReplacements>) -> Result<Self, Error> {
        let mut comp = Composition::from_json(json)?;
        if let Some(repl) = replacements {
            apply_color_replacements(&mut comp, repl);
        }
        let size = Size::new(comp.width, comp.height);
        let frame_rate = comp.frame_rate;
        let frames_count = comp.total_frames();
        Ok(Self {
            composition: comp,
            size,
            frame_rate,
            frames_count,
        })
    }

    /// Loads either plain Lottie JSON or gzip-compressed `.tgs` bytes, enforcing tdesktop's
    /// size limits (`MAX_FILE_SIZE` on compressed/uncompressed input and `MAX_GZIP_FILE_SIZE`).
    pub fn from_bytes(
        content: &[u8],
        replacements: Option<&ColorReplacements>,
    ) -> Result<Self, Error> {
        content_error(content)?;
        let unpacked = unpack_gzip(content)?;
        if unpacked.len() > MAX_FILE_SIZE {
            return Err(Error::FileTooLarge {
                size: unpacked.len(),
                max: MAX_FILE_SIZE,
            });
        }
        let json = read_utf8(&unpacked)?;
        Self::from_json(json, replacements)
    }

    /// Original composition dimensions.
    #[must_use]
    pub const fn size(&self) -> Size {
        self.size
    }

    /// Native frame rate in frames per second.
    #[must_use]
    pub const fn frame_rate(&self) -> f64 {
        self.frame_rate
    }

    /// Native frame count (`round(op - ip)`).
    #[must_use]
    pub const fn frames_count(&self) -> u32 {
        self.frames_count
    }

    /// Accesses the underlying parsed `Composition`.
    #[must_use]
    pub const fn composition(&self) -> &Composition {
        &self.composition
    }

    /// Renders native frame `index` (relative to `in_point`) into `to`, aspect-fit centered.
    pub fn render_to_prepared(&self, to: &mut FrameBuffer, index: u32) {
        let frame = self.composition.in_point + f64::from(index);
        render_composition_into(&self.composition, frame, to);
    }

    /// Renders at an arbitrary fractional composition frame (relative to `in_point`) into `to`.
    pub fn render_at_frame(&self, to: &mut FrameBuffer, frame_offset: f64) {
        let frame = self.composition.in_point + frame_offset;
        render_composition_into(&self.composition, frame, to);
    }
}

/// Per-consumer token for shared/cached frame providers (`Lottie::FrameProviderToken`).
#[derive(Debug, Default)]
pub struct FrameProviderToken {
    /// Result of the last render attempt.
    pub result: FrameRenderResultState,
    /// Whether this caller currently holds exclusive write access.
    pub exclusive: bool,
    /// Per-reader cache decoding state (used by cached providers).
    pub cache_context: CacheReadContext,
}

/// Wrapper with default `Ok` for `FrameProviderToken`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRenderResultState(pub FrameRenderResult);

impl Default for FrameRenderResultState {
    fn default() -> Self {
        Self(FrameRenderResult::Ok)
    }
}

/// Abstract source of rendered frames (`Lottie::FrameProvider`).
pub trait FrameProvider: Send {
    /// Renders the initial cover frame (frame 0) and initializes any token state.
    fn construct(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        request: FrameRequest,
    ) -> FrameBuffer;

    /// Returns the animation metadata.
    fn information(&self) -> Information;

    /// Returns `true` if the provider is valid and can produce frames.
    fn valid(&self) -> bool;

    /// Required pixel alignment/rounding for rendered frames (`2` direct, `8` cached).
    fn size_rounding(&self) -> u32;

    /// Creates a per-consumer token (primarily used by shared cached providers).
    fn create_token(&self) -> Option<FrameProviderToken> {
        None
    }

    /// Renders effective frame `index` into `to` for `request`.
    fn render(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        to: &mut FrameBuffer,
        request: FrameRequest,
        index: u32,
    ) -> bool;
}

/// Computes the tdesktop 60fps-to-30fps frame multiplier (`GetLottieFrameMultiplier`).
fn frame_multiplier(frame_rate: f64, quality: Quality) -> u32 {
    let rate = frame_rate.round() as u32;
    if quality == Quality::Default && rate == 60 {
        2
    } else {
        1
    }
}

/// Direct software-rasterizing frame provider (`Lottie::FrameProviderDirect`).
pub struct FrameProviderDirect {
    instance: Option<Instance>,
    information: Information,
    quality: Quality,
    multiplier: u32,
}

impl FrameProviderDirect {
    /// Creates an empty direct provider configured with `quality`.
    #[must_use]
    pub fn new(quality: Quality) -> Self {
        Self {
            instance: None,
            information: Information::default(),
            quality,
            multiplier: 1,
        }
    }

    /// Loads plain JSON or `.tgs` bytes and validates all tdesktop limits.
    pub fn load(
        &mut self,
        content: &[u8],
        replacements: Option<&ColorReplacements>,
    ) -> Result<(), Error> {
        self.information = Information::default();
        let instance = Instance::from_bytes(content, replacements)?;
        let multiplier = frame_multiplier(instance.frame_rate(), self.quality);
        let rate = instance.frame_rate().round().max(0.0) as u32;
        let count = instance.frames_count();
        let info = Information {
            size: instance.size(),
            frame_rate: rate / multiplier,
            frames_count: count.div_ceil(multiplier),
        };
        validate_information(info)?;
        self.multiplier = multiplier;
        self.information = info;
        self.instance = Some(instance);
        Ok(())
    }

    /// Returns `true` if the underlying parsed `Instance` is currently loaded in memory.
    #[must_use]
    pub fn loaded(&self) -> bool {
        self.instance.is_some()
    }

    /// Drops the parsed `Instance` to free memory (e.g. once all frames are cached).
    pub fn unload(&mut self) {
        self.instance = None;
    }

    /// Sets validated metadata (used when initializing purely from a complete cache).
    pub fn set_information(&mut self, info: Information) -> bool {
        if validate_information(info).is_ok() {
            self.information = info;
            true
        } else {
            self.information = Information::default();
            false
        }
    }

    /// Renders effective frame `index` into an already-sized buffer `to`.
    pub fn render_to_prepared(&self, to: &mut FrameBuffer, index: u32) {
        if let Some(ref inst) = self.instance {
            inst.render_to_prepared(to, index.saturating_mul(self.multiplier));
        }
    }

    /// Returns the native frame multiplier (`1` or `2` when halving 60 fps -> 30 fps).
    #[must_use]
    pub const fn multiplier(&self) -> u32 {
        self.multiplier
    }
}

impl FrameProvider for FrameProviderDirect {
    fn construct(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        request: FrameRequest,
    ) -> FrameBuffer {
        let mut cover = FrameBuffer::default();
        self.render(token, &mut cover, request, 0);
        cover
    }

    fn information(&self) -> Information {
        self.information
    }

    fn valid(&self) -> bool {
        self.information.frames_count > 0
    }

    fn size_rounding(&self) -> u32 {
        DIRECT_SIZE_ROUNDING
    }

    fn render(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        to: &mut FrameBuffer,
        request: FrameRequest,
        index: u32,
    ) -> bool {
        if let Some(tok) = token.as_mut()
            && !tok.exclusive
        {
            tok.result = FrameRenderResultState(FrameRenderResult::NotReady);
            return false;
        }
        if !self.valid() {
            return false;
        }
        let original = self.information.size;
        let size = if request.is_empty() {
            original
        } else {
            request.size(original, self.size_rounding())
        };
        to.ensure_storage(size);
        self.render_to_prepared(to, index);
        true
    }
}

/// Frame provider backed by a single compressed `Cache` (`Lottie::FrameProviderCached`).
pub struct FrameProviderCached {
    cache: Cache,
    direct: FrameProviderDirect,
    content: Vec<u8>,
    replacements: Option<ColorReplacements>,
}

impl FrameProviderCached {
    /// Creates a cached provider, loading `direct` only if `cached` is empty or incomplete.
    pub fn new(
        content: Vec<u8>,
        put: Option<Box<dyn FnMut(Vec<u8>) + Send>>,
        cached: Vec<u8>,
        request: FrameRequest,
        quality: Quality,
        replacements: Option<ColorReplacements>,
    ) -> Self {
        let cache = Cache::new(cached, request, put);
        let mut direct = FrameProviderDirect::new(quality);
        if cache.frames_count() == 0 || cache.frames_ready() < cache.frames_count() {
            let _ = direct.load(&content, replacements.as_ref());
        } else {
            direct.set_information(Information {
                size: cache.original_size(),
                frame_rate: cache.frame_rate(),
                frames_count: cache.frames_count(),
            });
        }
        Self {
            cache,
            direct,
            content,
            replacements,
        }
    }

    /// Accesses the underlying `Cache`.
    #[must_use]
    pub const fn cache(&self) -> &Cache {
        &self.cache
    }

    /// Mutably accesses the underlying `Cache` (e.g. to call `finalize_encoding()`).
    pub fn cache_mut(&mut self) -> &mut Cache {
        &mut self.cache
    }

    /// Returns whether the direct Lottie parser/renderer is currently loaded.
    #[must_use]
    pub fn direct_loaded(&self) -> bool {
        self.direct.loaded()
    }
}

impl FrameProvider for FrameProviderCached {
    fn construct(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        request: FrameRequest,
    ) -> FrameBuffer {
        let mut cover = self.cache.take_first_frame().unwrap_or_default();
        let is_exclusive = token.as_ref().is_none_or(|t| t.exclusive);
        if is_exclusive {
            if !cover.is_empty() {
                if let Some(tok) = token.as_mut() {
                    self.cache.keep_up_context(&mut tok.cache_context);
                }
                return cover;
            }
            let info = self.information();
            self.cache
                .init(info.size, info.frame_rate, info.frames_count, request);
        }
        self.render(token, &mut cover, request, 0);
        cover
    }

    fn information(&self) -> Information {
        self.direct.information()
    }

    fn valid(&self) -> bool {
        self.direct.valid()
    }

    fn size_rounding(&self) -> u32 {
        self.cache.size_rounding()
    }

    fn create_token(&self) -> Option<FrameProviderToken> {
        let mut tok = FrameProviderToken::default();
        self.cache.prepare_buffers(&mut tok.cache_context);
        Some(tok)
    }

    fn render(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        to: &mut FrameBuffer,
        request: FrameRequest,
        index: u32,
    ) -> bool {
        if !self.valid() {
            if let Some(tok) = token.as_mut() {
                tok.result = FrameRenderResultState(FrameRenderResult::Failed);
            }
            return false;
        }
        let original = self.information().size;
        let size = if request.is_empty() {
            original
        } else {
            request.size(original, self.size_rounding())
        };
        to.ensure_storage(size);

        if let Some(tok) = token.as_mut()
            && !tok.exclusive
        {
            let res =
                self.cache
                    .render_frame_with_context(&mut tok.cache_context, to, request, index);
            tok.result = FrameRenderResultState(res);
            return res == FrameRenderResult::Ok;
        }

        let res = self.cache.render_frame(to, request, index);
        if res == FrameRenderResult::Ok {
            if let Some(tok) = token.as_mut() {
                self.cache.keep_up_context(&mut tok.cache_context);
            }
            return true;
        }
        if res == FrameRenderResult::Failed
            || (res == FrameRenderResult::BadCacheSize && token.is_some())
            || (!self.direct.loaded()
                && self
                    .direct
                    .load(&self.content, self.replacements.as_ref())
                    .is_err())
        {
            self.direct.set_information(Information::default());
            return false;
        }

        self.direct.render_to_prepared(to, index);
        self.cache.append_frame(to, request, index);
        if self.cache.frames_ready() == self.cache.frames_count() {
            self.direct.unload();
        }
        if let Some(tok) = token.as_mut() {
            self.cache.keep_up_context(&mut tok.cache_context);
        }
        true
    }
}

/// Callback invoked by `FrameProviderCachedMulti` when a partition finishes encoding.
pub type MultiCachePutFn = Arc<Mutex<Box<dyn FnMut(usize, Vec<u8>) + Send>>>;

/// Multi-part cache provider splitting frames across `N` cache blobs
/// (`Lottie::FrameProviderCachedMulti`).
pub struct FrameProviderCachedMulti {
    content: Vec<u8>,
    replacements: Option<ColorReplacements>,
    caches: Vec<Cache>,
    direct: FrameProviderDirect,
    frames_per_cache: u32,
}

impl FrameProviderCachedMulti {
    /// Creates a multi-cache provider with one `Cache` per entry in `initial_caches`.
    pub fn new(
        content: Vec<u8>,
        put: Option<MultiCachePutFn>,
        initial_caches: Vec<Vec<u8>>,
        request: FrameRequest,
        quality: Quality,
        replacements: Option<ColorReplacements>,
    ) -> Self {
        let keys_count = initial_caches.len().max(1);
        let mut caches: Vec<Cache> = Vec::with_capacity(keys_count);
        let mut direct = FrameProviderDirect::new(quality);

        let make_put =
            |idx: usize, put: &Option<MultiCachePutFn>| -> Option<Box<dyn FnMut(Vec<u8>) + Send>> {
                put.as_ref().map(|arc| {
                    let cloned = Arc::clone(arc);
                    Box::new(move |bytes: Vec<u8>| {
                        if let Ok(mut guard) = cloned.lock() {
                            (*guard)(idx, bytes);
                        }
                    }) as Box<dyn FnMut(Vec<u8>) + Send>
                })
            };

        let mut loaded_failed = false;
        for (idx, cached_blob) in initial_caches.into_iter().enumerate() {
            caches.push(Cache::new(cached_blob, request, make_put(idx, &put)));
            let first_fr = caches[0].frame_rate();
            let first_orig = caches[0].original_size();
            let cur = &caches[idx];
            if cur.frames_count() == 0 {
                if !direct.loaded() && direct.load(&content, replacements.as_ref()).is_err() {
                    caches.clear();
                    loaded_failed = true;
                    break;
                }
                while caches.len() < keys_count {
                    let next_idx = caches.len();
                    caches.push(Cache::new(Vec::new(), request, make_put(next_idx, &put)));
                }
                break;
            } else if cur.frames_ready() < cur.frames_count()
                && !direct.loaded()
                && direct.load(&content, replacements.as_ref()).is_err()
            {
                caches.clear();
                loaded_failed = true;
                break;
            } else if cur.frame_rate() != first_fr || cur.original_size() != first_orig {
                caches.pop();
                if !direct.loaded() && direct.load(&content, replacements.as_ref()).is_err() {
                    caches.clear();
                    loaded_failed = true;
                    break;
                }
                while caches.len() < keys_count {
                    let next_idx = caches.len();
                    caches.push(Cache::new(Vec::new(), request, make_put(next_idx, &put)));
                }
                break;
            }
        }

        if !loaded_failed && !direct.loaded() && !caches.is_empty() {
            let total_frames: u32 = caches.iter().map(Cache::frames_count).sum();
            direct.set_information(Information {
                size: caches[0].original_size(),
                frame_rate: caches[0].frame_rate(),
                frames_count: total_frames,
            });
        }

        let mut provider = Self {
            content,
            replacements,
            caches,
            direct,
            frames_per_cache: 0,
        };

        if !loaded_failed && !provider.validate_frames_per_cache() && provider.frames_per_cache > 0
        {
            if provider.direct.loaded()
                || provider
                    .direct
                    .load(&provider.content, provider.replacements.as_ref())
                    .is_ok()
            {
                while provider.caches.len() < keys_count {
                    let next_idx = provider.caches.len();
                    provider
                        .caches
                        .push(Cache::new(Vec::new(), request, make_put(next_idx, &put)));
                }
            } else {
                provider.caches.clear();
                provider.frames_per_cache = 0;
            }
        }

        provider
    }

    fn validate_frames_per_cache(&mut self) -> bool {
        let info = self.direct.information();
        let count = self.caches.len() as u32;
        if count == 0 || info.frames_count == 0 {
            self.frames_per_cache = 0;
            return false;
        }
        self.frames_per_cache = info.frames_count.div_ceil(count);
        if self.frames_per_cache == 0 || info.frames_count <= (count - 1) * self.frames_per_cache {
            self.frames_per_cache = 0;
            return false;
        }
        for i in 0..(count as usize) {
            let c_frames = self.caches[i].frames_count();
            if c_frames == 0 {
                break;
            }
            let should_be = if i + 1 == count as usize {
                info.frames_count - (count - 1) * self.frames_per_cache
            } else {
                self.frames_per_cache
            };
            if c_frames != should_be {
                self.caches.truncate(i);
                return false;
            }
        }
        true
    }

    /// Number of cache partitions.
    #[must_use]
    pub fn caches_count(&self) -> usize {
        self.caches.len()
    }

    /// Number of frames assigned to each partition (last partition may hold fewer).
    #[must_use]
    pub const fn frames_per_cache(&self) -> u32 {
        self.frames_per_cache
    }

    /// Flushes all cache partitions.
    pub fn finalize_all(&mut self) {
        for c in &mut self.caches {
            c.finalize_encoding();
        }
    }
}

impl FrameProvider for FrameProviderCachedMulti {
    fn construct(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        request: FrameRequest,
    ) -> FrameBuffer {
        if self.frames_per_cache == 0 || self.caches.is_empty() {
            if let Some(tok) = token.as_mut() {
                tok.result = FrameRenderResultState(FrameRenderResult::Failed);
            }
            return FrameBuffer::default();
        }
        let mut cover = FrameBuffer::default();
        let is_exclusive = token.as_ref().is_none_or(|t| t.exclusive);
        if is_exclusive {
            let info = self.information();
            let count = self.caches.len();
            for i in 0..count {
                let cache_cover = self.caches[i].take_first_frame();
                if let Some(img) = cache_cover {
                    if i == 0 {
                        cover = img;
                    }
                } else {
                    let part_frames = if i + 1 == count {
                        info.frames_count - ((count as u32) - 1) * self.frames_per_cache
                    } else {
                        self.frames_per_cache
                    };
                    self.caches[i].init(info.size, info.frame_rate, part_frames, request);
                }
            }
            if !cover.is_empty() {
                if let Some(tok) = token.as_mut() {
                    self.caches[0].keep_up_context(&mut tok.cache_context);
                }
                return cover;
            }
        }
        self.render(token, &mut cover, request, 0);
        cover
    }

    fn information(&self) -> Information {
        self.direct.information()
    }

    fn valid(&self) -> bool {
        self.direct.valid() && self.frames_per_cache > 0
    }

    fn size_rounding(&self) -> u32 {
        self.caches
            .first()
            .map_or(CACHE_SIZE_ROUNDING, Cache::size_rounding)
    }

    fn create_token(&self) -> Option<FrameProviderToken> {
        let mut tok = FrameProviderToken::default();
        if let Some(first) = self.caches.first() {
            first.prepare_buffers(&mut tok.cache_context);
        }
        Some(tok)
    }

    fn render(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        to: &mut FrameBuffer,
        request: FrameRequest,
        index: u32,
    ) -> bool {
        if !self.valid() {
            if let Some(tok) = token.as_mut() {
                tok.result = FrameRenderResultState(FrameRenderResult::Failed);
            }
            return false;
        }
        let original = self.information().size;
        let size = if request.is_empty() {
            original
        } else {
            request.size(original, self.size_rounding())
        };
        to.ensure_storage(size);

        let cache_idx = (index / self.frames_per_cache) as usize;
        let index_in_cache = index % self.frames_per_cache;
        if cache_idx >= self.caches.len() {
            return false;
        }

        if let Some(tok) = token.as_mut()
            && !tok.exclusive
        {
            let res = self.caches[cache_idx].render_frame_with_context(
                &mut tok.cache_context,
                to,
                request,
                index_in_cache,
            );
            tok.result = FrameRenderResultState(res);
            return res == FrameRenderResult::Ok;
        }

        let res = self.caches[cache_idx].render_frame(to, request, index_in_cache);
        if res == FrameRenderResult::Ok {
            if let Some(tok) = token.as_mut() {
                self.caches[cache_idx].keep_up_context(&mut tok.cache_context);
            }
            return true;
        }
        if res == FrameRenderResult::Failed
            || (res == FrameRenderResult::BadCacheSize && token.is_some())
            || (!self.direct.loaded()
                && self
                    .direct
                    .load(&self.content, self.replacements.as_ref())
                    .is_err())
        {
            self.direct.set_information(Information::default());
            return false;
        }

        self.direct.render_to_prepared(to, index);
        self.caches[cache_idx].append_frame(to, request, index_in_cache);
        if self.caches[cache_idx].frames_ready() == self.caches[cache_idx].frames_count()
            && cache_idx + 1 == self.caches.len()
        {
            self.direct.unload();
        }
        if let Some(tok) = token.as_mut() {
            self.caches[cache_idx].keep_up_context(&mut tok.cache_context);
        }
        true
    }
}

/// Thread-safe shared wrapper around any `FrameProvider` (`Lottie::FrameProviderShared`).
pub struct FrameProviderShared {
    inner: Arc<Mutex<SharedInner>>,
    information: Information,
    size_rounding: u32,
}

struct SharedInner {
    provider: Option<Box<dyn FrameProvider>>,
    constructed: bool,
}

impl FrameProviderShared {
    /// Wraps `provider` for shared concurrent access across multiple players.
    #[must_use]
    pub fn new(provider: Box<dyn FrameProvider>) -> Self {
        let information = provider.information();
        let size_rounding = provider.size_rounding();
        Self {
            inner: Arc::new(Mutex::new(SharedInner {
                provider: Some(provider),
                constructed: false,
            })),
            information,
            size_rounding,
        }
    }

    /// Clones a handle pointing to the same underlying shared provider.
    #[must_use]
    pub fn clone_handle(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            information: self.information,
            size_rounding: self.size_rounding,
        }
    }
}

impl FrameProvider for FrameProviderShared {
    fn construct(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        request: FrameRequest,
    ) -> FrameBuffer {
        let Ok(mut guard) = self.inner.lock() else {
            return FrameBuffer::default();
        };
        let constructed = guard.constructed;
        let Some(ref mut provider) = guard.provider else {
            return FrameBuffer::default();
        };
        *token = provider.create_token();
        if let Some(tok) = token.as_mut() {
            tok.exclusive = !constructed;
        }
        let cover = provider.construct(token, request);
        guard.constructed = true;
        cover
    }

    fn information(&self) -> Information {
        self.information
    }

    fn valid(&self) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.provider.as_ref().map(|p| p.valid()))
            .unwrap_or(false)
    }

    fn size_rounding(&self) -> u32 {
        self.size_rounding
    }

    fn create_token(&self) -> Option<FrameProviderToken> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.provider.as_ref().and_then(|p| p.create_token()))
    }

    fn render(
        &mut self,
        token: &mut Option<FrameProviderToken>,
        to: &mut FrameBuffer,
        request: FrameRequest,
        index: u32,
    ) -> bool {
        let Ok(mut guard) = self.inner.lock() else {
            return false;
        };
        let Some(ref mut provider) = guard.provider else {
            if let Some(tok) = token.as_mut() {
                tok.result = FrameRenderResultState(FrameRenderResult::Failed);
            }
            return false;
        };
        if let Some(tok) = token.as_mut() {
            tok.exclusive = false;
            if provider.render(token, to, request, index) {
                return true;
            }
            if let Some(tok_after) = token.as_mut() {
                if tok_after.result.0 == FrameRenderResult::Failed {
                    guard.provider = None;
                    return false;
                }
                tok_after.exclusive = true;
            }
        }
        provider.render(token, to, request, index)
    }
}

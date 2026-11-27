/*
 * Thin bindgen-free C shim over FFmpeg (libavformat, libavcodec, libavutil,
 * libswscale, libswresample) porting Telegram Desktop's ffmpeg_utility.cpp.
 *
 * Licensed under GPL-3.0-or-later WITH openvpn-openssl-exception.
 */
#ifndef TG_FFMPEG_SHIM_H
#define TG_FFMPEG_SHIM_H

#include <stdint.h>
#include <stddef.h>

/*
 * Container and decoder whitelists restricting FFmpeg to the media formats and codecs
 * Telegram Desktop's minimal FFmpeg build enables (mp4/mov/m4a, webm/matroska, gif, mp3,
 * ogg/opus, aac, wav; h264, hevc, vp8, vp9, av1, mpeg4, gif, aac, mp3, opus, vorbis, pcm).
 */
#define TG_FORMAT_WHITELIST "mov,mp4,m4a,matroska,webm,gif,mp3,ogg,aac,wav"
#define TG_CODEC_WHITELIST \
    "h264,libopenh264,hevc,vp8,libvpx,vp9,libvpx-vp9,av1,libdav1d,libaom-av1,mpeg4,gif," \
    "aac,aac_fixed,aac_latm,mp3,mp3float,mp3adu,mp3adufloat,mp3on4,mp3on4float," \
    "opus,libopus,vorbis,libvorbis," \
    "pcm_s16le,pcm_s16be,pcm_s16le_planar,pcm_s16be_planar," \
    "pcm_s24le,pcm_s24be,pcm_s24le_planar," \
    "pcm_s32le,pcm_s32be,pcm_s32le_planar," \
    "pcm_s8,pcm_s8_planar,pcm_u8," \
    "pcm_u16le,pcm_u16be,pcm_u24le,pcm_u24be,pcm_u32le,pcm_u32be," \
    "pcm_f32le,pcm_f32be,pcm_f64le,pcm_f64be,pcm_alaw,pcm_mulaw"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct TgMediaContext TgMediaContext;
typedef struct TgSwsCache TgSwsCache;
typedef struct TgSwrCache TgSwrCache;

typedef int (*TgReadCallback)(void *opaque, uint8_t *buf, int buf_size);
typedef int64_t (*TgSeekCallback)(void *opaque, int64_t offset, int whence);

/* Error code constants matching FFmpeg AVERROR values. */
int tg_averror_eof(void);
int tg_averror_eagain(void);
int tg_averror_invaliddata(void);
int tg_averror_external(void);
int64_t tg_av_nopts_value(void);
void tg_av_error_string(int errnum, char *buf, size_t buf_size);

/* Pixel output format selectors for tg_sws_convert. */
enum TgOutputPixFmt {
    TG_OUT_RGBA = 0,
    TG_OUT_BGRA = 1
};

/* Decoded video frame format category. */
enum TgFramePixFmtKind {
    TG_PIX_OTHER = 0,
    TG_PIX_YUV420P = 1,
    TG_PIX_NV12 = 2,
    TG_PIX_YUVA420P = 3,
    TG_PIX_BGRA = 4,
    TG_PIX_RGBA = 5
};

/* Hardware acceleration kind active on a decoder. */
enum TgHwAccelKind {
    TG_HW_NONE = 0,
    TG_HW_D3D11VA = 1,
    TG_HW_DXVA2 = 2,
    TG_HW_CUDA = 3,
    TG_HW_VIDEOTOOLBOX = 4,
    TG_HW_VAAPI = 5,
    TG_HW_VDPAU = 6
};

typedef struct TgOpenOptions {
    int seekable;
    int ignore_editlist;
    int hw_allowed;
    int open_video;
    int open_audio;
    int64_t video_max_area;
    int thread_count; /* 0 = auto, <0 = single-threaded */
} TgOpenOptions;

typedef struct TgStreamInfo {
    int has_video;
    int video_stream_index;
    int video_width;
    int video_height;
    int video_timebase_num;
    int video_timebase_den;
    int64_t video_duration_pts; /* AV_NOPTS_VALUE if unknown */
    int video_sar_num;
    int video_sar_den;
    double video_fps;
    int video_rotation; /* normalized to 0, 90, 180, 270 */
    int video_codec_id;
    int video_is_h264;
    int video_is_vp9;
    int video_is_webm_vp9;
    char video_codec_name[64];

    int has_audio;
    int audio_stream_index;
    int audio_sample_rate;
    int audio_channels;
    int audio_timebase_num;
    int audio_timebase_den;
    int64_t audio_duration_pts; /* AV_NOPTS_VALUE if unknown */
    int audio_codec_id;
    int audio_is_opus;
    char audio_codec_name[64];

    int64_t format_duration_us; /* AV_NOPTS_VALUE if unknown */
    char format_name[64];
} TgStreamInfo;

typedef struct TgVideoFrameMeta {
    int width;
    int height;
    int64_t pts;
    int64_t best_effort_timestamp;
    int64_t pkt_dts;
    int64_t duration;
    int pix_fmt;
    int pix_fmt_kind;
    int has_alpha;
    int hw_accel_used;
    int y_stride;
    int u_stride;
    int v_stride;
    int chroma_width;
    int chroma_height;
} TgVideoFrameMeta;

typedef struct TgAudioFrameMeta {
    int sample_rate;
    int channels;
    int nb_samples;
    int64_t pts;
    int64_t best_effort_timestamp;
    int64_t pkt_dts;
    int64_t duration;
} TgAudioFrameMeta;

/* Packet / media lifecycle */
TgMediaContext *tg_media_open_custom_io(
    void *opaque,
    TgReadCallback read_cb,
    TgSeekCallback seek_cb,
    const TgOpenOptions *options,
    TgStreamInfo *out_info,
    int *out_error);

void tg_media_close(TgMediaContext *ctx);

/* Seeking & flushing */
int tg_media_seek_video(TgMediaContext *ctx, int64_t pts, int backward);
int tg_media_seek_audio(TgMediaContext *ctx, int64_t pts, int backward);
int tg_media_seek_to_start(TgMediaContext *ctx);
void tg_media_flush_codecs(TgMediaContext *ctx);

/*
 * Decode next frame from demuxer.
 * Return values:
 *   1 = video frame ready in ctx->video_frame (meta filled in out_video_meta)
 *   2 = audio frame ready in ctx->audio_frame (meta filled in out_audio_meta)
 *   0 = EOF reached (all decoders drained)
 *  <0 = FFmpeg error code
 */
int tg_media_decode_next(
    TgMediaContext *ctx,
    int want_video,
    int want_audio,
    TgVideoFrameMeta *out_video_meta,
    TgAudioFrameMeta *out_audio_meta);

/* Convert the currently held decoded video frame into tightly-packed RGBA/BGRA. */
int tg_media_convert_video_frame(
    TgMediaContext *ctx,
    int dst_width,
    int dst_height,
    int output_pix_fmt, /* enum TgOutputPixFmt */
    uint8_t *dst_buf,
    int dst_stride);

/*
 * Extract YUV420P or NV12 planes from the currently held video frame.
 * For YUV420P: y_buf (w*h), u_buf (cw*ch), v_buf (cw*ch).
 * For NV12:    y_buf (w*h), u_buf (cw*2*ch), v_buf may be NULL.
 */
int tg_media_extract_yuv(
    TgMediaContext *ctx,
    uint8_t *y_buf,
    size_t y_len,
    uint8_t *u_buf,
    size_t u_len,
    uint8_t *v_buf,
    size_t v_len);

/*
 * Resample the currently held decoded audio frame into interleaved f32 PCM.
 * Returns number of output frames (samples per channel) written, or <0 on error.
 */
int tg_media_convert_audio_f32(
    TgMediaContext *ctx,
    int dst_sample_rate,
    int dst_channels,
    float *dst_buf,
    int max_out_frames);

/* Returns required output frame capacity for the currently held audio frame. */
int tg_media_audio_out_frames_bound(
    TgMediaContext *ctx,
    int dst_sample_rate);

/* Query FFmpeg library versions for diagnostics. */
void tg_ffmpeg_versions(
    uint32_t *avformat_ver,
    uint32_t *avcodec_ver,
    uint32_t *avutil_ver,
    uint32_t *swscale_ver,
    uint32_t *swresample_ver);

/* FFmpeg build identifier (`av_version_info()`, e.g. "N-129333-g7fdb4c2341-20261127"). */
const char *tg_ffmpeg_build_id(void);

/* Video editor export / transcode configuration (ports media_video_encode.cpp). */
typedef struct TgVideoEncodeConfig {
    int64_t from_ms;        /* Trim start in milliseconds (>= 0). */
    int64_t till_ms;        /* Trim end in milliseconds (<= 0 means end of clip). */
    int target_width;       /* Output width (even; <= 0 keeps cropped/rotated width). */
    int target_height;      /* Output height (even; <= 0 keeps cropped/rotated height). */
    int crop_x;             /* Crop X in display pixels (<= 0 when crop_w/h == 0). */
    int crop_y;             /* Crop Y in display pixels. */
    int crop_w;             /* Crop width (<= 0 means no crop). */
    int crop_h;             /* Crop height (<= 0 means no crop). */
    int rotation;           /* Additional clockwise rotation: 0, 90, 180, 270. */
    int flipped;            /* Horizontal flip: 0 or 1. */
    int remove_audio;       /* 1 = strip audio (muted / GIF), 0 = keep audio if present. */
    float audio_volume;     /* Audio gain multiplier in [0.0, 2.0] (<= 0 mutes). */
    double fps_limit;       /* Optional max FPS (<= 0 keeps source FPS). */
    int64_t video_bitrate;  /* Target bitrate in bps (<= 0 auto-computes). */
} TgVideoEncodeConfig;

/* Result summary produced by tg_video_transcode_file. */
typedef struct TgVideoEncodeResult {
    int output_width;
    int output_height;
    int64_t duration_ms;
    int has_audio;
    int64_t file_size;
} TgVideoEncodeResult;

/* Transcode / trim / resize / mute a video file into a faststart MP4 file. */
int tg_video_transcode_file(
    const char *input_path,
    const char *output_path,
    const TgVideoEncodeConfig *config,
    TgVideoEncodeResult *out_result,
    char *err_buf,
    size_t err_buf_size);

/* Generate a tiny synthetic MP4 clip (video + optional sine audio) for tests. */
int tg_video_generate_test_clip(
    const char *output_path,
    int width,
    int height,
    int fps,
    int duration_ms,
    int with_audio,
    char *err_buf,
    size_t err_buf_size);

#ifdef __cplusplus
}
#endif

#endif /* TG_FFMPEG_SHIM_H */

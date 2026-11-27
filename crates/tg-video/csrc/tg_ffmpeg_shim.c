/*
 * Thin bindgen-free C shim over FFmpeg porting Telegram Desktop's
 * Telegram/SourceFiles/ffmpeg/ffmpeg_utility.cpp.
 *
 * Licensed under GPL-3.0-or-later WITH openvpn-openssl-exception.
 */
#include "tg_ffmpeg_shim.h"

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/channel_layout.h>
#include <libavutil/display.h>
#include <libavutil/error.h>
#include <libavutil/hwcontext.h>
#include <libavutil/imgutils.h>
#include <libavutil/mem.h>
#include <libavutil/opt.h>
#include <libavutil/pixdesc.h>
#include <libavutil/version.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>

#include <limits.h>
#include <math.h>
#include <stdlib.h>
#include <string.h>

#define TG_AVIO_BLOCK_SIZE 4096
#define TG_MAX_PIXELS_PADDING_RATIO 32
#define TG_MAX_PIXELS_FIXED_PADDING (64 * 1024)
#define TG_SKIP_INVALID_DATA_PACKETS 10

struct TgMediaContext {
    AVFormatContext *fmt;
    AVIOContext *avio;

    AVCodecContext *video_codec;
    AVCodecContext *audio_codec;
    int video_stream_index;
    int audio_stream_index;

    AVPacket *pkt;
    AVFrame *video_frame;
    AVFrame *video_transferred;
    AVFrame *video_active; /* points to video_frame or video_transferred */
    AVFrame *audio_frame;

    struct SwsContext *sws;
    int sws_src_w;
    int sws_src_h;
    int sws_src_fmt;
    int sws_dst_w;
    int sws_dst_h;
    int sws_dst_fmt;

    struct SwrContext *swr;
    int swr_src_fmt;
    int swr_src_rate;
    int swr_src_channels;
    int swr_dst_rate;
    int swr_dst_channels;

    int demux_eof;
    int video_drained;
    int audio_drained;
    int video_invalid_packets;
    int audio_invalid_packets;
    int hw_accel_used;
};

int tg_averror_eof(void) { return AVERROR_EOF; }
int tg_averror_eagain(void) { return AVERROR(EAGAIN); }
int tg_averror_invaliddata(void) { return AVERROR_INVALIDDATA; }
int tg_averror_external(void) { return AVERROR_EXTERNAL; }
int64_t tg_av_nopts_value(void) { return AV_NOPTS_VALUE; }

void tg_av_error_string(int errnum, char *buf, size_t buf_size) {
    if (!buf || buf_size == 0) {
        return;
    }
    av_strerror(errnum, buf, buf_size);
}

void tg_ffmpeg_versions(
    uint32_t *avformat_ver,
    uint32_t *avcodec_ver,
    uint32_t *avutil_ver,
    uint32_t *swscale_ver,
    uint32_t *swresample_ver) {
    if (avformat_ver) *avformat_ver = avformat_version();
    if (avcodec_ver) *avcodec_ver = avcodec_version();
    if (avutil_ver) *avutil_ver = avutil_version();
    if (swscale_ver) *swscale_ver = swscale_version();
    if (swresample_ver) *swresample_ver = swresample_version();
}

const char *tg_ffmpeg_build_id(void) {
    return av_version_info();
}

static int64_t max_pixels_for_area_limit(int64_t area) {
    return area + area / TG_MAX_PIXELS_PADDING_RATIO + TG_MAX_PIXELS_FIXED_PADDING;
}

static int normalize_rotation(int rotation) {
    int result = rotation % 360;
    if (result < 0) {
        result += 360;
    }
    return (result == 90 || result == 180 || result == 270) ? result : 0;
}

static int read_rotation_from_stream(const AVStream *stream) {
    if (!stream || !stream->codecpar) {
        return 0;
    }
    const AVPacketSideData *displaymatrix = av_packet_side_data_get(
        stream->codecpar->coded_side_data,
        stream->codecpar->nb_coded_side_data,
        AV_PKT_DATA_DISPLAYMATRIX);
    if (displaymatrix && displaymatrix->data && displaymatrix->size >= 9 * sizeof(int32_t)) {
        const int32_t *matrix = (const int32_t *)displaymatrix->data;
        double angle = av_display_rotation_get(matrix);
        if (isfinite(angle)) {
            int rounded = (int)llround(-angle);
            int normalized = normalize_rotation(rounded);
            if (normalized != 0) {
                return normalized;
            }
        }
    }
    const AVDictionaryEntry *rotate_tag = av_dict_get(stream->metadata, "rotate", NULL, 0);
    if (rotate_tag && rotate_tag->value && rotate_tag->value[0] != '\0') {
        char *endptr = NULL;
        long val = strtol(rotate_tag->value, &endptr, 10);
        if (endptr != rotate_tag->value) {
            return normalize_rotation((int)val);
        }
    }
    return 0;
}

static int is_webm_format_name(const char *name) {
    if (!name) {
        return 0;
    }
    const char *p = name;
    while (*p) {
        const char *comma = strchr(p, ',');
        size_t len = comma ? (size_t)(comma - p) : strlen(p);
        if (len == 4 && memcmp(p, "webm", 4) == 0) {
            return 1;
        }
        if (!comma) break;
        p = comma + 1;
    }
    return 0;
}

/*
 * Hardware decoding selection ported from ffmpeg_utility.cpp:
 * Windows tries D3D11VA -> DXVA2 -> CUDA, falling back to software.
 */
static int init_hw_device(AVCodecContext *context, enum AVHWDeviceType type) {
    AVCodecContext *parent = (AVCodecContext *)context->opaque;
    AVBufferRef *hw_device_ctx = NULL;
    int err = av_hwdevice_ctx_create(&hw_device_ctx, type, NULL, NULL, 0);
    if (err < 0 || !hw_device_ctx) {
        return 0;
    }
    if (parent->hw_device_ctx) {
        av_buffer_unref(&parent->hw_device_ctx);
    }
    parent->hw_device_ctx = av_buffer_ref(hw_device_ctx);
    av_buffer_unref(&hw_device_ctx);
    if (!parent->hw_device_ctx) {
        return 0;
    }
    context->hw_device_ctx = parent->hw_device_ctx;
    return 1;
}

static enum AVPixelFormat get_hw_format(
    AVCodecContext *context,
    const enum AVPixelFormat *formats) {
    if (!formats) {
        return AV_PIX_FMT_NONE;
    }

#ifdef _WIN32
    static const enum AVPixelFormat preferred[] = {
        AV_PIX_FMT_D3D11,
        AV_PIX_FMT_DXVA2_VLD,
        AV_PIX_FMT_CUDA
    };
#elif defined(__APPLE__)
    static const enum AVPixelFormat preferred[] = {
        AV_PIX_FMT_VIDEOTOOLBOX
    };
#else
    static const enum AVPixelFormat preferred[] = {
        AV_PIX_FMT_VAAPI,
        AV_PIX_FMT_VDPAU,
        AV_PIX_FMT_CUDA
    };
#endif

    size_t count = sizeof(preferred) / sizeof(preferred[0]);
    for (size_t i = 0; i < count; ++i) {
        enum AVPixelFormat want = preferred[i];
        int found = 0;
        for (const enum AVPixelFormat *p = formats; *p != AV_PIX_FMT_NONE; ++p) {
            if (*p == want) {
                found = 1;
                break;
            }
        }
        if (!found) {
            continue;
        }
        enum AVHWDeviceType dev_type = AV_HWDEVICE_TYPE_NONE;
        switch (want) {
#ifdef _WIN32
        case AV_PIX_FMT_D3D11: dev_type = AV_HWDEVICE_TYPE_D3D11VA; break;
        case AV_PIX_FMT_DXVA2_VLD: dev_type = AV_HWDEVICE_TYPE_DXVA2; break;
        case AV_PIX_FMT_CUDA: dev_type = AV_HWDEVICE_TYPE_CUDA; break;
#elif defined(__APPLE__)
        case AV_PIX_FMT_VIDEOTOOLBOX: dev_type = AV_HWDEVICE_TYPE_VIDEOTOOLBOX; break;
#else
        case AV_PIX_FMT_VAAPI: dev_type = AV_HWDEVICE_TYPE_VAAPI; break;
        case AV_PIX_FMT_VDPAU: dev_type = AV_HWDEVICE_TYPE_VDPAU; break;
        case AV_PIX_FMT_CUDA: dev_type = AV_HWDEVICE_TYPE_CUDA; break;
#endif
        default: dev_type = AV_HWDEVICE_TYPE_NONE; break;
        }

        if (dev_type != AV_HWDEVICE_TYPE_NONE && !init_hw_device(context, dev_type)) {
            continue;
        }
#ifdef _WIN32
        if (want == AV_PIX_FMT_DXVA2_VLD &&
            (context->pix_fmt == AV_PIX_FMT_D3D11 || context->pix_fmt == AV_PIX_FMT_D3D11VA_VLD)) {
            /* DXVA2 init locks as D3D11 by the pix_fmt left from a re-init. */
            context->pix_fmt = AV_PIX_FMT_NONE;
        }
#endif
        return want;
    }

    /* Fall back to last (software) format in the list, matching tdesktop. */
    enum AVPixelFormat fallback = AV_PIX_FMT_NONE;
    for (const enum AVPixelFormat *p = formats; *p != AV_PIX_FMT_NONE; ++p) {
        fallback = *p;
    }
    return fallback;
}

static const AVCodec *find_decoder(const AVCodecContext *context) {
    /* Force libvpx-vp9 for alpha channel support, matching ffmpeg_utility.cpp. */
    if (context->codec_id == AV_CODEC_ID_VP9) {
        const AVCodec *vpx = avcodec_find_decoder_by_name("libvpx-vp9");
        if (vpx) {
            return vpx;
        }
    }
    return avcodec_find_decoder(context->codec_id);
}

static AVCodecContext *open_stream_codec(
    AVStream *stream,
    int hw_allowed,
    int64_t video_max_area,
    int thread_count,
    int *out_error) {
    AVCodecContext *codec_ctx = avcodec_alloc_context3(NULL);
    if (!codec_ctx) {
        if (out_error) *out_error = AVERROR(ENOMEM);
        return NULL;
    }
    int err = avcodec_parameters_to_context(codec_ctx, stream->codecpar);
    if (err < 0) {
        avcodec_free_context(&codec_ctx);
        if (out_error) *out_error = err;
        return NULL;
    }
    codec_ctx->pkt_timebase = stream->time_base;

    if (video_max_area > 0 && codec_ctx->codec_type == AVMEDIA_TYPE_VIDEO) {
        codec_ctx->max_pixels = max_pixels_for_area_limit(video_max_area);
    }
    if (thread_count >= 0) {
        av_opt_set_int(codec_ctx, "threads", thread_count, 0);
    } else {
        av_opt_set_int(codec_ctx, "threads", 1, 0);
    }

    const AVCodec *codec = find_decoder(codec_ctx);
    if (!codec) {
        avcodec_free_context(&codec_ctx);
        if (out_error) *out_error = AVERROR_DECODER_NOT_FOUND;
        return NULL;
    }

    if (hw_allowed && codec_ctx->codec_type == AVMEDIA_TYPE_VIDEO) {
        codec_ctx->get_format = get_hw_format;
        codec_ctx->opaque = codec_ctx;
    }

    err = av_opt_set(codec_ctx, "codec_whitelist", TG_CODEC_WHITELIST, 0);
    if (err < 0) {
        avcodec_free_context(&codec_ctx);
        if (out_error) *out_error = err;
        return NULL;
    }

    err = avcodec_open2(codec_ctx, codec, NULL);
    if (err < 0) {
        avcodec_free_context(&codec_ctx);
        if (out_error) *out_error = err;
        return NULL;
    }
    return codec_ctx;
}

static int classify_hw_accel(enum AVPixelFormat pix_fmt) {
    switch (pix_fmt) {
    case AV_PIX_FMT_D3D11:
    case AV_PIX_FMT_D3D11VA_VLD:
        return TG_HW_D3D11VA;
    case AV_PIX_FMT_DXVA2_VLD:
        return TG_HW_DXVA2;
    case AV_PIX_FMT_CUDA:
        return TG_HW_CUDA;
    case AV_PIX_FMT_VIDEOTOOLBOX:
        return TG_HW_VIDEOTOOLBOX;
    case AV_PIX_FMT_VAAPI:
        return TG_HW_VAAPI;
    case AV_PIX_FMT_VDPAU:
        return TG_HW_VDPAU;
    default:
        return TG_HW_NONE;
    }
}

static int classify_pix_fmt(int pix_fmt) {
    switch (pix_fmt) {
    case AV_PIX_FMT_YUV420P:
    case AV_PIX_FMT_YUVJ420P:
        return TG_PIX_YUV420P;
    case AV_PIX_FMT_NV12:
        return TG_PIX_NV12;
    case AV_PIX_FMT_YUVA420P:
        return TG_PIX_YUVA420P;
    case AV_PIX_FMT_BGRA:
        return TG_PIX_BGRA;
    case AV_PIX_FMT_RGBA:
        return TG_PIX_RGBA;
    default:
        return TG_PIX_OTHER;
    }
}

static int pix_fmt_has_alpha(int pix_fmt) {
    const AVPixFmtDescriptor *desc = av_pix_fmt_desc_get((enum AVPixelFormat)pix_fmt);
    return (desc && (desc->flags & AV_PIX_FMT_FLAG_ALPHA)) ? 1 : 0;
}

static void copy_string_safe(char *dst, size_t dst_size, const char *src) {
    if (!dst || dst_size == 0) return;
    if (!src) {
        dst[0] = '\0';
        return;
    }
    size_t len = strlen(src);
    if (len >= dst_size) {
        len = dst_size - 1;
    }
    memcpy(dst, src, len);
    dst[len] = '\0';
}

TgMediaContext *tg_media_open_custom_io(
    void *opaque,
    TgReadCallback read_cb,
    TgSeekCallback seek_cb,
    const TgOpenOptions *options,
    TgStreamInfo *out_info,
    int *out_error) {
    if (out_error) *out_error = 0;
    if (out_info) memset(out_info, 0, sizeof(*out_info));

    if (!read_cb || !options) {
        if (out_error) *out_error = AVERROR(EINVAL);
        return NULL;
    }

    TgMediaContext *ctx = (TgMediaContext *)calloc(1, sizeof(TgMediaContext));
    if (!ctx) {
        if (out_error) *out_error = AVERROR(ENOMEM);
        return NULL;
    }
    ctx->video_stream_index = -1;
    ctx->audio_stream_index = -1;
    ctx->sws_src_fmt = AV_PIX_FMT_NONE;
    ctx->sws_dst_fmt = AV_PIX_FMT_NONE;
    ctx->swr_src_fmt = AV_SAMPLE_FMT_NONE;

    unsigned char *io_buf = (unsigned char *)av_malloc(TG_AVIO_BLOCK_SIZE);
    if (!io_buf) {
        if (out_error) *out_error = AVERROR(ENOMEM);
        tg_media_close(ctx);
        return NULL;
    }

    ctx->avio = avio_alloc_context(
        io_buf,
        TG_AVIO_BLOCK_SIZE,
        0,
        opaque,
        read_cb,
        NULL,
        options->seekable ? seek_cb : NULL);
    if (!ctx->avio) {
        av_freep(&io_buf);
        if (out_error) *out_error = AVERROR(ENOMEM);
        tg_media_close(ctx);
        return NULL;
    }
    ctx->avio->seekable = (options->seekable && seek_cb != NULL) ? AVIO_SEEKABLE_NORMAL : 0;

    ctx->fmt = avformat_alloc_context();
    if (!ctx->fmt) {
        if (out_error) *out_error = AVERROR(ENOMEM);
        tg_media_close(ctx);
        return NULL;
    }
    ctx->fmt->pb = ctx->avio;
    ctx->fmt->flags |= AVFMT_FLAG_CUSTOM_IO;
    /* RestrictToCustomIO + container/codec whitelists matching tdesktop's minimal FFmpeg build. */
    int wl_err = av_opt_set(ctx->fmt, "protocol_whitelist", "", 0);
    if (wl_err >= 0) {
        wl_err = av_opt_set(ctx->fmt, "format_whitelist", TG_FORMAT_WHITELIST, 0);
    }
    if (wl_err >= 0) {
        wl_err = av_opt_set(ctx->fmt, "codec_whitelist", TG_CODEC_WHITELIST, 0);
    }
    if (wl_err < 0) {
        if (out_error) *out_error = wl_err;
        tg_media_close(ctx);
        return NULL;
    }

    AVDictionary *dict = NULL;
    av_dict_set(&dict, "usetoc", "1", 0);
    if (options->ignore_editlist) {
        av_dict_set(&dict, "ignore_editlist", "1", 0);
    }

    int err = avformat_open_input(&ctx->fmt, NULL, NULL, &dict);
    av_dict_free(&dict);
    if (err < 0) {
        /* avformat_open_input frees ctx->fmt on error, but NOT ctx->avio. */
        ctx->fmt = NULL;
        if (out_error) *out_error = err;
        tg_media_close(ctx);
        return NULL;
    }

    if (options->seekable && seek_cb != NULL) {
        ctx->fmt->flags |= AVFMT_FLAG_FAST_SEEK;
    }

    err = avformat_find_stream_info(ctx->fmt, NULL);
    if (err < 0) {
        if (out_error) *out_error = err;
        tg_media_close(ctx);
        return NULL;
    }

    if (out_info) {
        out_info->video_stream_index = -1;
        out_info->audio_stream_index = -1;
        out_info->video_duration_pts = AV_NOPTS_VALUE;
        out_info->audio_duration_pts = AV_NOPTS_VALUE;
        out_info->format_duration_us = ctx->fmt->duration;
        if (ctx->fmt->iformat && ctx->fmt->iformat->name) {
            copy_string_safe(out_info->format_name, sizeof(out_info->format_name), ctx->fmt->iformat->name);
        }
    }

    int v_idx = av_find_best_stream(ctx->fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (v_idx >= 0) {
        AVStream *v_stream = ctx->fmt->streams[v_idx];
        if ((v_stream->disposition & AV_DISPOSITION_ATTACHED_PIC) == 0) {
            if (options->open_video) {
                int codec_err = 0;
                ctx->video_codec = open_stream_codec(
                    v_stream,
                    options->hw_allowed,
                    options->video_max_area,
                    options->thread_count,
                    &codec_err);
                if (!ctx->video_codec) {
                    if (out_error) *out_error = codec_err;
                    tg_media_close(ctx);
                    return NULL;
                }
            }
            ctx->video_stream_index = v_idx;
            if (out_info) {
                out_info->has_video = 1;
                out_info->video_stream_index = v_idx;
                out_info->video_width = v_stream->codecpar->width;
                out_info->video_height = v_stream->codecpar->height;
                out_info->video_timebase_num = v_stream->time_base.num;
                out_info->video_timebase_den = v_stream->time_base.den;
                out_info->video_duration_pts = v_stream->duration;
                out_info->video_sar_num = v_stream->sample_aspect_ratio.num;
                out_info->video_sar_den = v_stream->sample_aspect_ratio.den;
                AVRational guessed = av_guess_frame_rate(ctx->fmt, v_stream, NULL);
                if (guessed.num > 0 && guessed.den > 0) {
                    out_info->video_fps = av_q2d(guessed);
                }
                out_info->video_rotation = read_rotation_from_stream(v_stream);
                out_info->video_codec_id = (int)v_stream->codecpar->codec_id;
                out_info->video_is_h264 = (v_stream->codecpar->codec_id == AV_CODEC_ID_H264) ? 1 : 0;
                out_info->video_is_vp9 = (v_stream->codecpar->codec_id == AV_CODEC_ID_VP9) ? 1 : 0;
                out_info->video_is_webm_vp9 =
                    (out_info->video_is_vp9 &&
                     ctx->fmt->iformat &&
                     is_webm_format_name(ctx->fmt->iformat->name))
                        ? 1
                        : 0;
                const char *cname = avcodec_get_name(v_stream->codecpar->codec_id);
                copy_string_safe(out_info->video_codec_name, sizeof(out_info->video_codec_name), cname);
            }
        }
    }

    int a_idx = av_find_best_stream(ctx->fmt, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (a_idx >= 0) {
        AVStream *a_stream = ctx->fmt->streams[a_idx];
        if (a_stream->codecpar->sample_rate > 0) {
            if (options->open_audio) {
                int codec_err = 0;
                ctx->audio_codec = open_stream_codec(
                    a_stream,
                    0,
                    0,
                    options->thread_count,
                    &codec_err);
                if (!ctx->audio_codec) {
                    if (out_error) *out_error = codec_err;
                    tg_media_close(ctx);
                    return NULL;
                }
            }
            ctx->audio_stream_index = a_idx;
            if (out_info) {
                out_info->has_audio = 1;
                out_info->audio_stream_index = a_idx;
                out_info->audio_sample_rate = a_stream->codecpar->sample_rate;
                out_info->audio_channels = a_stream->codecpar->ch_layout.nb_channels;
                out_info->audio_timebase_num = a_stream->time_base.num;
                out_info->audio_timebase_den = a_stream->time_base.den;
                out_info->audio_duration_pts = a_stream->duration;
                out_info->audio_codec_id = (int)a_stream->codecpar->codec_id;
                out_info->audio_is_opus = (a_stream->codecpar->codec_id == AV_CODEC_ID_OPUS) ? 1 : 0;
                const char *cname = avcodec_get_name(a_stream->codecpar->codec_id);
                copy_string_safe(out_info->audio_codec_name, sizeof(out_info->audio_codec_name), cname);
            }
        }
    }

    ctx->pkt = av_packet_alloc();
    ctx->video_frame = av_frame_alloc();
    ctx->video_transferred = av_frame_alloc();
    ctx->audio_frame = av_frame_alloc();
    if (!ctx->pkt || !ctx->video_frame || !ctx->video_transferred || !ctx->audio_frame) {
        if (out_error) *out_error = AVERROR(ENOMEM);
        tg_media_close(ctx);
        return NULL;
    }

    return ctx;
}

void tg_media_close(TgMediaContext *ctx) {
    if (!ctx) return;
    if (ctx->sws) {
        sws_freeContext(ctx->sws);
        ctx->sws = NULL;
    }
    if (ctx->swr) {
        swr_free(&ctx->swr);
    }
    if (ctx->video_frame) av_frame_free(&ctx->video_frame);
    if (ctx->video_transferred) av_frame_free(&ctx->video_transferred);
    if (ctx->audio_frame) av_frame_free(&ctx->audio_frame);
    if (ctx->pkt) av_packet_free(&ctx->pkt);
    if (ctx->video_codec) avcodec_free_context(&ctx->video_codec);
    if (ctx->audio_codec) avcodec_free_context(&ctx->audio_codec);
    if (ctx->fmt) {
        ctx->fmt->pb = NULL;
        avformat_close_input(&ctx->fmt);
    }
    if (ctx->avio) {
        av_freep(&ctx->avio->buffer);
        avio_context_free(&ctx->avio);
    }
    free(ctx);
}

void tg_media_flush_codecs(TgMediaContext *ctx) {
    if (!ctx) return;
    if (ctx->video_codec) avcodec_flush_buffers(ctx->video_codec);
    if (ctx->audio_codec) avcodec_flush_buffers(ctx->audio_codec);
    av_frame_unref(ctx->video_frame);
    av_frame_unref(ctx->video_transferred);
    av_frame_unref(ctx->audio_frame);
    ctx->video_active = NULL;
    ctx->demux_eof = 0;
    ctx->video_drained = 0;
    ctx->audio_drained = 0;
    ctx->video_invalid_packets = 0;
    ctx->audio_invalid_packets = 0;
}

int tg_media_seek_video(TgMediaContext *ctx, int64_t pts, int backward) {
    if (!ctx || !ctx->fmt || ctx->video_stream_index < 0) {
        return AVERROR(EINVAL);
    }
    int flags = backward ? AVSEEK_FLAG_BACKWARD : 0;
    int err = av_seek_frame(ctx->fmt, ctx->video_stream_index, pts, flags);
    if (err >= 0) {
        tg_media_flush_codecs(ctx);
    }
    return err;
}

int tg_media_seek_audio(TgMediaContext *ctx, int64_t pts, int backward) {
    if (!ctx || !ctx->fmt || ctx->audio_stream_index < 0) {
        return AVERROR(EINVAL);
    }
    int flags = backward ? AVSEEK_FLAG_BACKWARD : 0;
    int err = av_seek_frame(ctx->fmt, ctx->audio_stream_index, pts, flags);
    if (err >= 0) {
        tg_media_flush_codecs(ctx);
    }
    return err;
}

int tg_media_seek_to_start(TgMediaContext *ctx) {
    if (!ctx || !ctx->fmt) {
        return AVERROR(EINVAL);
    }
    int stream_id = (ctx->video_stream_index >= 0) ? ctx->video_stream_index : -1;
    int err = avformat_seek_file(ctx->fmt, stream_id, INT64_MIN, 0, INT64_MAX, 0);
    if (err < 0) {
        err = av_seek_frame(ctx->fmt, stream_id, 0, AVSEEK_FLAG_BYTE);
    }
    if (err < 0) {
        err = av_seek_frame(ctx->fmt, stream_id, 0, AVSEEK_FLAG_FRAME);
    }
    if (err < 0) {
        err = av_seek_frame(ctx->fmt, stream_id, 0, AVSEEK_FLAG_BACKWARD);
    }
    if (err < 0) {
        err = av_seek_frame(ctx->fmt, -1, 0, AVSEEK_FLAG_BACKWARD);
    }
    if (err >= 0) {
        tg_media_flush_codecs(ctx);
    }
    return err;
}

static int prepare_video_frame_meta(TgMediaContext *ctx, TgVideoFrameMeta *out_meta) {
    AVFrame *src = ctx->video_frame;
    AVFrame *active = src;
    int hw_used = TG_HW_NONE;

    if (src->hw_frames_ctx != NULL) {
        hw_used = classify_hw_accel((enum AVPixelFormat)src->format);
        av_frame_unref(ctx->video_transferred);
        int err = av_hwframe_transfer_data(ctx->video_transferred, src, 0);
        if (err < 0) {
            return err;
        }
        ctx->video_transferred->pts = src->pts;
        ctx->video_transferred->best_effort_timestamp = src->best_effort_timestamp;
        ctx->video_transferred->pkt_dts = src->pkt_dts;
        ctx->video_transferred->duration = src->duration;
        active = ctx->video_transferred;
    }

    ctx->video_active = active;
    ctx->hw_accel_used = hw_used;

    if (out_meta) {
        int fmt = (active->format == AV_PIX_FMT_NONE && ctx->video_codec)
                      ? (int)ctx->video_codec->pix_fmt
                      : active->format;
        out_meta->width = active->width;
        out_meta->height = active->height;
        out_meta->pts = active->pts;
        out_meta->best_effort_timestamp = active->best_effort_timestamp;
        out_meta->pkt_dts = active->pkt_dts;
        out_meta->duration = active->duration;
        out_meta->pix_fmt = fmt;
        out_meta->pix_fmt_kind = classify_pix_fmt(fmt);
        out_meta->has_alpha = pix_fmt_has_alpha(fmt);
        out_meta->hw_accel_used = hw_used;
        out_meta->y_stride = active->linesize[0];
        out_meta->u_stride = active->linesize[1];
        out_meta->v_stride = active->linesize[2];
        out_meta->chroma_width = AV_CEIL_RSHIFT(active->width, 1);
        out_meta->chroma_height = AV_CEIL_RSHIFT(active->height, 1);
    }
    return 0;
}

static void fill_audio_frame_meta(const AVFrame *frame, TgAudioFrameMeta *out_meta) {
    if (!out_meta || !frame) return;
    out_meta->sample_rate = frame->sample_rate;
    out_meta->channels = frame->ch_layout.nb_channels;
    out_meta->nb_samples = frame->nb_samples;
    out_meta->pts = frame->pts;
    out_meta->best_effort_timestamp = frame->best_effort_timestamp;
    out_meta->pkt_dts = frame->pkt_dts;
    out_meta->duration = frame->duration;
}

int tg_media_decode_next(
    TgMediaContext *ctx,
    int want_video,
    int want_audio,
    TgVideoFrameMeta *out_video_meta,
    TgAudioFrameMeta *out_audio_meta) {
    if (!ctx || !ctx->fmt) {
        return AVERROR(EINVAL);
    }
    int use_video = want_video && ctx->video_codec != NULL;
    int use_audio = want_audio && ctx->audio_codec != NULL;
    if (!use_video && !use_audio) {
        return 0;
    }

    for (;;) {
        if (use_video && !ctx->video_drained) {
            av_frame_unref(ctx->video_frame);
            int res = avcodec_receive_frame(ctx->video_codec, ctx->video_frame);
            if (res >= 0) {
                int meta_err = prepare_video_frame_meta(ctx, out_video_meta);
                if (meta_err < 0) return meta_err;
                return 1;
            } else if (res == AVERROR_EOF) {
                ctx->video_drained = 1;
            } else if (res != AVERROR(EAGAIN)) {
                return res;
            }
        }

        if (use_audio && !ctx->audio_drained) {
            av_frame_unref(ctx->audio_frame);
            int res = avcodec_receive_frame(ctx->audio_codec, ctx->audio_frame);
            if (res >= 0) {
                fill_audio_frame_meta(ctx->audio_frame, out_audio_meta);
                return 2;
            } else if (res == AVERROR_EOF) {
                ctx->audio_drained = 1;
            } else if (res != AVERROR(EAGAIN)) {
                return res;
            }
        }

        if (ctx->demux_eof) {
            int video_done = !use_video || ctx->video_drained;
            int audio_done = !use_audio || ctx->audio_drained;
            if (video_done && audio_done) {
                return 0;
            }
            /* Drain codecs once after demux EOF. */
            if (use_video && !ctx->video_drained) {
                avcodec_send_packet(ctx->video_codec, NULL);
            }
            if (use_audio && !ctx->audio_drained) {
                avcodec_send_packet(ctx->audio_codec, NULL);
            }
            continue;
        }

        av_packet_unref(ctx->pkt);
        int read_res = av_read_frame(ctx->fmt, ctx->pkt);
        if (read_res == AVERROR_EOF) {
            ctx->demux_eof = 1;
            if (use_video) avcodec_send_packet(ctx->video_codec, NULL);
            if (use_audio) avcodec_send_packet(ctx->audio_codec, NULL);
            continue;
        } else if (read_res < 0) {
            return read_res;
        }

        int stream_idx = ctx->pkt->stream_index;
        if (use_video && stream_idx == ctx->video_stream_index) {
            int send_res = avcodec_send_packet(ctx->video_codec, ctx->pkt);
            av_packet_unref(ctx->pkt);
            if (send_res < 0) {
                if (send_res == AVERROR_INVALIDDATA &&
                    ++ctx->video_invalid_packets < TG_SKIP_INVALID_DATA_PACKETS) {
                    continue;
                }
                return send_res;
            }
            ctx->video_invalid_packets = 0;
        } else if (use_audio && stream_idx == ctx->audio_stream_index) {
            int send_res = avcodec_send_packet(ctx->audio_codec, ctx->pkt);
            av_packet_unref(ctx->pkt);
            if (send_res < 0) {
                if (send_res == AVERROR_INVALIDDATA &&
                    ctx->audio_codec->codec_id != AV_CODEC_ID_OPUS &&
                    ++ctx->audio_invalid_packets < TG_SKIP_INVALID_DATA_PACKETS) {
                    continue;
                }
                return send_res;
            }
            ctx->audio_invalid_packets = 0;
        } else {
            av_packet_unref(ctx->pkt);
        }
    }
}

static int validate_source_planes(const AVFrame *frame, enum AVPixelFormat pix_fmt) {
    const AVPixFmtDescriptor *desc = av_pix_fmt_desc_get(pix_fmt);
    if (!desc) return 0;
    int visible_bytes[4] = {0, 0, 0, 0};
    if (av_image_fill_linesizes(visible_bytes, pix_fmt, frame->width) < 0) {
        return 0;
    }
    int is_pal = (desc->flags & AV_PIX_FMT_FLAG_PAL) != 0;
    for (int plane = 0; plane < 4; ++plane) {
        const uint8_t *data = frame->data[plane];
        if (is_pal && plane == 1) {
            if (!data) return 0;
            continue;
        }
        int visible = visible_bytes[plane];
        if (visible <= 0) continue;
        int stride = frame->linesize[plane];
        if (!data || stride == 0 || stride == INT_MIN) return 0;
        int mag = stride < 0 ? -stride : stride;
        if (mag < visible) return 0;
    }
    return 1;
}

int tg_media_convert_video_frame(
    TgMediaContext *ctx,
    int dst_width,
    int dst_height,
    int output_pix_fmt,
    uint8_t *dst_buf,
    int dst_stride) {
    if (!ctx || !ctx->video_active || !dst_buf || dst_width <= 0 || dst_height <= 0 || dst_stride <= 0) {
        return AVERROR(EINVAL);
    }
    AVFrame *frame = ctx->video_active;
    if (frame->width <= 0 || frame->height <= 0 || !frame->data[0]) {
        return AVERROR_INVALIDDATA;
    }
    int src_fmt = (frame->format == AV_PIX_FMT_NONE && ctx->video_codec)
                      ? (int)ctx->video_codec->pix_fmt
                      : frame->format;
    if (src_fmt <= AV_PIX_FMT_NONE || src_fmt >= AV_PIX_FMT_NB) {
        return AVERROR_INVALIDDATA;
    }
    if (!validate_source_planes(frame, (enum AVPixelFormat)src_fmt)) {
        return AVERROR_INVALIDDATA;
    }

    enum AVPixelFormat dst_fmt = (output_pix_fmt == TG_OUT_BGRA) ? AV_PIX_FMT_BGRA : AV_PIX_FMT_RGBA;

    /* Custom SwsContext cache matching MakeSwscalePointer in ffmpeg_utility.cpp. */
    if (!ctx->sws ||
        ctx->sws_src_w != frame->width ||
        ctx->sws_src_h != frame->height ||
        ctx->sws_src_fmt != src_fmt ||
        ctx->sws_dst_w != dst_width ||
        ctx->sws_dst_h != dst_height ||
        ctx->sws_dst_fmt != (int)dst_fmt) {
        ctx->sws = sws_getCachedContext(
            ctx->sws,
            frame->width,
            frame->height,
            (enum AVPixelFormat)src_fmt,
            dst_width,
            dst_height,
            dst_fmt,
            SWS_BILINEAR,
            NULL,
            NULL,
            NULL);
        if (!ctx->sws) {
            return AVERROR(ENOMEM);
        }
        ctx->sws_src_w = frame->width;
        ctx->sws_src_h = frame->height;
        ctx->sws_src_fmt = src_fmt;
        ctx->sws_dst_w = dst_width;
        ctx->sws_dst_h = dst_height;
        ctx->sws_dst_fmt = (int)dst_fmt;
    }

    uint8_t *dst_data[AV_NUM_DATA_POINTERS] = { dst_buf, NULL, NULL, NULL };
    int dst_linesize[AV_NUM_DATA_POINTERS] = { dst_stride, 0, 0, 0 };

    int converted = sws_scale(
        ctx->sws,
        (const uint8_t *const *)frame->data,
        frame->linesize,
        0,
        frame->height,
        dst_data,
        dst_linesize);
    if (converted != dst_height) {
        return AVERROR_EXTERNAL;
    }
    return 0;
}

static void copy_plane_rows(
    uint8_t *dst,
    const uint8_t *src,
    int src_stride,
    int row_bytes,
    int height) {
    for (int y = 0; y < height; ++y) {
        memcpy(dst + (size_t)y * (size_t)row_bytes,
               src + (ptrdiff_t)y * (ptrdiff_t)src_stride,
               (size_t)row_bytes);
    }
}

int tg_media_extract_yuv(
    TgMediaContext *ctx,
    uint8_t *y_buf,
    size_t y_len,
    uint8_t *u_buf,
    size_t u_len,
    uint8_t *v_buf,
    size_t v_len) {
    if (!ctx || !ctx->video_active || !y_buf || !u_buf) {
        return AVERROR(EINVAL);
    }
    AVFrame *frame = ctx->video_active;
    int fmt = classify_pix_fmt(frame->format);
    if (fmt != TG_PIX_YUV420P && fmt != TG_PIX_NV12) {
        return AVERROR(EINVAL);
    }
    int w = frame->width;
    int h = frame->height;
    int cw = AV_CEIL_RSHIFT(w, 1);
    int ch = AV_CEIL_RSHIFT(h, 1);

    if (y_len < (size_t)w * (size_t)h) {
        return AVERROR(EINVAL);
    }
    copy_plane_rows(y_buf, frame->data[0], frame->linesize[0], w, h);

    if (fmt == TG_PIX_YUV420P) {
        if (!v_buf || u_len < (size_t)cw * (size_t)ch || v_len < (size_t)cw * (size_t)ch) {
            return AVERROR(EINVAL);
        }
        copy_plane_rows(u_buf, frame->data[1], frame->linesize[1], cw, ch);
        copy_plane_rows(v_buf, frame->data[2], frame->linesize[2], cw, ch);
    } else {
        /* NV12: interleaved UV plane of width cw * 2, height ch. */
        int uv_row = cw * 2;
        if (u_len < (size_t)uv_row * (size_t)ch) {
            return AVERROR(EINVAL);
        }
        copy_plane_rows(u_buf, frame->data[1], frame->linesize[1], uv_row, ch);
    }
    return 0;
}

int tg_media_audio_out_frames_bound(TgMediaContext *ctx, int dst_sample_rate) {
    if (!ctx || !ctx->audio_frame || ctx->audio_frame->sample_rate <= 0 || dst_sample_rate <= 0) {
        return AVERROR(EINVAL);
    }
    int64_t delay = ctx->swr ? swr_get_delay(ctx->swr, ctx->audio_frame->sample_rate) : 0;
    int64_t out_samples = av_rescale_rnd(
        delay + ctx->audio_frame->nb_samples,
        dst_sample_rate,
        ctx->audio_frame->sample_rate,
        AV_ROUND_UP);
    if (out_samples < 0 || out_samples > INT_MAX) {
        return AVERROR(EINVAL);
    }
    return (int)out_samples;
}

int tg_media_convert_audio_f32(
    TgMediaContext *ctx,
    int dst_sample_rate,
    int dst_channels,
    float *dst_buf,
    int max_out_frames) {
    if (!ctx || !ctx->audio_frame || !dst_buf || dst_sample_rate <= 0 || dst_channels <= 0 || max_out_frames <= 0) {
        return AVERROR(EINVAL);
    }
    AVFrame *frame = ctx->audio_frame;
    int src_channels = frame->ch_layout.nb_channels;
    if (src_channels <= 0 || frame->sample_rate <= 0 || frame->nb_samples <= 0) {
        return AVERROR_INVALIDDATA;
    }

    if (!ctx->swr ||
        ctx->swr_src_fmt != frame->format ||
        ctx->swr_src_rate != frame->sample_rate ||
        ctx->swr_src_channels != src_channels ||
        ctx->swr_dst_rate != dst_sample_rate ||
        ctx->swr_dst_channels != dst_channels) {
        if (ctx->swr) {
            swr_free(&ctx->swr);
        }
        AVChannelLayout dst_layout;
        av_channel_layout_default(&dst_layout, dst_channels);

        AVChannelLayout src_layout;
        if (av_channel_layout_check(&frame->ch_layout)) {
            av_channel_layout_copy(&src_layout, &frame->ch_layout);
        } else {
            av_channel_layout_default(&src_layout, src_channels);
        }

        int err = swr_alloc_set_opts2(
            &ctx->swr,
            &dst_layout,
            AV_SAMPLE_FMT_FLT,
            dst_sample_rate,
            &src_layout,
            (enum AVSampleFormat)frame->format,
            frame->sample_rate,
            0,
            NULL);
        av_channel_layout_uninit(&dst_layout);
        av_channel_layout_uninit(&src_layout);
        if (err < 0 || !ctx->swr) {
            if (ctx->swr) swr_free(&ctx->swr);
            return err < 0 ? err : AVERROR(ENOMEM);
        }
        err = swr_init(ctx->swr);
        if (err < 0) {
            swr_free(&ctx->swr);
            return err;
        }
        ctx->swr_src_fmt = frame->format;
        ctx->swr_src_rate = frame->sample_rate;
        ctx->swr_src_channels = src_channels;
        ctx->swr_dst_rate = dst_sample_rate;
        ctx->swr_dst_channels = dst_channels;
    }

    uint8_t *out_planes[1] = { (uint8_t *)dst_buf };
    int converted = swr_convert(
        ctx->swr,
        out_planes,
        max_out_frames,
        (const uint8_t **)frame->extended_data,
        frame->nb_samples);
    return converted;
}

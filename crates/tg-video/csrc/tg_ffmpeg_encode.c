/*
 * FFmpeg video transcoding / export and synthetic clip generator for Rustgram's
 * video editor, porting Telegram Desktop's media/media_video_encode.cpp.
 *
 * Licensed under GPL-3.0-or-later WITH openvpn-openssl-exception.
 */
#include "tg_ffmpeg_shim.h"

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/audio_fifo.h>
#include <libavutil/channel_layout.h>
#include <libavutil/display.h>
#include <libavutil/error.h>
#include <libavutil/imgutils.h>
#include <libavutil/mathematics.h>
#include <libavutil/mem.h>
#include <libavutil/opt.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>

#include <limits.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef M_PI
#define M_PI 3.14159265358979323846
#endif

/* Mirror decoder shim limits (`tg_ffmpeg_shim.c` / `MAX_FRAME_AREA` = 4K). */
#define TG_ENCODE_MAX_DIM 16384
#define TG_ENCODE_MAX_AREA ((int64_t)3840 * 2160)
#define TG_MAX_PIXELS_PADDING_RATIO 32
#define TG_MAX_PIXELS_FIXED_PADDING (64 * 1024)

static int64_t max_pixels_for_area_limit(int64_t area) {
    return area + area / TG_MAX_PIXELS_PADDING_RATIO + TG_MAX_PIXELS_FIXED_PADDING;
}

static int valid_video_dims(int w, int h) {
    if (w <= 0 || h <= 0 || w > TG_ENCODE_MAX_DIM || h > TG_ENCODE_MAX_DIM) {
        return 0;
    }
    int64_t area = (int64_t)w * (int64_t)h;
    if (area <= 0 || area > TG_ENCODE_MAX_AREA) {
        return 0;
    }
    if (w > INT_MAX / 4) {
        return 0;
    }
    if ((uint64_t)area > SIZE_MAX / 4) {
        return 0;
    }
    return 1;
}

static int validate_frame_planes(const AVFrame *frame, enum AVPixelFormat pix_fmt) {
    if (!frame || frame->width <= 0 || frame->height <= 0 || !frame->data[0]) {
        return 0;
    }
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

static void set_err(char *buf, size_t size, const char *ctx, int errnum) {
    if (!buf || size == 0) return;
    if (errnum < 0) {
        char avbuf[128];
        av_strerror(errnum, avbuf, sizeof(avbuf));
        snprintf(buf, size, "%s: %s (%d)", ctx ? ctx : "ffmpeg", avbuf, errnum);
    } else {
        snprintf(buf, size, "%s", ctx ? ctx : "ffmpeg error");
    }
}

static int even_down_min2(int v) {
    int e = v & ~1;
    return e < 2 ? 2 : e;
}

static int normalize_rot(int r) {
    int m = r % 360;
    if (m < 0) m += 360;
    if (m == 90 || m == 180 || m == 270) return m;
    return 0;
}

static int read_stream_rotation(const AVStream *stream) {
    if (!stream || !stream->codecpar) return 0;
    const AVPacketSideData *dm = av_packet_side_data_get(
        stream->codecpar->coded_side_data,
        stream->codecpar->nb_coded_side_data,
        AV_PKT_DATA_DISPLAYMATRIX);
    if (dm && dm->data && dm->size >= 9 * sizeof(int32_t)) {
        double angle = av_display_rotation_get((const int32_t *)dm->data);
        if (isfinite(angle)) {
            int n = normalize_rot((int)llround(-angle));
            if (n != 0) return n;
        }
    }
    const AVDictionaryEntry *tag = av_dict_get(stream->metadata, "rotate", NULL, 0);
    if (tag && tag->value && tag->value[0] != '\0') {
        return normalize_rot((int)strtol(tag->value, NULL, 10));
    }
    return 0;
}

static const AVCodec *find_video_encoder(void) {
    const AVCodec *c = avcodec_find_encoder_by_name("libx264");
    if (c) return c;
    c = avcodec_find_encoder_by_name("libopenh264");
    if (c) return c;
    c = avcodec_find_encoder(AV_CODEC_ID_H264);
    if (c) return c;
    return avcodec_find_encoder(AV_CODEC_ID_MPEG4);
}

static int64_t compute_target_bitrate(int w, int h, double fps) {
    double r = (fps > 1.0 && fps < 121.0) ? fps : 30.0;
    double bits = (double)w * (double)h * r * 0.07;
    if (bits < 600000.0) bits = 600000.0;
    if (bits > 6800000.0) bits = 6800000.0;
    return (int64_t)bits;
}

/* Rotate RGBA buffer clockwise by 0/90/180/270. Caller frees returned pointer if != src. */
static uint8_t *transform_rgba(
    const uint8_t *src,
    int src_w,
    int src_h,
    int file_rot,
    int crop_x,
    int crop_y,
    int crop_w,
    int crop_h,
    int flip_h,
    int user_rot,
    int *out_w,
    int *out_h) {
    /* 1. Apply file display rotation so pixels are in display orientation. */
    if (!src || !out_w || !out_h || !valid_video_dims(src_w, src_h)) {
        return NULL;
    }
    int disp_w = (file_rot == 90 || file_rot == 270) ? src_h : src_w;
    int disp_h = (file_rot == 90 || file_rot == 270) ? src_w : src_h;
    if (!valid_video_dims(disp_w, disp_h)) {
        return NULL;
    }
    size_t disp_bytes = (size_t)disp_w * (size_t)disp_h * 4;
    uint8_t *disp = (uint8_t *)malloc(disp_bytes);
    if (!disp) return NULL;

    for (int y = 0; y < src_h; ++y) {
        for (int x = 0; x < src_w; ++x) {
            int dx = x, dy = y;
            if (file_rot == 90) {
                dx = src_h - 1 - y;
                dy = x;
            } else if (file_rot == 180) {
                dx = src_w - 1 - x;
                dy = src_h - 1 - y;
            } else if (file_rot == 270) {
                dx = y;
                dy = src_w - 1 - x;
            }
            const uint8_t *sp = src + ((size_t)y * (size_t)src_w + (size_t)x) * 4;
            uint8_t *dp = disp + ((size_t)dy * (size_t)disp_w + (size_t)dx) * 4;
            memcpy(dp, sp, 4);
        }
    }

    /* 2. Clamp crop rectangle inside display dimensions (overflow-safe). */
    int cx = 0, cy = 0, cw = disp_w, ch = disp_h;
    if (crop_w > 0 && crop_h > 0) {
        cx = crop_x < 0 ? 0 : (crop_x >= disp_w ? disp_w - 1 : crop_x);
        cy = crop_y < 0 ? 0 : (crop_y >= disp_h ? disp_h - 1 : crop_y);
        cw = crop_w;
        ch = crop_h;
        if (cw > disp_w - cx) cw = disp_w - cx;
        if (ch > disp_h - cy) ch = disp_h - cy;
        if (cw < 1) cw = 1;
        if (ch < 1) ch = 1;
    }

    /* 3. Apply crop + optional horizontal flip + user rotation. */
    int final_w = (user_rot == 90 || user_rot == 270) ? ch : cw;
    int final_h = (user_rot == 90 || user_rot == 270) ? cw : ch;
    if (!valid_video_dims(final_w, final_h)) {
        free(disp);
        return NULL;
    }
    uint8_t *out = (uint8_t *)malloc((size_t)final_w * (size_t)final_h * 4);
    if (!out) {
        free(disp);
        return NULL;
    }

    for (int y = 0; y < ch; ++y) {
        for (int x = 0; x < cw; ++x) {
            int sx = cx + (flip_h ? (cw - 1 - x) : x);
            int sy = cy + y;
            int dx = x, dy = y;
            if (user_rot == 90) {
                dx = ch - 1 - y;
                dy = x;
            } else if (user_rot == 180) {
                dx = cw - 1 - x;
                dy = ch - 1 - y;
            } else if (user_rot == 270) {
                dx = y;
                dy = cw - 1 - x;
            }
            const uint8_t *sp = disp + ((size_t)sy * (size_t)disp_w + (size_t)sx) * 4;
            uint8_t *dp = out + ((size_t)dy * (size_t)final_w + (size_t)dx) * 4;
            memcpy(dp, sp, 4);
        }
    }

    free(disp);
    *out_w = final_w;
    *out_h = final_h;
    return out;
}

static int drain_encoder_packets(
    AVCodecContext *enc,
    AVFormatContext *out_fmt,
    AVStream *out_stream,
    AVPacket *out_pkt) {
    for (;;) {
        int r = avcodec_receive_packet(enc, out_pkt);
        if (r == AVERROR(EAGAIN) || r == AVERROR_EOF) {
            return 0;
        }
        if (r < 0) {
            return r;
        }
        av_packet_rescale_ts(out_pkt, enc->time_base, out_stream->time_base);
        out_pkt->stream_index = out_stream->index;
        int w = av_interleaved_write_frame(out_fmt, out_pkt);
        av_packet_unref(out_pkt);
        if (w < 0) {
            return w;
        }
    }
}

static int64_t file_size_bytes(const char *path) {
    FILE *f = fopen(path, "rb");
    if (!f) return 0;
#ifdef _WIN32
    _fseeki64(f, 0, SEEK_END);
    int64_t sz = _ftelli64(f);
#else
    fseeko(f, 0, SEEK_END);
    int64_t sz = (int64_t)ftello(f);
#endif
    fclose(f);
    return sz > 0 ? sz : 0;
}

int tg_video_transcode_file(
    const char *input_path,
    const char *output_path,
    const TgVideoEncodeConfig *config,
    TgVideoEncodeResult *out_result,
    char *err_buf,
    size_t err_buf_size) {
    if (!input_path || !output_path || !config) {
        set_err(err_buf, err_buf_size, "invalid transcode arguments", AVERROR(EINVAL));
        return AVERROR(EINVAL);
    }
    if (out_result) {
        memset(out_result, 0, sizeof(*out_result));
    }

    AVFormatContext *in_fmt = NULL;
    AVFormatContext *out_fmt = NULL;
    AVCodecContext *vdec = NULL;
    AVCodecContext *adec = NULL;
    AVCodecContext *venc = NULL;
    AVCodecContext *aenc = NULL;
    struct SwsContext *sws_to_rgba = NULL;
    struct SwsContext *sws_to_yuv = NULL;
    struct SwrContext *swr = NULL;
    AVAudioFifo *audio_fifo = NULL;
    AVPacket *in_pkt = NULL;
    AVPacket *out_pkt = NULL;
    AVFrame *dec_frame = NULL;
    AVFrame *enc_vframe = NULL;
    AVFrame *enc_aframe = NULL;
    uint8_t *rgba_buf = NULL;
    int ret = 0;

    /* Restrict input I/O strictly to local files and Telegram's whitelisted containers/codecs. */
    in_fmt = avformat_alloc_context();
    if (!in_fmt) {
        ret = AVERROR(ENOMEM);
        set_err(err_buf, err_buf_size, "avformat_alloc_context", ret);
        goto cleanup;
    }
    ret = av_opt_set(in_fmt, "protocol_whitelist", "file", 0);
    if (ret >= 0) {
        ret = av_opt_set(in_fmt, "format_whitelist", TG_FORMAT_WHITELIST, 0);
    }
    if (ret >= 0) {
        ret = av_opt_set(in_fmt, "codec_whitelist", TG_CODEC_WHITELIST, 0);
    }
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "av_opt_set(whitelists)", ret);
        goto cleanup;
    }
    AVDictionary *in_opts = NULL;
    av_dict_set(&in_opts, "protocol_whitelist", "file", 0);
    av_dict_set(&in_opts, "format_whitelist", TG_FORMAT_WHITELIST, 0);
    av_dict_set(&in_opts, "codec_whitelist", TG_CODEC_WHITELIST, 0);
    ret = avformat_open_input(&in_fmt, input_path, NULL, &in_opts);
    av_dict_free(&in_opts);
    if (ret < 0) {
        /* avformat_open_input frees in_fmt on failure. */
        in_fmt = NULL;
        set_err(err_buf, err_buf_size, "avformat_open_input", ret);
        goto cleanup;
    }
    ret = avformat_find_stream_info(in_fmt, NULL);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "avformat_find_stream_info", ret);
        goto cleanup;
    }

    int v_idx = av_find_best_stream(in_fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (v_idx < 0) {
        ret = v_idx;
        set_err(err_buf, err_buf_size, "no video stream in input", ret);
        goto cleanup;
    }
    AVStream *in_vstream = in_fmt->streams[v_idx];
    const AVCodec *vdec_codec = avcodec_find_decoder(in_vstream->codecpar->codec_id);
    if (!vdec_codec) {
        ret = AVERROR_DECODER_NOT_FOUND;
        set_err(err_buf, err_buf_size, "video decoder not found", ret);
        goto cleanup;
    }
    vdec = avcodec_alloc_context3(vdec_codec);
    if (!vdec) {
        ret = AVERROR(ENOMEM);
        set_err(err_buf, err_buf_size, "avcodec_alloc_context3(vdec)", ret);
        goto cleanup;
    }
    ret = avcodec_parameters_to_context(vdec, in_vstream->codecpar);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "avcodec_parameters_to_context(vdec)", ret);
        goto cleanup;
    }
    if ((vdec->width != 0 || vdec->height != 0) && !valid_video_dims(vdec->width, vdec->height)) {
        ret = AVERROR_INVALIDDATA;
        set_err(err_buf, err_buf_size, "invalid or oversized codecpar video dimensions", ret);
        goto cleanup;
    }
    vdec->max_pixels = max_pixels_for_area_limit(TG_ENCODE_MAX_AREA);
    ret = av_opt_set(vdec, "codec_whitelist", TG_CODEC_WHITELIST, 0);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "av_opt_set(vdec codec_whitelist)", ret);
        goto cleanup;
    }
    ret = avcodec_open2(vdec, vdec_codec, NULL);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "avcodec_open2(vdec)", ret);
        goto cleanup;
    }

    int file_rot = read_stream_rotation(in_vstream);
    int user_rot = normalize_rot(config->rotation);
    int flip_h = config->flipped ? 1 : 0;

    int coded_w = vdec->width;
    int coded_h = vdec->height;
    if (!valid_video_dims(coded_w, coded_h)) {
        ret = AVERROR_INVALIDDATA;
        set_err(err_buf, err_buf_size, "invalid or oversized input video dimensions", ret);
        goto cleanup;
    }
    int disp_w = (file_rot == 90 || file_rot == 270) ? coded_h : coded_w;
    int disp_h = (file_rot == 90 || file_rot == 270) ? coded_w : coded_h;

    int eff_w = disp_w;
    int eff_h = disp_h;
    if (config->crop_w > 0 && config->crop_h > 0) {
        eff_w = config->crop_w < disp_w ? config->crop_w : disp_w;
        eff_h = config->crop_h < disp_h ? config->crop_h : disp_h;
    }
    if (user_rot == 90 || user_rot == 270) {
        int tmp = eff_w;
        eff_w = eff_h;
        eff_h = tmp;
    }

    int out_w = (config->target_width > 0) ? even_down_min2(config->target_width) : even_down_min2(eff_w);
    int out_h = (config->target_height > 0) ? even_down_min2(config->target_height) : even_down_min2(eff_h);
    if (!valid_video_dims(out_w, out_h)) {
        ret = AVERROR(EINVAL);
        set_err(err_buf, err_buf_size, "invalid or oversized output video dimensions", ret);
        goto cleanup;
    }

    AVRational guessed_fps = av_guess_frame_rate(in_fmt, in_vstream, NULL);
    double src_fps = (guessed_fps.num > 0 && guessed_fps.den > 0) ? av_q2d(guessed_fps) : 30.0;
    if (src_fps < 1.0 || src_fps > 120.0) src_fps = 30.0;
    double out_fps = src_fps;
    if (config->fps_limit > 1.0 && config->fps_limit < out_fps) {
        out_fps = config->fps_limit;
    }

    int want_audio = (!config->remove_audio && config->audio_volume > 0.0001f) ? 1 : 0;
    int a_idx = want_audio ? av_find_best_stream(in_fmt, AVMEDIA_TYPE_AUDIO, -1, v_idx, NULL, 0) : -1;
    AVStream *in_astream = (a_idx >= 0) ? in_fmt->streams[a_idx] : NULL;
    if (in_astream) {
        const AVCodec *adec_codec = avcodec_find_decoder(in_astream->codecpar->codec_id);
        if (adec_codec) {
            adec = avcodec_alloc_context3(adec_codec);
            if (adec &&
                avcodec_parameters_to_context(adec, in_astream->codecpar) >= 0 &&
                av_opt_set(adec, "codec_whitelist", TG_CODEC_WHITELIST, 0) >= 0 &&
                avcodec_open2(adec, adec_codec, NULL) >= 0) {
                /* audio decoder opened */
            } else {
                avcodec_free_context(&adec);
                in_astream = NULL;
                a_idx = -1;
            }
        } else {
            in_astream = NULL;
            a_idx = -1;
        }
    }

    /* Open output MP4 container. */
    ret = avformat_alloc_output_context2(&out_fmt, NULL, "mp4", output_path);
    if (ret < 0 || !out_fmt) {
        set_err(err_buf, err_buf_size, "avformat_alloc_output_context2", ret < 0 ? ret : AVERROR(ENOMEM));
        ret = ret < 0 ? ret : AVERROR(ENOMEM);
        goto cleanup;
    }

    const AVCodec *venc_codec = find_video_encoder();
    if (!venc_codec) {
        ret = AVERROR_ENCODER_NOT_FOUND;
        set_err(err_buf, err_buf_size, "no H.264/MPEG4 encoder found", ret);
        goto cleanup;
    }
    AVStream *out_vstream = avformat_new_stream(out_fmt, venc_codec);
    if (!out_vstream) {
        ret = AVERROR(ENOMEM);
        set_err(err_buf, err_buf_size, "avformat_new_stream(video)", ret);
        goto cleanup;
    }
    venc = avcodec_alloc_context3(venc_codec);
    if (!venc) {
        ret = AVERROR(ENOMEM);
        set_err(err_buf, err_buf_size, "avcodec_alloc_context3(venc)", ret);
        goto cleanup;
    }
    venc->codec_id = venc_codec->id;
    venc->codec_type = AVMEDIA_TYPE_VIDEO;
    venc->width = out_w;
    venc->height = out_h;
    venc->time_base = (AVRational){ 1, 1000000 };
    venc->framerate = av_d2q(out_fps, 1000000);
    venc->pix_fmt = AV_PIX_FMT_YUV420P;
    venc->bit_rate = (config->video_bitrate > 0)
        ? config->video_bitrate
        : compute_target_bitrate(out_w, out_h, out_fps);
    venc->gop_size = (int)llround(out_fps);
    if (venc->gop_size < 1) venc->gop_size = 30;
    if (out_fmt->oformat->flags & AVFMT_GLOBALHEADER) {
        venc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
    }
    if (strcmp(venc_codec->name, "libx264") == 0) {
        av_opt_set(venc->priv_data, "preset", "ultrafast", 0);
        av_opt_set(venc->priv_data, "tune", "zerolatency", 0);
    }
    ret = avcodec_open2(venc, venc_codec, NULL);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "avcodec_open2(venc)", ret);
        goto cleanup;
    }
    ret = avcodec_parameters_from_context(out_vstream->codecpar, venc);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "avcodec_parameters_from_context(video)", ret);
        goto cleanup;
    }
    out_vstream->time_base = venc->time_base;

    AVStream *out_astream = NULL;
    if (adec && in_astream) {
        const AVCodec *aenc_codec = avcodec_find_encoder(AV_CODEC_ID_AAC);
        if (aenc_codec) {
            out_astream = avformat_new_stream(out_fmt, aenc_codec);
            aenc = avcodec_alloc_context3(aenc_codec);
            if (out_astream && aenc) {
                int dst_channels = adec->ch_layout.nb_channels <= 1 ? 1 : 2;
                av_channel_layout_default(&aenc->ch_layout, dst_channels);
                aenc->sample_rate = 48000;
                aenc->sample_fmt = AV_SAMPLE_FMT_FLTP;
                aenc->bit_rate = 64000 * dst_channels;
                aenc->time_base = (AVRational){ 1, aenc->sample_rate };
                if (out_fmt->oformat->flags & AVFMT_GLOBALHEADER) {
                    aenc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
                }
                if (avcodec_open2(aenc, aenc_codec, NULL) >= 0 &&
                    avcodec_parameters_from_context(out_astream->codecpar, aenc) >= 0) {
                    out_astream->time_base = aenc->time_base;
                    AVChannelLayout src_layout;
                    if (av_channel_layout_check(&adec->ch_layout)) {
                        av_channel_layout_copy(&src_layout, &adec->ch_layout);
                    } else {
                        av_channel_layout_default(&src_layout, dst_channels);
                    }
                    int swr_err = swr_alloc_set_opts2(
                        &swr,
                        &aenc->ch_layout,
                        aenc->sample_fmt,
                        aenc->sample_rate,
                        &src_layout,
                        adec->sample_fmt,
                        adec->sample_rate > 0 ? adec->sample_rate : 48000,
                        0,
                        NULL);
                    av_channel_layout_uninit(&src_layout);
                    if (swr_err < 0 || !swr || swr_init(swr) < 0) {
                        if (swr) swr_free(&swr);
                        avcodec_free_context(&aenc);
                        out_astream = NULL;
                    } else {
                        audio_fifo = av_audio_fifo_alloc(aenc->sample_fmt, aenc->ch_layout.nb_channels, 4096);
                        if (!audio_fifo) {
                            swr_free(&swr);
                            avcodec_free_context(&aenc);
                            out_astream = NULL;
                        }
                    }
                } else {
                    avcodec_free_context(&aenc);
                    out_astream = NULL;
                }
            }
        }
    }

    if (!(out_fmt->oformat->flags & AVFMT_NOFILE)) {
        AVDictionary *out_io_opts = NULL;
        av_dict_set(&out_io_opts, "protocol_whitelist", "file", 0);
        ret = avio_open2(&out_fmt->pb, output_path, AVIO_FLAG_WRITE, NULL, &out_io_opts);
        av_dict_free(&out_io_opts);
        if (ret < 0) {
            set_err(err_buf, err_buf_size, "avio_open2(output)", ret);
            goto cleanup;
        }
    }

    AVDictionary *mux_opts = NULL;
    av_dict_set(&mux_opts, "movflags", "+faststart", 0);
    ret = avformat_write_header(out_fmt, &mux_opts);
    av_dict_free(&mux_opts);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "avformat_write_header", ret);
        goto cleanup;
    }

    in_pkt = av_packet_alloc();
    out_pkt = av_packet_alloc();
    dec_frame = av_frame_alloc();
    enc_vframe = av_frame_alloc();
    enc_aframe = av_frame_alloc();
    if (!in_pkt || !out_pkt || !dec_frame || !enc_vframe || !enc_aframe) {
        ret = AVERROR(ENOMEM);
        set_err(err_buf, err_buf_size, "av_frame_alloc", ret);
        goto cleanup;
    }

    enc_vframe->format = AV_PIX_FMT_YUV420P;
    enc_vframe->width = out_w;
    enc_vframe->height = out_h;
    ret = av_frame_get_buffer(enc_vframe, 32);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "av_frame_get_buffer(video)", ret);
        goto cleanup;
    }

    int64_t from_us = config->from_ms > 0 ? config->from_ms * 1000 : 0;
    int64_t till_us = config->till_ms > 0 ? config->till_ms * 1000 : INT64_MAX;
    if (from_us > 0) {
        av_seek_frame(in_fmt, -1, from_us, AVSEEK_FLAG_BACKWARD);
    }

    int need_rgba_transform =
        (file_rot != 0) || (user_rot != 0) || (flip_h != 0) ||
        (config->crop_w > 0 && config->crop_h > 0);
    if (need_rgba_transform) {
        if (!valid_video_dims(coded_w, coded_h)) {
            ret = AVERROR_INVALIDDATA;
            set_err(err_buf, err_buf_size, "invalid rgba_buf dimensions", ret);
            goto cleanup;
        }
        rgba_buf = (uint8_t *)malloc((size_t)coded_w * (size_t)coded_h * 4);
        if (!rgba_buf) {
            ret = AVERROR(ENOMEM);
            set_err(err_buf, err_buf_size, "malloc(rgba_buf)", ret);
            goto cleanup;
        }
    }

    int64_t min_frame_step_us = (config->fps_limit > 1.0)
        ? (int64_t)llround(1000000.0 / config->fps_limit)
        : 0;
    int64_t last_video_pts_us = -1;
    int64_t video_frames_written = 0;
    int64_t audio_samples_written = 0;
    int64_t first_video_us = -1;
    float gain = config->audio_volume;
    if (gain < 0.0f) gain = 0.0f;
    if (gain > 2.0f) gain = 2.0f;

    int demux_done = 0;
    while (!demux_done) {
        int r = av_read_frame(in_fmt, in_pkt);
        if (r == AVERROR_EOF) {
            demux_done = 1;
            avcodec_send_packet(vdec, NULL);
            if (adec && aenc) avcodec_send_packet(adec, NULL);
        } else if (r < 0) {
            ret = r;
            set_err(err_buf, err_buf_size, "av_read_frame", ret);
            goto cleanup;
        } else {
            if (in_pkt->stream_index == v_idx) {
                avcodec_send_packet(vdec, in_pkt);
            } else if (a_idx >= 0 && in_pkt->stream_index == a_idx && adec && aenc) {
                avcodec_send_packet(adec, in_pkt);
            }
            av_packet_unref(in_pkt);
        }

        /* Drain decoded video frames. */
        for (;;) {
            int vr = avcodec_receive_frame(vdec, dec_frame);
            if (vr == AVERROR(EAGAIN) || vr == AVERROR_EOF) break;
            if (vr < 0) {
                ret = vr;
                set_err(err_buf, err_buf_size, "avcodec_receive_frame(video)", ret);
                goto cleanup;
            }
            int64_t raw_pts = (dec_frame->best_effort_timestamp != AV_NOPTS_VALUE)
                ? dec_frame->best_effort_timestamp
                : dec_frame->pts;
            int64_t pts_us = (raw_pts != AV_NOPTS_VALUE)
                ? av_rescale_q(raw_pts, in_vstream->time_base, (AVRational){ 1, 1000000 })
                : (int64_t)llround((double)video_frames_written * (1000000.0 / out_fps));

            if (pts_us < from_us) {
                av_frame_unref(dec_frame);
                continue;
            }
            if (pts_us > till_us) {
                av_frame_unref(dec_frame);
                demux_done = 1;
                break;
            }
            if (first_video_us < 0) {
                first_video_us = from_us > 0 ? from_us : pts_us;
            }
            int64_t out_pts_us = pts_us - first_video_us;
            if (out_pts_us < 0) out_pts_us = 0;
            if (last_video_pts_us >= 0) {
                if (min_frame_step_us > 0 && (out_pts_us - last_video_pts_us) < min_frame_step_us) {
                    av_frame_unref(dec_frame);
                    continue;
                }
                if (out_pts_us <= last_video_pts_us) {
                    out_pts_us = last_video_pts_us + 1000;
                }
            }

            ret = av_frame_make_writable(enc_vframe);
            if (ret < 0) {
                av_frame_unref(dec_frame);
                set_err(err_buf, err_buf_size, "av_frame_make_writable", ret);
                goto cleanup;
            }

            /* Validate decoded source frame dimensions, pixel format, and plane strides before sws_scale. */
            if (!valid_video_dims(dec_frame->width, dec_frame->height) ||
                dec_frame->format <= AV_PIX_FMT_NONE ||
                dec_frame->format >= AV_PIX_FMT_NB ||
                !validate_frame_planes(dec_frame, (enum AVPixelFormat)dec_frame->format) ||
                !validate_frame_planes(enc_vframe, AV_PIX_FMT_YUV420P)) {
                av_frame_unref(dec_frame);
                ret = AVERROR_INVALIDDATA;
                set_err(err_buf, err_buf_size, "invalid decoded video frame geometry or stride", ret);
                goto cleanup;
            }

            if (need_rgba_transform) {
                sws_to_rgba = sws_getCachedContext(
                    sws_to_rgba,
                    dec_frame->width,
                    dec_frame->height,
                    (enum AVPixelFormat)dec_frame->format,
                    coded_w,
                    coded_h,
                    AV_PIX_FMT_RGBA,
                    SWS_BILINEAR,
                    NULL, NULL, NULL);
                if (!sws_to_rgba) {
                    av_frame_unref(dec_frame);
                    ret = AVERROR(ENOMEM);
                    set_err(err_buf, err_buf_size, "sws_getCachedContext(rgba)", ret);
                    goto cleanup;
                }
                uint8_t *rgba_planes[4] = { rgba_buf, NULL, NULL, NULL };
                int rgba_linesize[4] = { coded_w * 4, 0, 0, 0 };
                int rgba_scaled = sws_scale(
                    sws_to_rgba,
                    (const uint8_t *const *)dec_frame->data,
                    dec_frame->linesize,
                    0,
                    dec_frame->height,
                    rgba_planes,
                    rgba_linesize);
                if (rgba_scaled != coded_h) {
                    av_frame_unref(dec_frame);
                    ret = AVERROR_EXTERNAL;
                    set_err(err_buf, err_buf_size, "sws_scale(rgba)", ret);
                    goto cleanup;
                }

                int tw = 0, th = 0;
                uint8_t *transformed = transform_rgba(
                    rgba_buf,
                    coded_w,
                    coded_h,
                    file_rot,
                    config->crop_x,
                    config->crop_y,
                    config->crop_w,
                    config->crop_h,
                    flip_h,
                    user_rot,
                    &tw,
                    &th);
                if (!transformed || !valid_video_dims(tw, th)) {
                    free(transformed);
                    av_frame_unref(dec_frame);
                    ret = AVERROR(ENOMEM);
                    set_err(err_buf, err_buf_size, "transform_rgba", ret);
                    goto cleanup;
                }

                sws_to_yuv = sws_getCachedContext(
                    sws_to_yuv,
                    tw,
                    th,
                    AV_PIX_FMT_RGBA,
                    out_w,
                    out_h,
                    AV_PIX_FMT_YUV420P,
                    SWS_BILINEAR,
                    NULL, NULL, NULL);
                if (!sws_to_yuv) {
                    free(transformed);
                    av_frame_unref(dec_frame);
                    ret = AVERROR(ENOMEM);
                    set_err(err_buf, err_buf_size, "sws_getCachedContext(yuv)", ret);
                    goto cleanup;
                }
                const uint8_t *src_planes[4] = { transformed, NULL, NULL, NULL };
                int src_linesize[4] = { tw * 4, 0, 0, 0 };
                int yuv_scaled = sws_scale(
                    sws_to_yuv,
                    src_planes,
                    src_linesize,
                    0,
                    th,
                    enc_vframe->data,
                    enc_vframe->linesize);
                free(transformed);
                if (yuv_scaled != out_h) {
                    av_frame_unref(dec_frame);
                    ret = AVERROR_EXTERNAL;
                    set_err(err_buf, err_buf_size, "sws_scale(yuv)", ret);
                    goto cleanup;
                }
            } else {
                sws_to_yuv = sws_getCachedContext(
                    sws_to_yuv,
                    dec_frame->width,
                    dec_frame->height,
                    (enum AVPixelFormat)dec_frame->format,
                    out_w,
                    out_h,
                    AV_PIX_FMT_YUV420P,
                    SWS_BILINEAR,
                    NULL, NULL, NULL);
                if (!sws_to_yuv) {
                    av_frame_unref(dec_frame);
                    ret = AVERROR(ENOMEM);
                    set_err(err_buf, err_buf_size, "sws_getCachedContext(direct)", ret);
                    goto cleanup;
                }
                int direct_scaled = sws_scale(
                    sws_to_yuv,
                    (const uint8_t *const *)dec_frame->data,
                    dec_frame->linesize,
                    0,
                    dec_frame->height,
                    enc_vframe->data,
                    enc_vframe->linesize);
                if (direct_scaled != out_h) {
                    av_frame_unref(dec_frame);
                    ret = AVERROR_EXTERNAL;
                    set_err(err_buf, err_buf_size, "sws_scale(direct)", ret);
                    goto cleanup;
                }
            }

            enc_vframe->pts = out_pts_us;
            last_video_pts_us = out_pts_us;
            video_frames_written++;
            av_frame_unref(dec_frame);

            ret = avcodec_send_frame(venc, enc_vframe);
            if (ret < 0) {
                set_err(err_buf, err_buf_size, "avcodec_send_frame(video)", ret);
                goto cleanup;
            }
            ret = drain_encoder_packets(venc, out_fmt, out_vstream, out_pkt);
            if (ret < 0) {
                set_err(err_buf, err_buf_size, "drain_encoder_packets(video)", ret);
                goto cleanup;
            }
        }

        /* Drain decoded audio frames. */
        if (adec && aenc && swr && audio_fifo && out_astream) {
            for (;;) {
                int ar = avcodec_receive_frame(adec, dec_frame);
                if (ar == AVERROR(EAGAIN) || ar == AVERROR_EOF) break;
                if (ar < 0) break;

                int64_t araw = (dec_frame->best_effort_timestamp != AV_NOPTS_VALUE)
                    ? dec_frame->best_effort_timestamp
                    : dec_frame->pts;
                int64_t apts_us = (araw != AV_NOPTS_VALUE)
                    ? av_rescale_q(araw, in_astream->time_base, (AVRational){ 1, 1000000 })
                    : 0;
                if (apts_us < from_us || apts_us > till_us) {
                    av_frame_unref(dec_frame);
                    continue;
                }

                int dst_ch = aenc->ch_layout.nb_channels;
                int64_t delay = swr_get_delay(swr, dec_frame->sample_rate > 0 ? dec_frame->sample_rate : 48000);
                int out_nb = (int)av_rescale_rnd(
                    delay + dec_frame->nb_samples,
                    aenc->sample_rate,
                    dec_frame->sample_rate > 0 ? dec_frame->sample_rate : 48000,
                    AV_ROUND_UP);
                if (out_nb > 0) {
                    uint8_t **conv = NULL;
                    int conv_linesize = 0;
                    if (av_samples_alloc_array_and_samples(
                            &conv,
                            &conv_linesize,
                            dst_ch,
                            out_nb,
                            aenc->sample_fmt,
                            0) >= 0) {
                        int got = swr_convert(
                            swr,
                            conv,
                            out_nb,
                            (const uint8_t **)dec_frame->extended_data,
                            dec_frame->nb_samples);
                        if (got > 0) {
                            if (fabsf(gain - 1.0f) > 0.001f) {
                                for (int ch = 0; ch < dst_ch; ++ch) {
                                    float *samples = (float *)conv[ch];
                                    for (int s = 0; s < got; ++s) {
                                        float v = samples[s] * gain;
                                        if (v > 1.0f) v = 1.0f;
                                        if (v < -1.0f) v = -1.0f;
                                        samples[s] = v;
                                    }
                                }
                            }
                            av_audio_fifo_write(audio_fifo, (void **)conv, got);
                        }
                        av_freep(&conv[0]);
                        av_freep(&conv);
                    }
                }
                av_frame_unref(dec_frame);

                int frame_sz = aenc->frame_size > 0 ? aenc->frame_size : 1024;
                while (av_audio_fifo_size(audio_fifo) >= frame_sz) {
                    av_frame_unref(enc_aframe);
                    enc_aframe->nb_samples = frame_sz;
                    av_channel_layout_copy(&enc_aframe->ch_layout, &aenc->ch_layout);
                    enc_aframe->format = aenc->sample_fmt;
                    enc_aframe->sample_rate = aenc->sample_rate;
                    if (av_frame_get_buffer(enc_aframe, 0) < 0) break;
                    if (av_audio_fifo_read(audio_fifo, (void **)enc_aframe->data, frame_sz) < frame_sz) {
                        break;
                    }
                    enc_aframe->pts = audio_samples_written;
                    audio_samples_written += frame_sz;
                    if (avcodec_send_frame(aenc, enc_aframe) >= 0) {
                        drain_encoder_packets(aenc, out_fmt, out_astream, out_pkt);
                    }
                }
            }
        }
    }

    /* Flush remaining audio samples & encoders. */
    if (aenc && out_astream && audio_fifo) {
        int rem = av_audio_fifo_size(audio_fifo);
        if (rem > 0) {
            av_frame_unref(enc_aframe);
            enc_aframe->nb_samples = rem;
            av_channel_layout_copy(&enc_aframe->ch_layout, &aenc->ch_layout);
            enc_aframe->format = aenc->sample_fmt;
            enc_aframe->sample_rate = aenc->sample_rate;
            if (av_frame_get_buffer(enc_aframe, 0) >= 0 &&
                av_audio_fifo_read(audio_fifo, (void **)enc_aframe->data, rem) == rem) {
                enc_aframe->pts = audio_samples_written;
                audio_samples_written += rem;
                if (avcodec_send_frame(aenc, enc_aframe) >= 0) {
                    drain_encoder_packets(aenc, out_fmt, out_astream, out_pkt);
                }
            }
        }
        avcodec_send_frame(aenc, NULL);
        drain_encoder_packets(aenc, out_fmt, out_astream, out_pkt);
    }

    avcodec_send_frame(venc, NULL);
    ret = drain_encoder_packets(venc, out_fmt, out_vstream, out_pkt);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "flush video encoder", ret);
        goto cleanup;
    }

    ret = av_write_trailer(out_fmt);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "av_write_trailer", ret);
        goto cleanup;
    }

    if (out_fmt->pb && !(out_fmt->oformat->flags & AVFMT_NOFILE)) {
        avio_closep(&out_fmt->pb);
    }

    if (out_result) {
        int64_t frame_dur_ms = (int64_t)llround(1000.0 / out_fps);
        int64_t dur_ms = (last_video_pts_us >= 0)
            ? (last_video_pts_us / 1000) + frame_dur_ms
            : 0;
        out_result->output_width = out_w;
        out_result->output_height = out_h;
        out_result->duration_ms = dur_ms;
        out_result->has_audio = (out_astream != NULL && audio_samples_written > 0) ? 1 : 0;
        out_result->file_size = file_size_bytes(output_path);
    }
    ret = 0;

cleanup:
    if (rgba_buf) free(rgba_buf);
    if (sws_to_rgba) sws_freeContext(sws_to_rgba);
    if (sws_to_yuv) sws_freeContext(sws_to_yuv);
    if (swr) swr_free(&swr);
    if (audio_fifo) av_audio_fifo_free(audio_fifo);
    if (dec_frame) av_frame_free(&dec_frame);
    if (enc_vframe) av_frame_free(&enc_vframe);
    if (enc_aframe) av_frame_free(&enc_aframe);
    if (in_pkt) av_packet_free(&in_pkt);
    if (out_pkt) av_packet_free(&out_pkt);
    if (vdec) avcodec_free_context(&vdec);
    if (adec) avcodec_free_context(&adec);
    if (venc) avcodec_free_context(&venc);
    if (aenc) avcodec_free_context(&aenc);
    if (out_fmt) {
        if (out_fmt->pb && !(out_fmt->oformat->flags & AVFMT_NOFILE)) {
            avio_closep(&out_fmt->pb);
        }
        avformat_free_context(out_fmt);
    }
    if (in_fmt) avformat_close_input(&in_fmt);
    return ret;
}

int tg_video_generate_test_clip(
    const char *output_path,
    int width,
    int height,
    int fps,
    int duration_ms,
    int with_audio,
    char *err_buf,
    size_t err_buf_size) {
    if (!output_path || width < 16 || height < 16 || fps < 1 || duration_ms < 100) {
        set_err(err_buf, err_buf_size, "invalid synthetic clip parameters", AVERROR(EINVAL));
        return AVERROR(EINVAL);
    }
    int w = even_down_min2(width);
    int h = even_down_min2(height);
    if (!valid_video_dims(w, h)) {
        set_err(err_buf, err_buf_size, "oversized synthetic clip dimensions", AVERROR(EINVAL));
        return AVERROR(EINVAL);
    }
    int total_frames = (int)(((int64_t)duration_ms * fps + 999) / 1000);
    if (total_frames < 2) total_frames = 2;

    AVFormatContext *out_fmt = NULL;
    AVCodecContext *venc = NULL;
    AVCodecContext *aenc = NULL;
    AVFrame *vframe = NULL;
    AVFrame *aframe = NULL;
    AVPacket *pkt = NULL;
    int ret = 0;

    ret = avformat_alloc_output_context2(&out_fmt, NULL, "mp4", output_path);
    if (ret < 0 || !out_fmt) {
        set_err(err_buf, err_buf_size, "avformat_alloc_output_context2", ret < 0 ? ret : AVERROR(ENOMEM));
        ret = ret < 0 ? ret : AVERROR(ENOMEM);
        goto cleanup;
    }

    const AVCodec *vcodec = find_video_encoder();
    if (!vcodec) {
        ret = AVERROR_ENCODER_NOT_FOUND;
        set_err(err_buf, err_buf_size, "no video encoder available", ret);
        goto cleanup;
    }
    AVStream *vstream = avformat_new_stream(out_fmt, vcodec);
    venc = avcodec_alloc_context3(vcodec);
    if (!vstream || !venc) {
        ret = AVERROR(ENOMEM);
        set_err(err_buf, err_buf_size, "alloc video encoder", ret);
        goto cleanup;
    }
    venc->codec_id = vcodec->id;
    venc->codec_type = AVMEDIA_TYPE_VIDEO;
    venc->width = w;
    venc->height = h;
    venc->time_base = (AVRational){ 1, fps };
    venc->framerate = (AVRational){ fps, 1 };
    venc->pix_fmt = AV_PIX_FMT_YUV420P;
    venc->bit_rate = 400000;
    venc->gop_size = fps;
    if (out_fmt->oformat->flags & AVFMT_GLOBALHEADER) {
        venc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
    }
    if (strcmp(vcodec->name, "libx264") == 0) {
        av_opt_set(venc->priv_data, "preset", "ultrafast", 0);
        av_opt_set(venc->priv_data, "tune", "zerolatency", 0);
    }
    ret = avcodec_open2(venc, vcodec, NULL);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "avcodec_open2(test video)", ret);
        goto cleanup;
    }
    ret = avcodec_parameters_from_context(vstream->codecpar, venc);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "avcodec_parameters_from_context(test video)", ret);
        goto cleanup;
    }
    vstream->time_base = venc->time_base;

    AVStream *astream = NULL;
    if (with_audio) {
        const AVCodec *acodec = avcodec_find_encoder(AV_CODEC_ID_AAC);
        if (acodec) {
            astream = avformat_new_stream(out_fmt, acodec);
            aenc = avcodec_alloc_context3(acodec);
            if (astream && aenc) {
                av_channel_layout_default(&aenc->ch_layout, 1);
                aenc->sample_rate = 48000;
                aenc->sample_fmt = AV_SAMPLE_FMT_FLTP;
                aenc->bit_rate = 64000;
                aenc->time_base = (AVRational){ 1, 48000 };
                if (out_fmt->oformat->flags & AVFMT_GLOBALHEADER) {
                    aenc->flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
                }
                if (avcodec_open2(aenc, acodec, NULL) >= 0 &&
                    avcodec_parameters_from_context(astream->codecpar, aenc) >= 0) {
                    astream->time_base = aenc->time_base;
                } else {
                    avcodec_free_context(&aenc);
                    astream = NULL;
                }
            }
        }
    }

    if (!(out_fmt->oformat->flags & AVFMT_NOFILE)) {
        AVDictionary *out_io_opts = NULL;
        av_dict_set(&out_io_opts, "protocol_whitelist", "file", 0);
        ret = avio_open2(&out_fmt->pb, output_path, AVIO_FLAG_WRITE, NULL, &out_io_opts);
        av_dict_free(&out_io_opts);
        if (ret < 0) {
            set_err(err_buf, err_buf_size, "avio_open2(test clip)", ret);
            goto cleanup;
        }
    }

    AVDictionary *opts = NULL;
    av_dict_set(&opts, "movflags", "+faststart", 0);
    ret = avformat_write_header(out_fmt, &opts);
    av_dict_free(&opts);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "avformat_write_header(test clip)", ret);
        goto cleanup;
    }

    pkt = av_packet_alloc();
    vframe = av_frame_alloc();
    vframe->format = AV_PIX_FMT_YUV420P;
    vframe->width = w;
    vframe->height = h;
    ret = av_frame_get_buffer(vframe, 32);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "av_frame_get_buffer(test video)", ret);
        goto cleanup;
    }

    /* Generate moving luma/chroma pattern so each timestamp has a distinct visual frame. */
    for (int i = 0; i < total_frames; ++i) {
        ret = av_frame_make_writable(vframe);
        if (ret < 0) goto cleanup;
        for (int y = 0; y < h; ++y) {
            for (int x = 0; x < w; ++x) {
                vframe->data[0][y * vframe->linesize[0] + x] = (uint8_t)((x + y + i * 7) & 0xFF);
            }
        }
        for (int y = 0; y < h / 2; ++y) {
            for (int x = 0; x < w / 2; ++x) {
                vframe->data[1][y * vframe->linesize[1] + x] = (uint8_t)((128 + y + i * 4) & 0xFF);
                vframe->data[2][y * vframe->linesize[2] + x] = (uint8_t)((64 + x + i * 5) & 0xFF);
            }
        }
        vframe->pts = i;
        ret = avcodec_send_frame(venc, vframe);
        if (ret < 0) {
            set_err(err_buf, err_buf_size, "avcodec_send_frame(test video)", ret);
            goto cleanup;
        }
        ret = drain_encoder_packets(venc, out_fmt, vstream, pkt);
        if (ret < 0) {
            set_err(err_buf, err_buf_size, "drain(test video)", ret);
            goto cleanup;
        }
    }

    if (aenc && astream) {
        int frame_sz = aenc->frame_size > 0 ? aenc->frame_size : 1024;
        int64_t total_samples = ((int64_t)duration_ms * aenc->sample_rate) / 1000;
        aframe = av_frame_alloc();
        aframe->nb_samples = frame_sz;
        av_channel_layout_copy(&aframe->ch_layout, &aenc->ch_layout);
        aframe->format = aenc->sample_fmt;
        aframe->sample_rate = aenc->sample_rate;
        ret = av_frame_get_buffer(aframe, 0);
        if (ret < 0) {
            set_err(err_buf, err_buf_size, "av_frame_get_buffer(test audio)", ret);
            goto cleanup;
        }
        int64_t sample_idx = 0;
        while (sample_idx < total_samples) {
            ret = av_frame_make_writable(aframe);
            if (ret < 0) goto cleanup;
            float *ch0 = (float *)aframe->data[0];
            for (int s = 0; s < frame_sz; ++s) {
                double t = (double)(sample_idx + s) / (double)aenc->sample_rate;
                ch0[s] = (float)(0.25 * sin(2.0 * M_PI * 440.0 * t));
            }
            aframe->pts = sample_idx;
            sample_idx += frame_sz;
            if (avcodec_send_frame(aenc, aframe) >= 0) {
                drain_encoder_packets(aenc, out_fmt, astream, pkt);
            }
        }
        avcodec_send_frame(aenc, NULL);
        drain_encoder_packets(aenc, out_fmt, astream, pkt);
    }

    avcodec_send_frame(venc, NULL);
    ret = drain_encoder_packets(venc, out_fmt, vstream, pkt);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "flush(test video)", ret);
        goto cleanup;
    }

    ret = av_write_trailer(out_fmt);
    if (ret < 0) {
        set_err(err_buf, err_buf_size, "av_write_trailer(test clip)", ret);
        goto cleanup;
    }
    ret = 0;

cleanup:
    if (vframe) av_frame_free(&vframe);
    if (aframe) av_frame_free(&aframe);
    if (pkt) av_packet_free(&pkt);
    if (venc) avcodec_free_context(&venc);
    if (aenc) avcodec_free_context(&aenc);
    if (out_fmt) {
        if (out_fmt->pb && !(out_fmt->oformat->flags & AVFMT_NOFILE)) {
            avio_closep(&out_fmt->pb);
        }
        avformat_free_context(out_fmt);
    }
    return ret;
}

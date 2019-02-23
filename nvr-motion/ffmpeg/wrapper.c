// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2017 Scott Lamb <slamb@slamb.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.
/* vim: set sw=4 et: */

#include <libavcodec/avcodec.h>
#include <libavcodec/version.h>
#include <libavformat/avformat.h>
#include <libavformat/version.h>
#include <libavutil/avutil.h>
#include <libavutil/dict.h>
#include <libavutil/version.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdlib.h>

const int moonfire_ffmpeg_compiled_libavcodec_version = LIBAVCODEC_VERSION_INT;
const int moonfire_ffmpeg_compiled_libavformat_version = LIBAVFORMAT_VERSION_INT;
const int moonfire_ffmpeg_compiled_libavutil_version = LIBAVUTIL_VERSION_INT;

const int moonfire_ffmpeg_av_dict_ignore_suffix = AV_DICT_IGNORE_SUFFIX;

const int64_t moonfire_ffmpeg_av_nopts_value = AV_NOPTS_VALUE;

const int moonfire_ffmpeg_avmedia_type_video = AVMEDIA_TYPE_VIDEO;

const int moonfire_ffmpeg_av_codec_id_h264 = AV_CODEC_ID_H264;

const int moonfire_ffmpeg_averror_decoder_not_found = AVERROR_DECODER_NOT_FOUND;
const int moonfire_ffmpeg_averror_eof = AVERROR_EOF;
const int moonfire_ffmpeg_averror_enomem = AVERROR(ENOMEM);
const int moonfire_ffmpeg_averror_unknown = AVERROR_UNKNOWN;

static int lock_callback(void **mutex, enum AVLockOp op) {
    switch (op) {
        case AV_LOCK_CREATE:
            *mutex = malloc(sizeof(pthread_mutex_t));
            if (*mutex == NULL)
                return -1;
            if (pthread_mutex_init(*mutex, NULL) != 0)
                return -1;
            break;
        case AV_LOCK_DESTROY:
            if (pthread_mutex_destroy(*mutex) != 0)
                return -1;
            free(*mutex);
            *mutex = NULL;
            break;
        case AV_LOCK_OBTAIN:
            if (pthread_mutex_lock(*mutex) != 0)
                return -1;
            break;
        case AV_LOCK_RELEASE:
            if (pthread_mutex_unlock(*mutex) != 0)
                return -1;
            break;
        default:
            return -1;
    }
    return 0;
}

void moonfire_ffmpeg_init(void) {
    if (av_lockmgr_register(&lock_callback) < 0) {
        abort();
    }
}

struct moonfire_ffmpeg_streams {
    AVStream** streams;
    size_t len;
};

struct moonfire_ffmpeg_data {
    uint8_t *data;
    size_t len;
};

struct VideoParameters {
    int width;
    int height;
    AVRational sample_aspect_ratio;
    enum AVPixelFormat pix_fmt;
    AVRational time_base;
};

struct moonfire_ffmpeg_frame_stuff {
    uint8_t **data;
    int *linesizes;
    int format;
    int width;
    int height;
};

struct moonfire_ffmpeg_streams moonfire_ffmpeg_fctx_streams(AVFormatContext *ctx) {
    struct moonfire_ffmpeg_streams s = {ctx->streams, ctx->nb_streams};
    return s;
}

int moonfire_ffmpeg_fctx_open_write(AVFormatContext *ctx, const char *url) {
    return avio_open(&ctx->pb, url, AVIO_FLAG_WRITE);
}

void moonfire_ffmpeg_cctx_params(const AVCodecContext *ctx, struct VideoParameters *p) {
    p->width = ctx->width;
    p->height = ctx->height;
    p->sample_aspect_ratio = ctx->sample_aspect_ratio;
    p->pix_fmt = ctx->pix_fmt;
    p->time_base = ctx->time_base;
}

void moonfire_ffmpeg_cctx_set_params(AVCodecContext *ctx, const struct VideoParameters *p) {
    ctx->width = p->width;
    ctx->height = p->height;
    ctx->sample_aspect_ratio = p->sample_aspect_ratio;
    ctx->pix_fmt = p->pix_fmt;
    ctx->time_base = p->time_base;
}

AVPacket *moonfire_ffmpeg_packet_alloc(void) { return malloc(sizeof(AVPacket)); }
void moonfire_ffmpeg_packet_free(AVPacket *pkt) { free(pkt); }
bool moonfire_ffmpeg_packet_is_key(AVPacket *pkt) { return (pkt->flags & AV_PKT_FLAG_KEY) != 0; }
int64_t moonfire_ffmpeg_packet_pts(AVPacket *pkt) { return pkt->pts; }
void moonfire_ffmpeg_packet_set_dts(AVPacket *pkt, int64_t dts) { pkt->dts = dts; }
void moonfire_ffmpeg_packet_set_pts(AVPacket *pkt, int64_t pts) { pkt->pts = pts; }
void moonfire_ffmpeg_packet_set_duration(AVPacket *pkt, int dur) { pkt->duration = dur; }
int64_t moonfire_ffmpeg_packet_dts(AVPacket *pkt) { return pkt->dts; }
int moonfire_ffmpeg_packet_duration(AVPacket *pkt) { return pkt->duration; }
int moonfire_ffmpeg_packet_stream_index(AVPacket *pkt) { return pkt->stream_index; }
struct moonfire_ffmpeg_data moonfire_ffmpeg_packet_data(AVPacket *pkt) {
    struct moonfire_ffmpeg_data d = {pkt->data, pkt->size};
    return d;
}

const AVCodecContext *moonfire_ffmpeg_stream_codec(const AVStream *stream) { return stream->codec; }
AVCodecContext *moonfire_ffmpeg_stream_codec_mut(AVStream *stream) { return stream->codec; }
AVRational moonfire_ffmpeg_stream_time_base(AVStream *stream) { return stream->time_base; }

int moonfire_ffmpeg_cctx_codec_id(AVCodecContext *cctx) { return cctx->codec_id; }
int moonfire_ffmpeg_cctx_codec_type(AVCodecContext *cctx) { return cctx->codec_type; }
struct moonfire_ffmpeg_data moonfire_ffmpeg_cctx_extradata(AVCodecContext *cctx) {
    struct moonfire_ffmpeg_data d = {cctx->extradata, cctx->extradata_size};
    return d;
}
int moonfire_ffmpeg_cctx_height(AVCodecContext *cctx) { return cctx->height; }
int moonfire_ffmpeg_cctx_width(AVCodecContext *cctx) { return cctx->width; }
int moonfire_ffmpeg_cctx_pix_fmt(AVCodecContext *cctx) { return cctx->pix_fmt; }

int moonfire_ffmpeg_frame_height(AVFrame *frame) { return frame->height; }
int moonfire_ffmpeg_frame_width(AVFrame *frame) { return frame->width; }
int moonfire_ffmpeg_frame_pix_fmt(AVFrame *frame) { return frame->format; }
void moonfire_ffmpeg_frame_stuff(AVFrame *frame,
                                 struct moonfire_ffmpeg_frame_stuff* s) {
    s->data = frame->data;
    s->linesizes = frame->linesize;
    s->format = frame->format;
    s->width = frame->width;
    s->height = frame->height;
}

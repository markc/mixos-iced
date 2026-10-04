/* Tiny accessors for AVFormatContext fields that bindgen renders opaque
 * (it can't lay the struct out from the public headers). The C compiler sees
 * the real layout, so these are ABI-safe. */
#include <libavformat/avformat.h>

void compd_avfmt_set_pb(AVFormatContext *ctx, AVIOContext *pb) {
    ctx->pb = pb;
}

AVIOContext *compd_avfmt_get_pb(AVFormatContext *ctx) {
    return ctx->pb;
}

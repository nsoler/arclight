/*
 * Arclight native plugin ABI, version 1.
 *
 * A plugin is a shared library placed in one of Arclight's plugin
 * directories (see `arclight plugins`) and named libarclight_plugin_*.so or
 * *.arclight.so; other files are ignored. It exports one function,
 * `arclight_plugin_entry`, returning a static ArclightPlugin.
 *
 * Images are 32-bit float RGBA, linear light, premultiplied alpha, stored
 * row-major from the top-left: pixel (x, y) channel c is at
 * pixels[(y * width + x) * 4 + c].
 */
#ifndef ARCLIGHT_PLUGIN_H
#define ARCLIGHT_PLUGIN_H

#include <stddef.h>
#include <stdint.h>

#define ARCLIGHT_PLUGIN_ABI 1

#ifdef __cplusplus
extern "C" {
#endif

/*
 * `render` may be called from several threads at once and must not keep any
 * pointer it is given after it returns.
 */

/* An input image. `pixels` is NULL for an unconnected optional input. */
typedef struct {
    uint32_t width;
    uint32_t height;
    const float *pixels;
} ArclightImage;

/*
 * Allocates the output image, zero-filled, and returns its pixels for the
 * plugin to write. Call it exactly once per successful render. The buffer
 * belongs to the host and stays valid until render returns.
 */
typedef float *(*ArclightAlloc)(void *host, uint32_t width, uint32_t height);

typedef struct {
    /* Must be ARCLIGHT_PLUGIN_ABI. */
    uint32_t abi_version;

    /*
     * UTF-8 JSON array describing the plugin's ops:
     *
     * [{"name": "my_op", "label": "My Op", "category": "filter",
     *   "help": "...", "time_varying": false,
     *   "inputs": [{"label": "input"}, {"label": "mask", "optional": true}],
     *   "params": [{"name": "amount", "label": "Amount", "kind": "float",
     *               "min": 0, "max": 1, "default": 0.5}]}]
     *
     * category: source, color, channel, merge, shape, transform, filter, output.
     * kind: float, int (with min/max), vec2, vec3, color, bool, path,
     *       choice (with "choices": [...]).
     */
    const char *ops_json;

    /*
     * Render op `op`. `params_json` is a JSON object with every declared
     * param (defaults filled in, keyframes resolved at `frame`). Returns 0 on
     * success; otherwise writes a NUL-terminated message into `error`
     * (at most error_len bytes) and returns nonzero.
     */
    int32_t (*render)(const char *op, const char *params_json,
                      const ArclightImage *inputs, uint32_t n_inputs,
                      int64_t frame, void *host, ArclightAlloc alloc,
                      char *error, size_t error_len);
} ArclightPlugin;

/* The one symbol a plugin exports. */
const ArclightPlugin *arclight_plugin_entry(void);

#ifdef __cplusplus
}
#endif

#endif /* ARCLIGHT_PLUGIN_H */

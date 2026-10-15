/* OpenFadeR plugin ABI, version 1: effects (below) and track libraries (RkLibraryApi). Same layout as the
 * `openfader-plugin` crate (sdk/src/lib.rs), whose `Slot` documentation says what the input, the output and each
 * context field mean in every slot.
 *
 * An effects library exports `openfader_plugin_v1`, which returns the table of its effects. The host calls
 * `tick` on the audio thread: no allocation, locks, I/O or blocking there. One instance is never called from two
 * threads at once but may move between threads. */
#ifndef OPENFADER_PLUGIN_H
#define OPENFADER_PLUGIN_H

#include <stddef.h>
#include <stdint.h>

#define RK_ABI 1

/* Where an effect runs (RkEffect.slot). */
#define RK_SLOT_BEAT 0  /* Beat FX unit: channel or master in, replaced */
#define RK_SLOT_COLOR 1 /* COLOR FX knob: channel after the EQ in, replaced; knob -1..1 */
#define RK_SLOT_MIC 2   /* microphone in, replaced; knob = MIC FX parameter 0..1 */
#define RK_SLOT_SEND 3  /* sum of the channel sends in, the return out (no dry signal) */
#define RK_SLOT_LEVER 4 /* master in, replaced; knob = lever 0 (rest)..1 */
#define RK_SLOT_FADER 5 /* channel before its fader in, post-fader out; knob = fader 0..1 */

/* What an effect gets for one frame. */
typedef struct {
    float rate;   /* sample rate in Hz */
    float period; /* samples per selected beat fraction (tempo-slewed) */
    float beats;  /* the selected beat fraction, in beats (1/8 to 16) */
    float level;  /* LEVEL knob, 0 to 1 */
    float g;      /* engage ramp, 0 (off) to 1 (on) */
    float knob;   /* the slot's own control, see RK_SLOT_* */
} RkCtx;

typedef struct {
    /* NUL-terminated selector label: 1 to 16 characters of ASCII letters, digits, space, '-' or '_', unique in its slot. */
    const char *name;
    /* New instance for `rate` Hz, or NULL on failure. Required. */
    void *(*create)(uint32_t rate);
    /* Replaces frame[0] (left) and frame[1] (right) with the effect's output. Required. */
    void (*tick)(void *inst, float *frame, const RkCtx *ctx);
    /* Forget everything. May be NULL. */
    void (*reset)(void *inst);
    /* The effect was switched ON: restart phases or captures. May be NULL. */
    void (*engage)(void *inst);
    /* Frees the instance. Required. */
    void (*destroy)(void *inst);
    /* RK_SLOT_*; left out of an initialiser it is 0, RK_SLOT_BEAT. */
    uint32_t slot;
} RkEffect;

typedef struct {
    uint32_t abi; /* RK_ABI */
    uint32_t count;
    const RkEffect *effects;
} RkPluginInfo;

/* ---- Library plugins: a native library that reads a track library (a file or folder format) for the host and may write
 * the host's collection in that format. Same layout as `openfader_plugin::library` (sdk/src/library/mod.rs); the requests
 * and answers are JSON (the ops and fields are in docs/plugins.md, "Library plugins"). A library exports
 * `openfader_library_v1` returning a static RkLibraryApi. `call` takes one JSON request (an object with an "op") and returns
 * one JSON answer ({"error": "text"} on failure; never unwinds) in a buffer owned by the library until it is handed to
 * `free`. The host checks `abi` before anything else. */
#define RK_LIBRARY_ABI 1

typedef struct {
    uint8_t *ptr;
    size_t len;
} RkBuf;

typedef struct {
    uint32_t abi; /* RK_LIBRARY_ABI */
    RkBuf (*call)(const uint8_t *req, size_t len);
    void (*free)(RkBuf buf);
} RkLibraryApi;

#ifdef __cplusplus
extern "C" {
#endif
const RkPluginInfo *openfader_plugin_v1(void);
const RkLibraryApi *openfader_library_v1(void);
#ifdef __cplusplus
}
#endif

#endif

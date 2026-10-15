/* RINGMOD: ring modulator, a sine carrier at 110 Hz divided by the selected beat fraction (1/8 beat = 880 Hz,
 * 1 beat = 110 Hz, 16 beats = 6.9 Hz). The carrier restarts when the unit is switched ON. LEVEL is the wet amount.
 * C example of the OpenFadeR plugin ABI; build with build.sh. */
#include <math.h>
#include <stdlib.h>

#include "openfader_plugin.h"

typedef struct {
    double phase; /* carrier, 0..1 */
} RingMod;

static void *create(uint32_t rate) {
    (void)rate;
    return calloc(1, sizeof(RingMod));
}

static void tick(void *inst, float *frame, const RkCtx *c) {
    RingMod *r = inst;
    float carrier = sinf((float)(r->phase * 6.283185307179586));
    r->phase += 110.0 / c->beats / c->rate;
    r->phase -= floor(r->phase);
    float wet[2] = {frame[0] * carrier, frame[1] * carrier};
    float m = c->level * c->g;
    for (int k = 0; k < 2; k++)
        frame[k] += (wet[k] - frame[k]) * m;
}

static void engage(void *inst) {
    ((RingMod *)inst)->phase = 0.0;
}

static void destroy(void *inst) {
    free(inst);
}

static const RkEffect EFFECTS[] = {
    {"RINGMOD", create, tick, engage, engage, destroy},
};
static const RkPluginInfo INFO = {RK_ABI, 1, EFFECTS};

const RkPluginInfo *openfader_plugin_v1(void) {
    return &INFO;
}

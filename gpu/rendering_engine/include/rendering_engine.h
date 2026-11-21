#pragma once

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#define BROWSER_NOEXCEPT noexcept
#else
#define BROWSER_NOEXCEPT
#endif

enum { BROWSER_RENDERER_ABI = 3 };

typedef struct BrowserRenderer BrowserRenderer;
typedef struct BrowserTextFactory BrowserTextFactory;
typedef struct BrowserTextLayout BrowserTextLayout;

typedef struct BrowserRect {
    float x, y, width, height;
} BrowserRect;

typedef struct BrowserColor {
    float red, green, blue, alpha;
} BrowserColor;

typedef struct BrowserTextRange {
    uint32_t start, length;
} BrowserTextRange;

typedef struct BrowserTextRun {
    BrowserTextRange range;
    BrowserColor color;
    float size;
    uint32_t weight, family, italic, underline, strikethrough;
} BrowserTextRun;

typedef struct BrowserHitRegion {
    BrowserRect rect;
    uint32_t range;
} BrowserHitRegion;

typedef struct BrowserTextOptions {
    float size, width;
    uint32_t weight, alignment, wrap, trim;
} BrowserTextOptions;

typedef struct BrowserImageView {
    const uint8_t* bgra;
    uint32_t width, height, stride;
} BrowserImageView;

enum BrowserCommandKind {
    BROWSER_FILL = 1,
    BROWSER_TEXT = 2,
    BROWSER_IMAGE = 3,
    BROWSER_PUSH_CLIP = 4,
    BROWSER_POP_CLIP = 5,
};

typedef struct BrowserCommand {
    BrowserRect rect;
    BrowserColor color;
    float radius;
    uint32_t kind, resource;
} BrowserCommand;

typedef struct BrowserSceneView {
    uint64_t key;
    const BrowserCommand* commands;
    const BrowserTextLayout* const* text;
    const BrowserImageView* images;
    uint32_t command_count, text_count, image_count;
    BrowserColor background;
} BrowserSceneView;

typedef struct BrowserLayer {
    const BrowserSceneView* scene;
    BrowserRect clip;
    float offset_x, offset_y, opacity;
    uint32_t fill_background;
} BrowserLayer;

typedef struct BrowserMotionFrame {
    BrowserRect initial_screen, target_screen;
    uint64_t source_frame;
    float screen_x, screen_y, scale;
    uint32_t transition, duration_ms;
} BrowserMotionFrame;

typedef struct BrowserFrame {
    const BrowserLayer* layers;
    const BrowserMotionFrame* motion;
    BrowserColor background;
    uint32_t layer_count;
} BrowserFrame;

typedef struct BrowserFrameResult {
    uint32_t presented, motion_complete;
} BrowserFrameResult;

uint32_t browser_renderer_abi(void) BROWSER_NOEXCEPT;
int32_t browser_renderer_create(void* window, uint32_t width, uint32_t height, uint32_t dpi,
    uint32_t warp, BrowserRenderer** result) BROWSER_NOEXCEPT;
uint32_t browser_renderer_uses_warp(const BrowserRenderer* renderer) BROWSER_NOEXCEPT;
int32_t browser_renderer_resize(BrowserRenderer* renderer, uint32_t width, uint32_t height,
    uint32_t dpi) BROWSER_NOEXCEPT;
int32_t browser_renderer_draw(BrowserRenderer* renderer, const BrowserFrame* frame,
    BrowserFrameResult* result) BROWSER_NOEXCEPT;
int32_t browser_renderer_read_pixel(BrowserRenderer* renderer, uint32_t x, uint32_t y,
    uint32_t* bgra) BROWSER_NOEXCEPT;
void browser_renderer_destroy(BrowserRenderer* renderer) BROWSER_NOEXCEPT;

int32_t browser_text_factory_create(BrowserTextFactory** result) BROWSER_NOEXCEPT;
int32_t browser_text_factory_register_font(BrowserTextFactory* factory,
    const uint8_t* data, uint32_t length, const uint16_t* family_name,
    uint32_t* font_id) BROWSER_NOEXCEPT;
void browser_text_factory_destroy(BrowserTextFactory* factory) BROWSER_NOEXCEPT;
int32_t browser_text_layout_create(BrowserTextFactory* factory,
    const uint16_t* text, uint32_t length, const BrowserTextOptions* options,
    const BrowserTextRange* ranges, uint32_t range_count,
    const BrowserTextRun* runs, uint32_t run_count, BrowserTextLayout** result,
    float* width, float* height) BROWSER_NOEXCEPT;
const BrowserHitRegion* browser_text_layout_hits(const BrowserTextLayout* layout,
    uint32_t* count) BROWSER_NOEXCEPT;
void browser_text_layout_destroy(BrowserTextLayout* layout) BROWSER_NOEXCEPT;

#ifdef __cplusplus
}
#endif

#undef BROWSER_NOEXCEPT

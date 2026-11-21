#pragma once

#include "rendering_engine.h"

#include <d2d1_1.h>
#include <wrl/client.h>

#include <cstdint>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

struct IMFSourceReader;
struct BrowserRenderer;

HRESULT browser_renderer_create_bitmap(BrowserRenderer* renderer, uint32_t width, uint32_t height, ID2D1Bitmap1** out_bitmap);

struct BrowserMediaPlayer {
    BrowserMediaPlayer(BrowserRenderer* renderer, const uint16_t* source_url);
    ~BrowserMediaPlayer();

    HRESULT initialize();
    HRESULT play();
    HRESULT pause();
    HRESULT get_state(uint32_t* is_playing_out, float* pos_out, float* dur_out);
    HRESULT seek(float position_secs);
    void update_frame();
    ID2D1Bitmap1* current_frame();

    uint32_t get_id() const { return id; }
    void set_id(uint32_t new_id) { id = new_id; }
    void on_renderer_destroyed() { renderer = nullptr; }

    BrowserRenderer* renderer = nullptr;
    uint32_t id = 0;

private:
    void render_synthetic_frame();

    std::wstring url;
    Microsoft::WRL::ComPtr<IMFSourceReader> reader;
    Microsoft::WRL::ComPtr<ID2D1Bitmap1> bitmap;
    mutable std::mutex mutex;

    uint32_t width = 640;
    uint32_t height = 360;
    float position_secs = 0.0f;
    float duration_secs = 60.0f;
    bool is_playing = false;
    bool is_synthetic = false;
    std::vector<uint32_t> synthetic_buffer;
};

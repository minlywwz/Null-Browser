#include "rendering_engine.h"
#include "media_player.h"
#include "motion_engine/motion_engine.h"
#include "text.h"

#include <d2d1_1.h>
#include <d2d1_1helper.h>
#include <d3d11.h>
#include <dxgi1_3.h>

#include <algorithm>
#include <cmath>
#include <deque>
#include <limits>
#include <memory>
#include <new>

using Microsoft::WRL::ComPtr;

static_assert(sizeof(BrowserRect) == 16 && sizeof(BrowserColor) == 16);
static_assert(sizeof(BrowserCommand) == 44 && sizeof(BrowserMotionFrame) == 64);
#ifdef _WIN64
static_assert(sizeof(BrowserSceneView) == 64 && sizeof(BrowserLayer) == 40);
static_assert(sizeof(BrowserFrame) == 40 && sizeof(BrowserImageView) == 24);
#endif

namespace {
constexpr auto pixel_format = DXGI_FORMAT_B8G8R8A8_UNORM;

D2D1_RECT_F rectangle(const BrowserRect& rect) {
    return D2D1::RectF(rect.x, rect.y, rect.x + rect.width, rect.y + rect.height);
}

D2D1_COLOR_F color(const BrowserColor& value) {
    return D2D1::ColorF(value.red, value.green, value.blue, value.alpha);
}

bool intersects(const BrowserRect& rect, const BrowserLayer& layer) {
    const float x = rect.x + layer.offset_x;
    const float y = rect.y + layer.offset_y;
    return x < layer.clip.x + layer.clip.width && x + rect.width > layer.clip.x
        && y < layer.clip.y + layer.clip.height && y + rect.height > layer.clip.y;
}
}

struct BrowserRenderer {
    ~BrowserRenderer() {
        if (frame_latency) {
            CloseHandle(frame_latency);
        }
        for (auto& p : media_players) {
            if (p) {
                p->on_renderer_destroyed();
            }
        }
        media_players.clear();
    }

    HRESULT initialize(HWND window, uint32_t width, uint32_t height, uint32_t dpi, bool force_warp);
    HRESULT resize(uint32_t width, uint32_t height, uint32_t dpi);
    HRESULT draw(const BrowserFrame& frame, BrowserFrameResult& result);
    HRESULT read_pixel(uint32_t x, uint32_t y, uint32_t& bgra);
    HRESULT create_media_player(const uint16_t* source_url, BrowserMediaPlayer** result);
    void remove_media_player(BrowserMediaPlayer* player);
    HRESULT create_bitmap(uint32_t width, uint32_t height, ID2D1Bitmap1** out_bitmap);
    bool warp = false;
    std::vector<std::unique_ptr<BrowserMediaPlayer>> media_players;

private:
    HRESULT create_output(uint32_t width, uint32_t height, uint32_t dpi);
    HRESULT create_swapchain(HWND window, uint32_t width, uint32_t height);
    HRESULT update_motion_texture(uint64_t frame);
    void draw_motion(const BrowserMotionFrame& frame);
    void draw_layer(const BrowserLayer& layer);
    ID2D1Bitmap1* image(const BrowserSceneView& scene, uint32_t index);

    struct ImageCache {
        uint64_t key;
        std::vector<ComPtr<ID2D1Bitmap1>> images;
    };

    ComPtr<ID3D11Device> d3d;
    ComPtr<ID3D11DeviceContext> d3d_context;
    ComPtr<IDXGISwapChain2> swapchain;
    ComPtr<ID2D1DeviceContext> d2d;
    ComPtr<ID2D1Bitmap1> output;
    ComPtr<ID2D1SolidColorBrush> brush;
    ComPtr<ID3D11Texture2D> offscreen;
    ComPtr<ID2D1Bitmap1> motion_texture;
    std::deque<ImageCache> images;
    MotionEngine motion_engine;
    HANDLE frame_latency = nullptr;
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t dpi = 96;
    uint64_t last_motion_frame = std::numeric_limits<uint64_t>::max();
};

HRESULT BrowserRenderer::initialize(HWND window, uint32_t w, uint32_t h, uint32_t new_dpi, bool force_warp) {
    auto create_device = [&](D3D_DRIVER_TYPE type) {
        return D3D11CreateDevice(nullptr, type, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            nullptr, 0, D3D11_SDK_VERSION, &d3d, nullptr, &d3d_context);
    };
    HRESULT hr = force_warp ? E_FAIL : create_device(D3D_DRIVER_TYPE_HARDWARE);
    if (FAILED(hr)) {
        warp = true;
        hr = create_device(D3D_DRIVER_TYPE_WARP);
    }
    if (FAILED(hr)) {
        return hr;
    }
    ComPtr<IDXGIDevice> dxgi;
    if (FAILED(hr = d3d.As(&dxgi))) {
        return hr;
    }
    ComPtr<ID2D1Device> device;
    if (FAILED(hr = D2D1CreateDevice(dxgi.Get(), nullptr, &device))) {
        return hr;
    }
    if (FAILED(hr = device->CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE, &d2d))) {
        return hr;
    }
    d2d->SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
    if (FAILED(hr = d2d->CreateSolidColorBrush(D2D1::ColorF(D2D1::ColorF::White), &brush))) {
        return hr;
    }
    if (window && FAILED(hr = create_swapchain(window, w, h))) {
        return hr;
    }
    return create_output(w, h, new_dpi);
}

HRESULT BrowserRenderer::create_swapchain(HWND window, uint32_t w, uint32_t h) {
    ComPtr<IDXGIDevice> device;
    HRESULT hr = d3d.As(&device);
    if (FAILED(hr)) {
        return hr;
    }
    ComPtr<IDXGIAdapter> adapter;
    if (FAILED(hr = device->GetAdapter(&adapter))) {
        return hr;
    }
    ComPtr<IDXGIFactory2> factory;
    if (FAILED(hr = adapter->GetParent(IID_PPV_ARGS(&factory)))) {
        return hr;
    }
    DXGI_SWAP_CHAIN_DESC1 desc{};
    desc.Width = w;
    desc.Height = h;
    desc.Format = pixel_format;
    desc.SampleDesc.Count = 1;
    desc.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    desc.BufferCount = 2;
    desc.Scaling = DXGI_SCALING_STRETCH;
    desc.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
    desc.AlphaMode = DXGI_ALPHA_MODE_IGNORE;
    desc.Flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT;
    ComPtr<IDXGISwapChain1> chain;
    if (FAILED(hr = factory->CreateSwapChainForHwnd(d3d.Get(), window, &desc, nullptr, nullptr, &chain))) {
        return hr;
    }
    factory->MakeWindowAssociation(window, DXGI_MWA_NO_ALT_ENTER);
    if (FAILED(hr = chain.As(&swapchain))) {
        return hr;
    }
    if (FAILED(hr = swapchain->SetMaximumFrameLatency(1))) {
        return hr;
    }
    frame_latency = swapchain->GetFrameLatencyWaitableObject();
    return frame_latency ? S_OK : E_FAIL;
}

HRESULT BrowserRenderer::create_output(uint32_t w, uint32_t h, uint32_t new_dpi) {
    ComPtr<IDXGISurface> surface;
    HRESULT hr;
    if (swapchain) {
        hr = swapchain->GetBuffer(0, IID_PPV_ARGS(&surface));
    } else {
        D3D11_TEXTURE2D_DESC desc{};
        desc.Width = w;
        desc.Height = h;
        desc.MipLevels = 1;
        desc.ArraySize = 1;
        desc.Format = pixel_format;
        desc.SampleDesc.Count = 1;
        desc.BindFlags = D3D11_BIND_RENDER_TARGET;
        hr = d3d->CreateTexture2D(&desc, nullptr, &offscreen);
        if (SUCCEEDED(hr)) {
            hr = offscreen.As(&surface);
        }
    }
    if (FAILED(hr)) {
        return hr;
    }
    const auto dpi_value = static_cast<float>(new_dpi);
    const auto props = D2D1::BitmapProperties1(
        D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
        D2D1::PixelFormat(pixel_format, D2D1_ALPHA_MODE_IGNORE), dpi_value, dpi_value);
    if (FAILED(hr = d2d->CreateBitmapFromDxgiSurface(surface.Get(), &props, &output))) {
        return hr;
    }
    d2d->SetTarget(output.Get());
    d2d->SetDpi(dpi_value, dpi_value);
    width = w;
    height = h;
    dpi = new_dpi;
    return S_OK;
}

HRESULT BrowserRenderer::resize(uint32_t w, uint32_t h, uint32_t new_dpi) {
    if (output && w == width && h == height && new_dpi == dpi) {
        return S_OK;
    }
    d2d->SetTarget(nullptr);
    output.Reset();
    offscreen.Reset();
    if (swapchain && (w != width || h != height)) {
        d3d_context->ClearState();
        const HRESULT hr = swapchain->ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN,
            DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT);
        if (FAILED(hr)) {
            return hr;
        }
    }
    return create_output(w, h, new_dpi);
}

ID2D1Bitmap1* BrowserRenderer::image(const BrowserSceneView& scene, uint32_t index) {
    if (index >= scene.image_count) {
        return nullptr;
    }
    auto entry = std::find_if(images.begin(), images.end(), [&](const ImageCache& cache) {
        return cache.key == scene.key;
    });
    if (entry == images.end()) {
        if (images.size() == 3) {
            images.pop_front();
        }
        images.push_back({scene.key, std::vector<ComPtr<ID2D1Bitmap1>>(scene.image_count)});
        entry = std::prev(images.end());
    }
    auto& bitmap = entry->images[index];
    if (!bitmap) {
        const auto& view = scene.images[index];
        const auto props = D2D1::BitmapProperties1(D2D1_BITMAP_OPTIONS_NONE,
            D2D1::PixelFormat(pixel_format, D2D1_ALPHA_MODE_PREMULTIPLIED), 96, 96);
        d2d->CreateBitmap(D2D1::SizeU(view.width, view.height), view.bgra, view.stride, &props, &bitmap);
    }
    return bitmap.Get();
}

void BrowserRenderer::draw_layer(const BrowserLayer& layer) {
    if (!layer.scene || layer.clip.width <= 0 || layer.clip.height <= 0 || layer.opacity <= 0) {
        return;
    }
    const auto& scene = *layer.scene;
    const auto clip = rectangle(layer.clip);
    d2d->PushAxisAlignedClip(clip, D2D1_ANTIALIAS_MODE_ALIASED);
    const bool translucent = layer.opacity < 1;
    if (translucent) {
        d2d->PushLayer(D2D1::LayerParameters1(clip, nullptr, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            D2D1::Matrix3x2F::Identity(), layer.opacity), nullptr);
    }
    if (layer.fill_background) {
        brush->SetColor(color(scene.background));
        d2d->FillRectangle(clip, brush.Get());
    }
    d2d->SetTransform(D2D1::Matrix3x2F::Translation(layer.offset_x, layer.offset_y));
    uint32_t clips = 0;
    for (uint32_t index = 0; index < scene.command_count; ++index) {
        const auto& command = scene.commands[index];
        if (command.kind == BROWSER_PUSH_CLIP) {
            d2d->PushAxisAlignedClip(rectangle(command.rect), D2D1_ANTIALIAS_MODE_ALIASED);
            ++clips;
            continue;
        }
        if (command.kind == BROWSER_POP_CLIP) {
            if (clips) {
                d2d->PopAxisAlignedClip();
                --clips;
            }
            continue;
        }
        if (!intersects(command.rect, layer)) {
            continue;
        }
        const auto bounds = rectangle(command.rect);
        switch (command.kind) {
        case BROWSER_FILL: {
            brush->SetColor(color(command.color));
            const auto radius = std::min(command.radius, std::min(command.rect.width, command.rect.height) * 0.5f);
            if (radius > 0) {
                d2d->FillRoundedRectangle(D2D1::RoundedRect(bounds, radius, radius), brush.Get());
            } else {
                d2d->FillRectangle(bounds, brush.Get());
            }
            break;
        }
        case BROWSER_TEXT:
            if (command.resource < scene.text_count && command.rect.width > 0 && command.rect.height > 0) {
                const auto& layout = *scene.text[command.resource];
                const std::scoped_lock lock(layout.drawing);
                const auto base = command.color;
                const auto uses_effect = [base](const BrowserTextRun& run) {
                    const auto ink = run.color;
                    return ink.red != base.red || ink.green != base.green
                        || ink.blue != base.blue || ink.alpha != base.alpha;
                };
                for (const auto& run : layout.runs) {
                    if (uses_effect(run)) {
                        ComPtr<ID2D1SolidColorBrush> effect;
                        if (SUCCEEDED(d2d->CreateSolidColorBrush(color(run.color), &effect))) {
                            layout.value->SetDrawingEffect(effect.Get(), {run.range.start, run.range.length});
                        }
                    }
                }
                brush->SetColor(color(command.color));
                d2d->PushAxisAlignedClip(bounds, D2D1_ANTIALIAS_MODE_ALIASED);
                d2d->DrawTextLayout(D2D1::Point2F(command.rect.x, command.rect.y),
                    layout.value.Get(), brush.Get(), D2D1_DRAW_TEXT_OPTIONS_CLIP);
                d2d->PopAxisAlignedClip();
                for (const auto& run : layout.runs) {
                    if (uses_effect(run)) {
                        layout.value->SetDrawingEffect(nullptr, {run.range.start, run.range.length});
                    }
                }
            }
            break;
        case BROWSER_IMAGE:
            if (auto* bitmap = image(scene, command.resource)) {
                d2d->DrawBitmap(bitmap, &bounds, 1.0f, D2D1_INTERPOLATION_MODE_LINEAR);
            }
            break;
        case BROWSER_VIDEO: {
            BrowserMediaPlayer* player = nullptr;
            if (command.resource < media_players.size()) {
                player = media_players[command.resource].get();
            }
            ID2D1Bitmap1* frame_bitmap = nullptr;
            if (player) {
                player->update_frame();
                frame_bitmap = player->current_frame();
            }
            if (frame_bitmap) {
                d2d->DrawBitmap(frame_bitmap, &bounds, 1.0f, D2D1_INTERPOLATION_MODE_LINEAR);
            } else {
                brush->SetColor(D2D1::ColorF(0x101418));
                d2d->FillRectangle(bounds, brush.Get());

                brush->SetColor(D2D1::ColorF(0x283038));
                d2d->DrawRectangle(bounds, brush.Get(), 1.0f);

                const float cx = bounds.left + (bounds.right - bounds.left) * 0.5f;
                const float cy = bounds.top + (bounds.bottom - bounds.top) * 0.5f;
                const float size = std::min(command.rect.width, command.rect.height) * 0.2f;
                if (size > 2.0f) {
                    brush->SetColor(D2D1::ColorF(0x000000, 0.6f));
                    d2d->FillEllipse(D2D1::Ellipse(D2D1::Point2F(cx, cy), size * 1.2f, size * 1.2f), brush.Get());

                    ComPtr<ID2D1Factory> d2d_factory;
                    d2d->GetFactory(&d2d_factory);
                    ComPtr<ID2D1PathGeometry> path;
                    if (d2d_factory && SUCCEEDED(d2d_factory->CreatePathGeometry(&path))) {
                        ComPtr<ID2D1GeometrySink> sink;
                        if (SUCCEEDED(path->Open(&sink))) {
                            sink->BeginFigure(D2D1::Point2F(cx - size * 0.5f, cy - size * 0.7f), D2D1_FIGURE_BEGIN_FILLED);
                            sink->AddLine(D2D1::Point2F(cx + size * 0.8f, cy));
                            sink->AddLine(D2D1::Point2F(cx - size * 0.5f, cy + size * 0.7f));
                            sink->EndFigure(D2D1_FIGURE_END_CLOSED);
                            sink->Close();

                            brush->SetColor(D2D1::ColorF(D2D1::ColorF::White, 0.9f));
                            d2d->FillGeometry(path.Get(), brush.Get());
                        }
                    }
                }
            }
            break;
        }
        }
    }
    while (clips > 0) {
        d2d->PopAxisAlignedClip();
        --clips;
    }
    d2d->SetTransform(D2D1::Matrix3x2F::Identity());
    if (translucent) {
        d2d->PopLayer();
    }
    d2d->PopAxisAlignedClip();
}

HRESULT BrowserRenderer::update_motion_texture(uint64_t frame) {
    if (!motion_texture) {
        const auto props = D2D1::BitmapProperties1(D2D1_BITMAP_OPTIONS_TARGET,
            D2D1::PixelFormat(pixel_format, D2D1_ALPHA_MODE_PREMULTIPLIED), 96, 96);
        const HRESULT hr = d2d->CreateBitmap(D2D1::SizeU(640, 360), nullptr, 0, &props, &motion_texture);
        if (FAILED(hr)) {
            return hr;
        }
    } else if (frame == last_motion_frame) {
        return S_OK;
    }
    d2d->SetTarget(motion_texture.Get());
    d2d->SetDpi(96, 96);
    d2d->BeginDraw();
    d2d->Clear(D2D1::ColorF(0x0e2232));
    for (uint32_t i = 0; i < 12; ++i) {
        const float y = static_cast<float>(i * 30);
        const float t = static_cast<float>(i) / 11.0f;
        brush->SetColor(D2D1::ColorF(0.055f + 0.12f * t, 0.13f + 0.06f * t, 0.22f + 0.14f * t));
        d2d->FillRectangle(D2D1::RectF(0, y, 640, y + 30), brush.Get());
    }
    for (uint32_t i = 0; i < 28; ++i) {
        const float x = 20.0f + static_cast<float>(i) * 22.0f;
        const float phase = static_cast<float>(frame % 720) * 0.035f + static_cast<float>(i) * 0.35f;
        const float top = 183.0f + 61.0f * std::sin(phase);
        brush->SetColor(D2D1::ColorF(0.22f, 0.85f, 0.73f, 0.28f + static_cast<float>(i) / 44.0f));
        d2d->FillRoundedRectangle(D2D1::RoundedRect(D2D1::RectF(x, top, x + 12, 300), 5, 5), brush.Get());
    }
    brush->SetColor(D2D1::ColorF(0xf5fffa));
    const float x = 40.0f + static_cast<float>(frame % 560);
    d2d->FillEllipse(D2D1::Ellipse(D2D1::Point2F(x, 87.0f), 7.0f, 7.0f), brush.Get());
    for (uint32_t bit = 0; bit < 12; ++bit) {
        brush->SetColor(D2D1::ColorF((frame & (uint64_t{1} << bit)) ? 0x89ffe6 : 0x143845));
        const float left = 20.0f + static_cast<float>(bit) * 16.0f;
        d2d->FillRectangle(D2D1::RectF(left, 20, left + 11, 31), brush.Get());
    }
    const HRESULT hr = d2d->EndDraw();
    d2d->SetTarget(output.Get());
    d2d->SetDpi(static_cast<float>(dpi), static_cast<float>(dpi));
    if (SUCCEEDED(hr)) {
        last_motion_frame = frame;
    }
    return hr;
}

void BrowserRenderer::draw_motion(const BrowserMotionFrame& frame) {
    const auto rect = motion_engine.update(frame);
    if (rect.width <= 0 || rect.height <= 0) {
        return;
    }
    const auto background = rectangle(rect);
    brush->SetColor(D2D1::ColorF(0x040810));
    d2d->FillRectangle(background, brush.Get());
    const float scale = std::min(rect.width / 640.0f, rect.height / 360.0f);
    const float w = scale * 640.0f;
    const float h = scale * 360.0f;
    const auto target = rectangle({rect.x + (rect.width - w) / 2.0f, rect.y + (rect.height - h) / 2.0f, w, h});
    d2d->DrawBitmap(motion_texture.Get(), &target, 1.0f, D2D1_INTERPOLATION_MODE_LINEAR);
}

HRESULT BrowserRenderer::draw(const BrowserFrame& frame, BrowserFrameResult& result) {
    if (!output) {
        return E_FAIL;
    }
    if (frame_latency) {
        const DWORD wait = WaitForSingleObject(frame_latency, 80);
        if (wait == WAIT_TIMEOUT) {
            return S_FALSE;
        }
        if (wait != WAIT_OBJECT_0) {
            return HRESULT_FROM_WIN32(GetLastError());
        }
    }
    if (frame.motion) {
        const HRESULT hr = update_motion_texture(frame.motion->source_frame);
        if (FAILED(hr)) {
            return hr;
        }
    } else {
        motion_engine.reset();
        motion_texture.Reset();
        last_motion_frame = std::numeric_limits<uint64_t>::max();
    }
    d2d->BeginDraw();
    d2d->SetTransform(D2D1::Matrix3x2F::Identity());
    d2d->Clear(color(frame.background));
    if (frame.motion) {
        draw_motion(*frame.motion);
    }
    for (uint32_t i = 0; i < frame.layer_count; ++i) {
        draw_layer(frame.layers[i]);
    }
    HRESULT hr = d2d->EndDraw();
    if (FAILED(hr)) {
        return hr;
    }
    if (swapchain) {
        hr = swapchain->Present(1, 0);
        if (hr != S_OK) {
            return hr;
        }
    }
    result.presented = 1;
    result.motion_complete = frame.motion && motion_engine.complete();
    return S_OK;
}

HRESULT BrowserRenderer::read_pixel(uint32_t x, uint32_t y, uint32_t& bgra) {
    if (!offscreen || x >= width || y >= height) {
        return E_INVALIDARG;
    }
    D3D11_TEXTURE2D_DESC desc{};
    offscreen->GetDesc(&desc);
    desc.Width = desc.Height = 1;
    desc.Usage = D3D11_USAGE_STAGING;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    desc.BindFlags = 0;
    ComPtr<ID3D11Texture2D> staging;
    HRESULT hr = d3d->CreateTexture2D(&desc, nullptr, &staging);
    if (FAILED(hr)) {
        return hr;
    }
    const D3D11_BOX region{x, y, 0, x + 1, y + 1, 1};
    d3d_context->CopySubresourceRegion(staging.Get(), 0, 0, 0, 0, offscreen.Get(), 0, &region);
    D3D11_MAPPED_SUBRESOURCE mapping{};
    hr = d3d_context->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapping);
    if (SUCCEEDED(hr)) {
        bgra = *static_cast<const uint32_t*>(mapping.pData);
        d3d_context->Unmap(staging.Get(), 0);
    }
    return hr;
}

uint32_t browser_renderer_abi() noexcept {
    return BROWSER_RENDERER_ABI;
}

int32_t browser_renderer_create(void* window, uint32_t width, uint32_t height, uint32_t dpi,
    uint32_t warp, BrowserRenderer** result) noexcept {
    if (!result || !width || !height || !dpi || width > 16384 || height > 16384) {
        return E_INVALIDARG;
    }
    *result = nullptr;
    try {
        auto renderer = std::make_unique<BrowserRenderer>();
        const HRESULT hr = renderer->initialize(static_cast<HWND>(window), width, height, dpi, warp != 0);
        if (SUCCEEDED(hr)) {
            *result = renderer.release();
        }
        return hr;
    } catch (const std::bad_alloc&) {
        return E_OUTOFMEMORY;
    } catch (...) {
        return E_FAIL;
    }
}

uint32_t browser_renderer_uses_warp(const BrowserRenderer* renderer) noexcept {
    return renderer && renderer->warp;
}

int32_t browser_renderer_resize(BrowserRenderer* renderer, uint32_t width, uint32_t height, uint32_t dpi) noexcept {
    if (!renderer || !width || !height || !dpi || width > 16384 || height > 16384) {
        return E_INVALIDARG;
    }
    return renderer->resize(width, height, dpi);
}

int32_t browser_renderer_draw(BrowserRenderer* renderer, const BrowserFrame* frame, BrowserFrameResult* result) noexcept {
    if (!renderer || !frame || !result || frame->layer_count > 4 || (frame->layer_count && !frame->layers)) {
        return E_INVALIDARG;
    }
    *result = {};
    try {
        return renderer->draw(*frame, *result);
    } catch (const std::bad_alloc&) {
        return E_OUTOFMEMORY;
    } catch (...) {
        return E_FAIL;
    }
}

int32_t browser_renderer_read_pixel(BrowserRenderer* renderer, uint32_t x, uint32_t y, uint32_t* bgra) noexcept {
    return renderer && bgra ? renderer->read_pixel(x, y, *bgra) : E_INVALIDARG;
}

void browser_renderer_destroy(BrowserRenderer* renderer) noexcept {
    delete renderer;
}

HRESULT BrowserRenderer::create_media_player(const uint16_t* source_url, BrowserMediaPlayer** result) {
    auto player = std::make_unique<BrowserMediaPlayer>(this, source_url);
    HRESULT hr = player->initialize();
    if (FAILED(hr)) {
        return hr;
    }
    player->set_id(static_cast<uint32_t>(media_players.size()));
    *result = player.get();
    media_players.push_back(std::move(player));
    return S_OK;
}

void BrowserRenderer::remove_media_player(BrowserMediaPlayer* player) {
    for (auto& p : media_players) {
        if (p.get() == player) {
            p.reset();
            break;
        }
    }
}

HRESULT BrowserRenderer::create_bitmap(uint32_t w, uint32_t h, ID2D1Bitmap1** out_bitmap) {
    if (!d2d || !out_bitmap) {
        return E_FAIL;
    }
    const auto props = D2D1::BitmapProperties1(
        D2D1_BITMAP_OPTIONS_NONE,
        D2D1::PixelFormat(pixel_format, D2D1_ALPHA_MODE_IGNORE),
        96.0f, 96.0f
    );
    return d2d->CreateBitmap(D2D1::SizeU(w, h), nullptr, 0, &props, out_bitmap);
}

HRESULT browser_renderer_create_bitmap(BrowserRenderer* renderer, uint32_t width, uint32_t height, ID2D1Bitmap1** out_bitmap) {
    if (!renderer) return E_POINTER;
    return renderer->create_bitmap(width, height, out_bitmap);
}

int32_t browser_media_player_create(BrowserRenderer* renderer,
    const uint16_t* source_url, BrowserMediaPlayer** result) noexcept {
    if (!renderer || !result) {
        return E_INVALIDARG;
    }
    *result = nullptr;
    try {
        return renderer->create_media_player(source_url, result);
    } catch (const std::bad_alloc&) {
        return E_OUTOFMEMORY;
    } catch (...) {
        return E_FAIL;
    }
}

void browser_media_player_destroy(BrowserMediaPlayer* player) noexcept {
    if (!player) return;
    if (player->renderer) {
        player->renderer->remove_media_player(player);
    }
}

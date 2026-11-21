#include "media_player.h"

#pragma comment(lib, "ole32.lib")
#pragma comment(lib, "mfplat.lib")
#pragma comment(lib, "mfuuid.lib")
#pragma comment(lib, "mfreadwrite.lib")

#include <mfapi.h>
#include <mfidl.h>
#include <mfreadwrite.h>
#include <mferror.h>
#include <propvarutil.h>

#include <algorithm>
#include <atomic>
#include <cmath>
#include <unordered_set>

using Microsoft::WRL::ComPtr;

namespace {
std::atomic<int> g_mf_ref_count{0};
std::mutex g_players_mutex;
std::unordered_set<BrowserMediaPlayer*> g_active_players;

void ensure_mf_started() {
    if (g_mf_ref_count.fetch_add(1) == 0) {
        CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_FULL);
    }
}

void release_mf() {
    if (g_mf_ref_count.fetch_sub(1) == 1) {
        MFShutdown();
        CoUninitialize();
    }
}
}

BrowserMediaPlayer::BrowserMediaPlayer(BrowserRenderer* r, const uint16_t* source_url)
    : renderer(r) {
    if (source_url) {
        url = reinterpret_cast<const wchar_t*>(source_url);
    }
    std::scoped_lock lock(g_players_mutex);
    g_active_players.insert(this);
}

BrowserMediaPlayer::~BrowserMediaPlayer() {
    {
        std::scoped_lock lock(g_players_mutex);
        g_active_players.erase(this);
    }
    bitmap.Reset();
    reader.Reset();
    release_mf();
}

HRESULT BrowserMediaPlayer::initialize() {
    std::scoped_lock lock(mutex);
    ensure_mf_started();

    if (url.empty()) {
        is_synthetic = true;
        render_synthetic_frame();
        return S_OK;
    }

    ComPtr<IMFAttributes> attributes;
    HRESULT hr = MFCreateAttributes(&attributes, 2);
    if (SUCCEEDED(hr)) {
        attributes->SetUINT32(MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, TRUE);
        attributes->SetUINT32(MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, TRUE);
    }

    hr = MFCreateSourceReaderFromURL(url.c_str(), attributes.Get(), &reader);
    if (FAILED(hr) || !reader) {
        is_synthetic = true;
        render_synthetic_frame();
        return S_OK;
    }

    hr = reader->SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS, FALSE);
    hr = reader->SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM, TRUE);

    ComPtr<IMFMediaType> partial_type;
    hr = MFCreateMediaType(&partial_type);
    if (SUCCEEDED(hr)) {
        partial_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
        partial_type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_RGB32);
        hr = reader->SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM, nullptr, partial_type.Get());
    }

    if (FAILED(hr)) {
        reader.Reset();
        is_synthetic = true;
        render_synthetic_frame();
        return S_OK;
    }

    ComPtr<IMFMediaType> current_type;
    hr = reader->GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM, &current_type);
    if (SUCCEEDED(hr)) {
        UINT32 w = 0, h = 0;
        if (SUCCEEDED(MFGetAttributeSize(current_type.Get(), MF_MT_FRAME_SIZE, &w, &h)) && w > 0 && h > 0) {
            width = w;
            height = h;
        }
    }

    PROPVARIANT var;
    PropVariantInit(&var);
    if (SUCCEEDED(reader->GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE, MF_PD_DURATION, &var))) {
        if (var.vt == VT_UI8 || var.vt == VT_I8) {
            duration_secs = static_cast<float>(static_cast<double>(var.uhVal.QuadPart) / 10000000.0);
        }
        PropVariantClear(&var);
    }

    update_frame();
    return S_OK;
}

HRESULT BrowserMediaPlayer::play() {
    std::scoped_lock lock(mutex);
    is_playing = true;
    return S_OK;
}

HRESULT BrowserMediaPlayer::pause() {
    std::scoped_lock lock(mutex);
    is_playing = false;
    return S_OK;
}

HRESULT BrowserMediaPlayer::get_state(uint32_t* is_playing_out, float* pos_out, float* dur_out) {
    std::scoped_lock lock(mutex);
    if (is_playing_out) {
        *is_playing_out = is_playing ? 1 : 0;
    }
    if (pos_out) {
        *pos_out = position_secs;
    }
    if (dur_out) {
        *dur_out = duration_secs;
    }
    return S_OK;
}

HRESULT BrowserMediaPlayer::seek(float pos) {
    std::scoped_lock lock(mutex);
    if (pos < 0.0f) pos = 0.0f;
    if (duration_secs > 0.0f && pos > duration_secs) pos = duration_secs;
    position_secs = pos;

    if (reader) {
        PROPVARIANT var;
        PropVariantInit(&var);
        var.vt = VT_I8;
        var.hVal.QuadPart = static_cast<LONGLONG>(static_cast<double>(pos) * 10000000.0);
        HRESULT hr = reader->SetCurrentPosition(GUID_NULL, var);
        PropVariantClear(&var);
        if (FAILED(hr)) {
            return hr;
        }

        DWORD stream_index = 0, flags = 0;
        LONGLONG timestamp = 0;
        ComPtr<IMFSample> sample;
        if (SUCCEEDED(reader->ReadSample(MF_SOURCE_READER_FIRST_VIDEO_STREAM, 0, &stream_index, &flags, &timestamp, &sample)) && sample) {
            ComPtr<IMFMediaBuffer> buffer;
            if (SUCCEEDED(sample->ConvertToContiguousBuffer(&buffer))) {
                BYTE* data = nullptr;
                DWORD max_len = 0, cur_len = 0;
                if (SUCCEEDED(buffer->Lock(&data, &max_len, &cur_len))) {
                    if (!bitmap && renderer) {
                        browser_renderer_create_bitmap(renderer, width, height, &bitmap);
                    }
                    if (bitmap && data) {
                        D2D1_RECT_U dest_rect = { 0, 0, width, height };
                        bitmap->CopyFromMemory(&dest_rect, data, width * 4);
                    }
                    buffer->Unlock();
                }
            }
        }
    } else {
        render_synthetic_frame();
    }
    return S_OK;
}

void BrowserMediaPlayer::update_frame() {
    std::scoped_lock lock(mutex);
    if (!renderer) return;

    if (is_synthetic) {
        if (is_playing) {
            position_secs += 1.0f / 30.0f;
            if (duration_secs > 0.0f && position_secs >= duration_secs) {
                position_secs = duration_secs;
                is_playing = false;
            }
        }
        render_synthetic_frame();
        return;
    }

    if (!reader) {
        is_synthetic = true;
        render_synthetic_frame();
        return;
    }

    if (!is_playing && bitmap) {
        return;
    }

    DWORD stream_index = 0;
    DWORD flags = 0;
    LONGLONG timestamp = 0;
    ComPtr<IMFSample> sample;

    HRESULT hr = reader->ReadSample(
        MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        0,
        &stream_index,
        &flags,
        &timestamp,
        &sample
    );

    if (SUCCEEDED(hr)) {
        if (flags & MF_SOURCE_READERF_ENDOFSTREAM) {
            is_playing = false;
            return;
        }
        if (sample) {
            if (timestamp > 0) {
                position_secs = static_cast<float>(static_cast<double>(timestamp) / 10000000.0);
            }
            ComPtr<IMFMediaBuffer> buffer;
            if (SUCCEEDED(sample->ConvertToContiguousBuffer(&buffer))) {
                BYTE* data = nullptr;
                DWORD max_len = 0, cur_len = 0;
                if (SUCCEEDED(buffer->Lock(&data, &max_len, &cur_len))) {
                    if (!bitmap && renderer) {
                        browser_renderer_create_bitmap(renderer, width, height, &bitmap);
                    }
                    if (bitmap && data) {
                        D2D1_RECT_U dest_rect = { 0, 0, width, height };
                        bitmap->CopyFromMemory(&dest_rect, data, width * 4);
                    }
                    buffer->Unlock();
                }
            }
        }
    }
}

void BrowserMediaPlayer::render_synthetic_frame() {
    if (!renderer) return;
    if (synthetic_buffer.size() != width * height) {
        synthetic_buffer.resize(width * height);
    }

    const uint32_t bg_color = 0xFF000000 | (0x0B << 16) | (0x19 << 8) | 0x2C;
    const uint32_t accent_color = 0xFF000000 | (0x19 << 16) | (0xBD << 8) | 0xA4;

    for (size_t y = 0; y < height; ++y) {
        for (size_t x = 0; x < width; ++x) {
            synthetic_buffer[y * width + x] = bg_color;
        }
    }

    int box_size = 40;
    int max_x = (int)width - box_size;
    int progress_x = max_x > 0 ? (static_cast<int>(position_secs * 60.0f) % max_x) : 0;
    int box_y = (int)(height / 2) - box_size / 2;
    if (box_y < 0) box_y = 0;

    for (int y = box_y; y < box_y + box_size && y < (int)height; ++y) {
        for (int x = progress_x; x < progress_x + box_size && x < (int)width; ++x) {
            synthetic_buffer[y * width + x] = accent_color;
        }
    }

    if (!bitmap) {
        browser_renderer_create_bitmap(renderer, width, height, &bitmap);
    }
    if (bitmap) {
        D2D1_RECT_U dest_rect = { 0, 0, width, height };
        bitmap->CopyFromMemory(&dest_rect, synthetic_buffer.data(), width * 4);
    }
}

ID2D1Bitmap1* BrowserMediaPlayer::current_frame() {
    std::scoped_lock lock(mutex);
    return bitmap.Get();
}

int32_t browser_media_player_play(BrowserMediaPlayer* player) noexcept {
    if (!player) return E_INVALIDARG;
    {
        std::scoped_lock lock(g_players_mutex);
        if (g_active_players.find(player) == g_active_players.end()) return E_INVALIDARG;
    }
    return player->play();
}

int32_t browser_media_player_pause(BrowserMediaPlayer* player) noexcept {
    if (!player) return E_INVALIDARG;
    {
        std::scoped_lock lock(g_players_mutex);
        if (g_active_players.find(player) == g_active_players.end()) return E_INVALIDARG;
    }
    return player->pause();
}

int32_t browser_media_player_get_state(BrowserMediaPlayer* player,
    uint32_t* is_playing, float* position_secs, float* duration_secs) noexcept {
    if (!player) return E_INVALIDARG;
    {
        std::scoped_lock lock(g_players_mutex);
        if (g_active_players.find(player) == g_active_players.end()) return E_INVALIDARG;
    }
    return player->get_state(is_playing, position_secs, duration_secs);
}

int32_t browser_media_player_seek(BrowserMediaPlayer* player, float position_secs) noexcept {
    if (!player) return E_INVALIDARG;
    {
        std::scoped_lock lock(g_players_mutex);
        if (g_active_players.find(player) == g_active_players.end()) return E_INVALIDARG;
    }
    return player->seek(position_secs);
}

uint32_t browser_media_player_get_id(const BrowserMediaPlayer* player) noexcept {
    if (!player) return 0;
    {
        std::scoped_lock lock(g_players_mutex);
        if (g_active_players.find(const_cast<BrowserMediaPlayer*>(player)) == g_active_players.end()) return 0;
    }
    return player->get_id();
}

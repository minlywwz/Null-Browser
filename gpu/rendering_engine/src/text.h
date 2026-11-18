#pragma once

#include "rendering_engine.h"

#include <dwrite.h>
#include <wrl/client.h>
#include <mutex>
#include <vector>

struct BrowserTextLayout {
    Microsoft::WRL::ComPtr<IDWriteTextLayout> value;
    std::vector<BrowserHitRegion> hits;
    std::vector<BrowserTextRun> runs;
    mutable std::mutex drawing;
};

#include "text.h"

#include <algorithm>
#include <cmath>
#include <memory>
#include <new>

using Microsoft::WRL::ComPtr;

struct BrowserTextFactory {
    ComPtr<IDWriteFactory> value;
};

int32_t browser_text_factory_create(BrowserTextFactory** result) noexcept {
    if (result == nullptr) {
        return E_POINTER;
    }
    *result = nullptr;
    auto factory = std::unique_ptr<BrowserTextFactory>(new (std::nothrow) BrowserTextFactory);
    if (!factory) {
        return E_OUTOFMEMORY;
    }
    const HRESULT hr = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED, __uuidof(IDWriteFactory),
        reinterpret_cast<IUnknown**>(factory->value.GetAddressOf()));
    if (SUCCEEDED(hr)) {
        *result = factory.release();
    }
    return hr;
}

void browser_text_factory_destroy(BrowserTextFactory* factory) noexcept {
    delete factory;
}

int32_t browser_text_layout_create(BrowserTextFactory* factory, const uint16_t* text,
    uint32_t length, const BrowserTextOptions* options, const BrowserTextRange* ranges,
    uint32_t range_count, const BrowserTextRun* runs, uint32_t run_count,
    BrowserTextLayout** result, float* width, float* height) noexcept {
    if (!factory || !text || !options || !result || !width || !height || (range_count && !ranges)
        || (run_count && !runs) || !length || length > 65536 || range_count > length
        || run_count > length
        || !std::isfinite(options->size) || options->size < 1 || options->size > 256
        || !std::isfinite(options->width) || options->width < 1 || options->width > 16384) {
        return E_INVALIDARG;
    }
    *result = nullptr;
    *width = 0;
    *height = 0;
    try {
        auto layout = std::make_unique<BrowserTextLayout>();
        ComPtr<IDWriteTextFormat> format;
        HRESULT hr = factory->value->CreateTextFormat(L"Segoe UI", nullptr,
            static_cast<DWRITE_FONT_WEIGHT>(std::clamp(options->weight, 100u, 900u)),
            DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, options->size,
            L"en-US", &format);
        if (FAILED(hr)) {
            return hr;
        }
        format->SetWordWrapping(options->wrap ? DWRITE_WORD_WRAPPING_WRAP : DWRITE_WORD_WRAPPING_NO_WRAP);
        if (options->alignment == 1) {
            format->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
        } else if (options->alignment == 2) {
            format->SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING);
        }
        if (options->trim) {
            ComPtr<IDWriteInlineObject> ellipsis;
            if (SUCCEEDED(factory->value->CreateEllipsisTrimmingSign(format.Get(), &ellipsis))) {
                DWRITE_TRIMMING trimming{DWRITE_TRIMMING_GRANULARITY_CHARACTER, 0, 0};
                format->SetTrimming(&trimming, ellipsis.Get());
            }
        }
        static_assert(sizeof(wchar_t) == sizeof(uint16_t));
        hr = factory->value->CreateTextLayout(reinterpret_cast<const wchar_t*>(text), length,
            format.Get(), options->width, 2000000.0f, &layout->value);
        if (FAILED(hr)) {
            return hr;
        }
        uint32_t run_end = 0;
        for (uint32_t i = 0; i < run_count; ++i) {
            const auto& run = runs[i];
            const auto& range = run.range;
            const auto& ink = run.color;
            const auto channel = [](float value) { return value >= 0 && value <= 1; };
            if (!range.length || range.start < run_end || range.start >= length
                || range.length > length - range.start || !(run.size >= 1 && run.size <= 256)
                || run.weight < 100 || run.weight > 900 || run.family > 2
                || !channel(ink.red) || !channel(ink.green) || !channel(ink.blue) || !channel(ink.alpha)) {
                return E_INVALIDARG;
            }
            run_end = range.start + range.length;
            const DWRITE_TEXT_RANGE span{range.start, range.length};
            const wchar_t* families[] = {L"Segoe UI", L"Times New Roman", L"Consolas"};
            if (FAILED(hr = layout->value->SetFontSize(run.size, span))
                || FAILED(hr = layout->value->SetFontWeight(static_cast<DWRITE_FONT_WEIGHT>(run.weight), span))
                || FAILED(hr = layout->value->SetFontFamilyName(families[run.family], span))
                || FAILED(hr = layout->value->SetFontStyle(run.italic ? DWRITE_FONT_STYLE_ITALIC : DWRITE_FONT_STYLE_NORMAL, span))
                || FAILED(hr = layout->value->SetUnderline(run.underline != 0, span))
                || FAILED(hr = layout->value->SetStrikethrough(run.strikethrough != 0, span))) {
                return hr;
            }
            layout->runs.push_back(run);
        }
        std::vector<DWRITE_HIT_TEST_METRICS> hit_metrics;
        for (uint32_t i = 0; i < range_count; ++i) {
            const auto& range = ranges[i];
            if (!range.length || range.start >= length || range.length > length - range.start) {
                return E_INVALIDARG;
            }
            if (!run_count && FAILED(hr = layout->value->SetUnderline(TRUE,
                DWRITE_TEXT_RANGE{range.start, range.length}))) {
                return hr;
            }
            UINT32 count = 0;
            hr = layout->value->HitTestTextRange(range.start, range.length, 0, 0, nullptr, 0, &count);
            if (FAILED(hr) && hr != HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER)) {
                return hr;
            }
            if (count == 0) {
                continue;
            }
            if (count > 65536 || layout->hits.size() + count > 65536) {
                return E_INVALIDARG;
            }
            hit_metrics.resize(count);
            hr = layout->value->HitTestTextRange(range.start, range.length, 0, 0,
                hit_metrics.data(), count, &count);
            if (FAILED(hr)) {
                return hr;
            }
            for (UINT32 j = 0; j < count; ++j) {
                const auto& hit = hit_metrics[j];
                layout->hits.push_back({{hit.left, hit.top, hit.width, hit.height}, i});
            }
        }
        DWRITE_TEXT_METRICS metrics{};
        hr = layout->value->GetMetrics(&metrics);
        if (SUCCEEDED(hr)) {
            *width = metrics.width;
            *height = metrics.height;
            *result = layout.release();
        }
        return hr;
    } catch (const std::bad_alloc&) {
        return E_OUTOFMEMORY;
    } catch (...) {
        return E_FAIL;
    }
}

const BrowserHitRegion* browser_text_layout_hits(const BrowserTextLayout* layout, uint32_t* count) noexcept {
    if (!layout || !count) {
        return nullptr;
    }
    *count = static_cast<uint32_t>(layout->hits.size());
    return layout->hits.data();
}

void browser_text_layout_destroy(BrowserTextLayout* layout) noexcept {
    delete layout;
}

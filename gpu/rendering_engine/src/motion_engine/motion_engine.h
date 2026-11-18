#pragma once

#include "rendering_engine.h"

#include <chrono>

class MotionEngine {
public:
    BrowserRect update(const BrowserMotionFrame& frame);
    bool complete() const { return complete_; }
    void reset() { initialized_ = false; }

private:
    using Clock = std::chrono::steady_clock;
    BrowserRect sample(Clock::time_point now);

    bool initialized_ = false;
    bool complete_ = false;
    uint32_t transition_ = 0;
    std::chrono::milliseconds duration_{0};
    Clock::time_point started_;
    BrowserRect start_{};
    BrowserRect target_{};
};

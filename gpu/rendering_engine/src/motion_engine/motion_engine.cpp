#include "motion_engine.h"

#include <algorithm>
#include <cmath>

namespace {
bool same_rect(const BrowserRect& a, const BrowserRect& b) {
    return a.x == b.x && a.y == b.y && a.width == b.width && a.height == b.height;
}
}

BrowserRect MotionEngine::sample(Clock::time_point now) {
    if (duration_.count() == 0 || now - started_ >= duration_) {
        complete_ = true;
        return target_;
    }
    complete_ = false;
    const float t = std::chrono::duration<float>(now - started_).count()
        / std::chrono::duration<float>(duration_).count();
    const float eased = t * t * (3.0f - 2.0f * t);
    return {
        std::lerp(start_.x, target_.x, eased), std::lerp(start_.y, target_.y, eased),
        std::lerp(start_.width, target_.width, eased), std::lerp(start_.height, target_.height, eased),
    };
}

BrowserRect MotionEngine::update(const BrowserMotionFrame& frame) {
    const auto now = Clock::now();
    if (!initialized_ || transition_ != frame.transition || !same_rect(target_, frame.target_screen)) {
        start_ = initialized_ ? sample(now) : frame.initial_screen;
        target_ = frame.target_screen;
        transition_ = frame.transition;
        initialized_ = true;
        started_ = now;
        duration_ = std::chrono::milliseconds(same_rect(start_, target_) ? 0 : std::min(frame.duration_ms, 250u));
    } else if (frame.duration_ms == 0) {
        duration_ = std::chrono::milliseconds(0);
    }
    const auto screen = sample(now);
    const float scale = frame.scale > 0.0f ? frame.scale : 1.0f;
    return {
        (screen.x - frame.screen_x) / scale, (screen.y - frame.screen_y) / scale,
        screen.width / scale, screen.height / scale,
    };
}

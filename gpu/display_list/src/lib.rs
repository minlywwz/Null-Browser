use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }

    pub fn translated(self, x: f32, y: f32) -> Self {
        Self::new(self.x + x, self.y + y, self.width, self.height)
    }

    pub fn intersects(self, other: Self) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then_some(Self::new(x, y, right - x, bottom - y))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct Color {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

impl Color {
    pub const TRANSPARENT: Self = Self {
        red: 0.0,
        green: 0.0,
        blue: 0.0,
        alpha: 0.0,
    };

    pub const fn rgb(value: u32) -> Self {
        Self::rgba((value << 8) | 0xff)
    }

    pub const fn rgba(value: u32) -> Self {
        Self {
            red: (value >> 24) as u8 as f32 / 255.0,
            green: (value >> 16) as u8 as f32 / 255.0,
            blue: (value >> 8) as u8 as f32 / 255.0,
            alpha: value as u8 as f32 / 255.0,
        }
    }
}

/// All coordinates are in logical pixels. Resource IDs are local to the list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    PushClip {
        rect: Rect,
    },
    PopClip,
    Fill {
        rect: Rect,
        color: Color,
        radius: f32,
    },
    Text {
        bounds: Rect,
        color: Color,
        layout: u32,
    },
    Image {
        rect: Rect,
        image: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayList {
    pub background: Color,
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum TextAlign {
    #[default]
    Start = 0,
    Center = 1,
    End = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub size: f32,
    pub weight: u32,
    pub align: TextAlign,
}

/// DirectWrite consumes UTF-16 offsets. Keeping these offsets in the contract
/// also makes the link hit regions independent of UTF-8 byte boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct TextRange {
    pub start: u32,
    pub length: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct FontFamily(pub u32);

#[allow(non_upper_case_globals)]
impl FontFamily {
    pub const SansSerif: Self = Self(0);
    pub const Serif: Self = Self(1);
    pub const Monospace: Self = Self(2);

    pub const fn custom(index: u32) -> Self {
        Self(3 + index)
    }

    pub const fn id(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[repr(C)]
pub struct TextRun {
    pub range: TextRange,
    pub color: Color,
    pub size: f32,
    pub weight: u32,
    pub family: FontFamily,
    pub italic: u32,
    pub underline: u32,
    pub strikethrough: u32,
}

#[derive(Serialize, Deserialize)]
pub struct TextMetrics {
    pub layout: u32,
    pub width: f32,
    pub height: f32,
    pub links: Vec<(usize, Rect)>,
}

pub trait TextMeasurer {
    fn measure(
        &mut self,
        text: &str,
        style: TextStyle,
        width: f32,
        links: &[TextRange],
    ) -> Result<TextMetrics, String>;

    fn measure_styled(
        &mut self,
        text: &str,
        style: TextStyle,
        width: f32,
        links: &[TextRange],
        _runs: &[TextRun],
        _wrap: bool,
    ) -> Result<TextMetrics, String> {
        self.measure(text, style, width, links)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_font_family_properties() {
        assert_eq!(FontFamily::SansSerif.id(), 0);
        assert_eq!(FontFamily::Serif.id(), 1);
        assert_eq!(FontFamily::Monospace.id(), 2);
        assert_eq!(FontFamily::custom(0).id(), 3);
        assert_eq!(FontFamily::custom(1).id(), 4);
        assert_eq!(FontFamily::custom(42).id(), 45);
        assert_eq!(FontFamily::default(), FontFamily::SansSerif);
    }
}

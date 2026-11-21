use std::{
    ffi::c_void,
    ptr::{self, NonNull},
    slice,
    sync::Arc,
};

use display_list::{
    Color, Command, DisplayList, FontFamily, Rect, TextMeasurer, TextMetrics, TextRange, TextRun,
    TextStyle,
};

const ABI_VERSION: u32 = 3;

#[repr(C)]
struct TextOptions {
    size: f32,
    width: f32,
    weight: u32,
    alignment: u32,
    wrap: u32,
    trim: u32,
}

impl TextOptions {
    fn new(style: TextStyle, width: f32, wrap: bool, trim: bool) -> Self {
        Self {
            size: style.size,
            width,
            weight: style.weight,
            alignment: style.align as u32,
            wrap: u32::from(wrap),
            trim: u32::from(trim),
        }
    }
}

#[repr(C)]
struct HitRegion {
    rect: Rect,
    range: u32,
}

#[repr(C)]
struct ImageView {
    bgra: *const u8,
    width: u32,
    height: u32,
    stride: u32,
}

#[repr(C)]
struct DrawCommand {
    rect: Rect,
    color: Color,
    radius: f32,
    kind: u32,
    resource: u32,
}

#[repr(C)]
struct SceneView {
    key: u64,
    commands: *const DrawCommand,
    text: *const TextLayout,
    images: *const ImageView,
    command_count: u32,
    text_count: u32,
    image_count: u32,
    background: Color,
}

#[repr(C)]
struct DrawLayer {
    scene: *const SceneView,
    clip: Rect,
    offset_x: f32,
    offset_y: f32,
    opacity: f32,
    fill_background: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct MotionFrame {
    pub initial_screen: Rect,
    pub target_screen: Rect,
    pub source_frame: u64,
    pub screen_x: f32,
    pub screen_y: f32,
    pub scale: f32,
    pub transition: u32,
    pub duration_ms: u32,
}

#[repr(C)]
struct Frame {
    layers: *const DrawLayer,
    motion: *const MotionFrame,
    background: Color,
    layer_count: u32,
}

#[derive(Default)]
#[repr(C)]
struct NativeFrameResult {
    presented: u32,
    motion_complete: u32,
}

pub struct FrameResult {
    pub presented: bool,
    pub motion_complete: bool,
}

const _: () = {
    assert!(size_of::<DrawCommand>() == 44);
    assert!(size_of::<MotionFrame>() == 64);
    #[cfg(target_pointer_width = "64")]
    {
        assert!(size_of::<SceneView>() == 64);
        assert!(size_of::<DrawLayer>() == 40);
        assert!(size_of::<Frame>() == 40);
        assert!(size_of::<ImageView>() == 24);
    }
};

unsafe extern "C" {
    fn browser_renderer_abi() -> u32;
    fn browser_renderer_create(
        window: *mut c_void,
        width: u32,
        height: u32,
        dpi: u32,
        warp: u32,
        result: *mut *mut c_void,
    ) -> i32;
    fn browser_renderer_uses_warp(renderer: *const c_void) -> u32;
    fn browser_renderer_resize(renderer: *mut c_void, width: u32, height: u32, dpi: u32) -> i32;
    fn browser_renderer_draw(
        renderer: *mut c_void,
        frame: *const Frame,
        result: *mut NativeFrameResult,
    ) -> i32;
    fn browser_renderer_read_pixel(renderer: *mut c_void, x: u32, y: u32, pixel: *mut u32) -> i32;
    fn browser_renderer_destroy(renderer: *mut c_void);
    fn browser_text_factory_create(result: *mut *mut c_void) -> i32;
    fn browser_text_factory_register_font(
        factory: *mut c_void,
        data: *const u8,
        length: u32,
        family_name: *const u16,
        font_id: *mut u32,
    ) -> i32;
    fn browser_text_factory_destroy(factory: *mut c_void);
    fn browser_text_layout_create(
        factory: *mut c_void,
        text: *const u16,
        length: u32,
        options: *const TextOptions,
        ranges: *const TextRange,
        range_count: u32,
        runs: *const TextRun,
        run_count: u32,
        result: *mut *mut c_void,
        width: *mut f32,
        height: *mut f32,
    ) -> i32;
    fn browser_text_layout_hits(layout: *const c_void, count: *mut u32) -> *const HitRegion;
    fn browser_text_layout_destroy(layout: *mut c_void);
}

fn check(status: i32, operation: &str) -> Result<(), String> {
    if status < 0 {
        Err(format!(
            "{operation} failed (HRESULT 0x{:08x}).",
            status as u32
        ))
    } else {
        Ok(())
    }
}

pub struct TextFactory(NonNull<c_void>);

impl TextFactory {
    pub fn new() -> Result<Self, String> {
        let mut factory = ptr::null_mut();
        // The ABI writes an owned handle or null into this out parameter.
        check(
            unsafe { browser_text_factory_create(&mut factory) },
            "DirectWrite initialization",
        )?;
        NonNull::new(factory)
            .map(Self)
            .ok_or_else(|| "DirectWrite returned no factory.".into())
    }

    pub fn batch(&self) -> TextBatch<'_> {
        TextBatch {
            factory: self,
            layouts: Vec::new(),
        }
    }

    pub fn register_font(&mut self, data: &[u8], family: &str) -> Result<FontFamily, String> {
        let family_u16: Vec<u16> = family.encode_utf16().chain(std::iter::once(0)).collect();
        let length = u32::try_from(data.len()).map_err(|_| "The font data is too large.")?;
        let mut font_id = 0u32;
        check(
            unsafe {
                browser_text_factory_register_font(
                    self.0.as_ptr(),
                    if data.is_empty() {
                        ptr::null()
                    } else {
                        data.as_ptr()
                    },
                    length,
                    family_u16.as_ptr(),
                    &mut font_id,
                )
            },
            "Register font",
        )?;
        Ok(FontFamily(font_id))
    }
}

impl Drop for TextFactory {
    fn drop(&mut self) {
        unsafe { browser_text_factory_destroy(self.0.as_ptr()) };
    }
}

#[repr(transparent)]
struct TextLayout(NonNull<c_void>);

// An immutable DirectWrite layout is transferred to the drawing thread by value.
// It holds its own references, and the wrapper never allows concurrent access.
unsafe impl Send for TextLayout {}

impl Drop for TextLayout {
    fn drop(&mut self) {
        unsafe { browser_text_layout_destroy(self.0.as_ptr()) };
    }
}

pub struct TextBatch<'a> {
    factory: &'a TextFactory,
    layouts: Vec<TextLayout>,
}

impl TextBatch<'_> {
    fn measure_text(
        &mut self,
        text: &str,
        options: TextOptions,
        links: &[TextRange],
        runs: &[TextRun],
    ) -> Result<TextMetrics, String> {
        let text: Vec<_> = text.encode_utf16().collect();
        let mut layout = ptr::null_mut();
        let mut measured_width = 0.0;
        let mut height = 0.0;
        let length = u32::try_from(text.len()).map_err(|_| "The text layout is too large.")?;
        let range_count = u32::try_from(links.len()).map_err(|_| "The text has too many links.")?;
        let run_count =
            u32::try_from(runs.len()).map_err(|_| "The text has too many style runs.")?;
        check(
            unsafe {
                browser_text_layout_create(
                    self.factory.0.as_ptr(),
                    text.as_ptr(),
                    length,
                    &options,
                    links.as_ptr(),
                    range_count,
                    runs.as_ptr(),
                    run_count,
                    &mut layout,
                    &mut measured_width,
                    &mut height,
                )
            },
            "Text layout",
        )?;
        let native = TextLayout(NonNull::new(layout).ok_or("DirectWrite returned no layout.")?);
        let mut count = 0;
        let hits = unsafe { browser_text_layout_hits(native.0.as_ptr(), &mut count) };
        let links = if count == 0 {
            Vec::new()
        } else {
            // The native vector lives inside the layout and is immutable after creation.
            unsafe { slice::from_raw_parts(hits, count as usize) }
                .iter()
                .map(|hit| (hit.range as usize, hit.rect))
                .collect()
        };
        let layout = u32::try_from(self.layouts.len())
            .map_err(|_| "The scene has too many text layouts.")?;
        self.layouts.push(native);
        Ok(TextMetrics {
            layout,
            width: measured_width,
            height,
            links,
        })
    }

    pub fn label(
        &mut self,
        text: &str,
        style: TextStyle,
        width: f32,
    ) -> Result<TextMetrics, String> {
        self.measure_text(
            text,
            TextOptions::new(style, width.max(1.0), false, true),
            &[],
            &[],
        )
    }

    pub fn prepare(self, key: u64, list: DisplayList, images: Vec<Image>) -> PreparedScene {
        PreparedScene::new(key, list, self.layouts, images)
    }
}

impl TextMeasurer for TextBatch<'_> {
    fn measure(
        &mut self,
        text: &str,
        style: TextStyle,
        width: f32,
        links: &[TextRange],
    ) -> Result<TextMetrics, String> {
        self.measure_styled(text, style, width, links, &[], true)
    }

    fn measure_styled(
        &mut self,
        text: &str,
        style: TextStyle,
        width: f32,
        links: &[TextRange],
        runs: &[TextRun],
        wrap: bool,
    ) -> Result<TextMetrics, String> {
        self.measure_text(
            text,
            TextOptions::new(style, width, wrap, false),
            links,
            runs,
        )
    }
}

#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    bgra: Arc<[u8]>,
}

impl Image {
    pub fn new(width: u32, height: u32, bgra: Vec<u8>) -> Result<Self, String> {
        if !(1..=4096).contains(&width)
            || !(1..=4096).contains(&height)
            || bgra.len() != width as usize * height as usize * 4
        {
            return Err("Invalid image size.".into());
        }
        Ok(Self {
            width,
            height,
            bgra: bgra.into(),
        })
    }
}

pub struct PreparedScene {
    key: u64,
    commands: Vec<DrawCommand>,
    text: Vec<TextLayout>,
    images: Vec<Image>,
    background: Color,
}

impl PreparedScene {
    fn new(key: u64, list: DisplayList, text: Vec<TextLayout>, images: Vec<Image>) -> Self {
        let commands = list
            .commands
            .into_iter()
            .map(|command| match command {
                Command::PushClip { rect } => DrawCommand {
                    rect,
                    color: Color::TRANSPARENT,
                    radius: 0.0,
                    kind: 4,
                    resource: 0,
                },
                Command::PopClip => DrawCommand {
                    rect: Rect::default(),
                    color: Color::TRANSPARENT,
                    radius: 0.0,
                    kind: 5,
                    resource: 0,
                },
                Command::Fill {
                    rect,
                    color,
                    radius,
                } => DrawCommand {
                    rect,
                    color,
                    radius,
                    kind: 1,
                    resource: 0,
                },
                Command::Text {
                    bounds,
                    color,
                    layout,
                } => DrawCommand {
                    rect: bounds,
                    color,
                    radius: 0.0,
                    kind: 2,
                    resource: layout,
                },
                Command::Image { rect, image } => DrawCommand {
                    rect,
                    color: Color::TRANSPARENT,
                    radius: 0.0,
                    kind: 3,
                    resource: image,
                },
            })
            .collect();
        Self {
            key,
            commands,
            text,
            images,
            background: list.background,
        }
    }

    fn image_views(&self) -> Vec<ImageView> {
        self.images
            .iter()
            .map(|image| ImageView {
                bgra: image.bgra.as_ptr(),
                width: image.width,
                height: image.height,
                stride: image.width * 4,
            })
            .collect()
    }

    fn view(&self, images: &[ImageView]) -> SceneView {
        SceneView {
            key: self.key,
            commands: self.commands.as_ptr(),
            text: self.text.as_ptr(),
            images: images.as_ptr(),
            command_count: self.commands.len() as u32,
            text_count: self.text.len() as u32,
            image_count: images.len() as u32,
            background: self.background,
        }
    }
}

pub struct Layer<'a> {
    pub scene: &'a PreparedScene,
    pub clip: Rect,
    pub offset_x: f32,
    pub offset_y: f32,
    pub opacity: f32,
    pub fill_background: bool,
}

pub struct Renderer(NonNull<c_void>);

impl Renderer {
    fn new(
        window: *mut c_void,
        width: u32,
        height: u32,
        dpi: u32,
        warp: bool,
    ) -> Result<Self, String> {
        if unsafe { browser_renderer_abi() } != ABI_VERSION {
            return Err("Rendering Engine ABI mismatch.".into());
        }
        let mut renderer = ptr::null_mut();
        check(
            unsafe {
                browser_renderer_create(window, width, height, dpi, u32::from(warp), &mut renderer)
            },
            "D3D11 initialization",
        )?;
        NonNull::new(renderer)
            .map(Self)
            .ok_or_else(|| "Rendering Engine returned no renderer.".into())
    }

    /// # Safety
    /// The caller keeps the HWND alive until this renderer has been dropped and
    /// keeps the window message pump running while the renderer can present.
    pub unsafe fn for_window(
        window: usize,
        width: u32,
        height: u32,
        dpi: u32,
        warp: bool,
    ) -> Result<Self, String> {
        if window == 0 {
            return Err("Invalid browser window.".into());
        }
        Self::new(window as *mut c_void, width, height, dpi, warp)
    }

    pub fn offscreen(width: u32, height: u32) -> Result<Self, String> {
        Self::new(ptr::null_mut(), width, height, 96, true)
    }

    pub fn uses_warp(&self) -> bool {
        unsafe { browser_renderer_uses_warp(self.0.as_ptr()) != 0 }
    }

    pub fn resize(&mut self, width: u32, height: u32, dpi: u32) -> Result<(), String> {
        check(
            unsafe { browser_renderer_resize(self.0.as_ptr(), width, height, dpi) },
            "Resize",
        )
    }

    pub fn draw(
        &mut self,
        layers: &[Layer<'_>],
        background: Color,
        motion: Option<&MotionFrame>,
    ) -> Result<FrameResult, String> {
        if layers.len() > 4 {
            return Err("Too many compositor layers.".into());
        }
        let images: Vec<_> = layers
            .iter()
            .map(|layer| layer.scene.image_views())
            .collect();
        let views: Vec<_> = layers
            .iter()
            .zip(&images)
            .map(|(layer, images)| layer.scene.view(images))
            .collect();
        let layers: Vec<_> = layers
            .iter()
            .zip(&views)
            .map(|(layer, view)| DrawLayer {
                scene: view,
                clip: layer.clip,
                offset_x: layer.offset_x,
                offset_y: layer.offset_y,
                opacity: layer.opacity.clamp(0.0, 1.0),
                fill_background: u32::from(layer.fill_background),
            })
            .collect();
        let frame = Frame {
            layers: layers.as_ptr(),
            motion: motion.map_or(ptr::null(), ptr::from_ref),
            background,
            layer_count: layers.len() as u32,
        };
        let mut result = NativeFrameResult::default();
        check(
            unsafe { browser_renderer_draw(self.0.as_ptr(), &frame, &mut result) },
            "Frame rendering",
        )?;
        Ok(FrameResult {
            presented: result.presented != 0,
            motion_complete: result.motion_complete != 0,
        })
    }

    pub fn read_pixel(&mut self, x: u32, y: u32) -> Result<u32, String> {
        let mut pixel = 0;
        check(
            unsafe { browser_renderer_read_pixel(self.0.as_ptr(), x, y, &mut pixel) },
            "Read pixel",
        )?;
        Ok(pixel)
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        unsafe { browser_renderer_destroy(self.0.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use display_list::TextAlign;

    #[test]
    fn draws_and_clips_commands_on_d3d11_warp() {
        let mut renderer = Renderer::offscreen(64, 64).unwrap();
        assert!(renderer.uses_warp());
        let factory = TextFactory::new().unwrap();
        let scene = factory.batch().prepare(
            1,
            DisplayList {
                background: Color::rgb(0),
                commands: vec![Command::Fill {
                    rect: Rect::new(4.0, 4.0, 40.0, 40.0),
                    color: Color::rgb(0x19bda4),
                    radius: 0.0,
                }],
            },
            Vec::new(),
        );
        let frame = renderer
            .draw(
                &[Layer {
                    scene: &scene,
                    clip: Rect::new(0.0, 0.0, 24.0, 24.0),
                    offset_x: 0.0,
                    offset_y: 0.0,
                    opacity: 1.0,
                    fill_background: true,
                }],
                Color::rgb(0),
                None,
            )
            .unwrap();
        assert!(frame.presented);
        assert_eq!(renderer.read_pixel(10, 10).unwrap() & 0x00ff_ffff, 0x19bda4);
        assert_eq!(renderer.read_pixel(30, 30).unwrap() & 0x00ff_ffff, 0);
    }

    #[test]
    fn uses_directwrite_hit_regions_for_a_wrapped_unicode_link() {
        let factory = TextFactory::new().unwrap();
        let mut batch = factory.batch();
        let metrics = batch
            .measure(
                "🧭 Пример кириллицы",
                TextStyle {
                    size: 18.0,
                    weight: 400,
                    align: TextAlign::Start,
                },
                80.0,
                &[TextRange {
                    start: 3,
                    length: 16,
                }],
            )
            .unwrap();
        assert!(metrics.height > 30.0);
        assert!(metrics.links.len() > 1);
        assert!(
            metrics
                .links
                .iter()
                .all(|(index, rect)| *index == 0 && rect.width > 0.0)
        );
    }

    #[test]
    fn preserves_the_live_motion_layer_across_output_resize() {
        let mut renderer = Renderer::offscreen(640, 360).unwrap();
        let rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        let mut motion = MotionFrame {
            initial_screen: rect,
            target_screen: rect,
            source_frame: 1,
            screen_x: 0.0,
            screen_y: 0.0,
            scale: 1.0,
            transition: 1,
            duration_ms: 0,
        };
        let first = renderer.draw(&[], Color::rgb(0), Some(&motion)).unwrap();
        assert!(first.presented && first.motion_complete);
        assert_eq!(renderer.read_pixel(25, 25).unwrap() & 0xffffff, 0x89ffe6);
        renderer.resize(800, 600, 96).unwrap();
        motion.target_screen = rect.translated(100.0, 80.0);
        motion.transition = 2;
        motion.source_frame = 2;
        let second = renderer.draw(&[], Color::rgb(0), Some(&motion)).unwrap();
        assert!(second.presented && second.motion_complete);
        assert_eq!(renderer.read_pixel(125, 105).unwrap() & 0xffffff, 0x143845);
        assert_eq!(renderer.read_pixel(141, 105).unwrap() & 0xffffff, 0x89ffe6);
        assert_eq!(renderer.read_pixel(25, 25).unwrap() & 0xffffff, 0);
    }

    #[test]
    fn registers_custom_font_family() {
        let mut factory = TextFactory::new().unwrap();
        let font_id = factory.register_font(&[], "Arial").unwrap();
        assert_eq!(font_id, FontFamily::custom(0));
        let second_id = factory.register_font(&[], "Segoe UI").unwrap();
        assert_eq!(second_id, FontFamily::custom(1));

        let mut batch = factory.batch();
        let metrics = batch
            .measure_styled(
                "Hello Custom Font",
                TextStyle {
                    size: 14.0,
                    weight: 400,
                    align: TextAlign::Start,
                },
                200.0,
                &[],
                &[TextRun {
                    range: TextRange {
                        start: 0,
                        length: 17,
                    },
                    color: Color::rgb(0),
                    size: 14.0,
                    weight: 400,
                    family: font_id,
                    italic: 0,
                    underline: 0,
                    strikethrough: 0,
                }],
                false,
            )
            .unwrap();
        assert!(metrics.width > 0.0);
        assert!(metrics.height > 0.0);
    }
}

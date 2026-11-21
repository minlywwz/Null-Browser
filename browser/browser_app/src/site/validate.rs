use display_list::{Color, Command, Rect};
use rendering_ffi::{Image, PreparedScene, TextBatch};
use web_content_process::Snapshot;
use web_engine::LaidOutPage;

use crate::document::Geometry;

pub(super) struct Scene {
    pub(super) geometry: Geometry,
    pub(super) prepared: PreparedScene,
}

pub(super) fn prepare_snapshot(
    snapshot: Snapshot,
    batch: TextBatch<'_>,
    text_count: usize,
    key: u32,
    images: &mut Vec<Image>,
) -> Result<Scene, String> {
    let invalid = || String::from("The Web Content Process returned an invalid scene.");
    let Snapshot {
        title,
        page,
        images: decoded,
    } = snapshot;
    let LaidOutPage {
        display_list,
        height,
        links,
        anchors,
    } = page;
    if let Some(decoded) = decoded {
        if decoded.len() > web_content_process::MAX_IMAGES
            || decoded.iter().map(|image| image.bgra.len()).sum::<usize>()
                > web_content_process::MAX_PIXEL_BYTES
        {
            return Err(invalid());
        }
        *images = decoded
            .into_iter()
            .map(|image| Image::new(image.width, image.height, image.bgra))
            .collect::<Result<_, _>>()?;
    }
    if title.len() > 1024
        || display_list.commands.len() > 30_000
        || links.len() > 65_536
        || anchors.len() > 10_000
        || !(0.0..=2_000_000.0).contains(&height)
        || !color_valid(display_list.background)
        || links
            .iter()
            .any(|link| !rect_valid(link.rect) || link.href.len() > network_stack::MAX_URL_LENGTH)
        || anchors
            .values()
            .any(|value| !(0.0..=2_000_000.0).contains(value))
    {
        return Err(invalid());
    }
    let mut clips = 0u32;
    for command in &display_list.commands {
        let valid = match command {
            Command::PushClip { rect } => {
                clips += 1;
                clips <= 128 && rect_valid(*rect)
            }
            Command::PopClip if clips > 0 => {
                clips -= 1;
                true
            }
            Command::PopClip => false,
            Command::Fill {
                rect,
                color,
                radius,
            } => rect_valid(*rect) && color_valid(*color) && (0.0..=128.0).contains(radius),
            Command::Text {
                bounds,
                color,
                layout,
            } => rect_valid(*bounds) && color_valid(*color) && (*layout as usize) < text_count,
            Command::Image { rect, image } => rect_valid(*rect) && (*image as usize) < images.len(),
            Command::Video { rect, .. } => rect_valid(*rect),
        };
        if !valid {
            return Err(invalid());
        }
    }
    if clips != 0 {
        return Err(invalid());
    }
    Ok(Scene {
        geometry: Geometry {
            title,
            height,
            links,
            anchors,
        },
        prepared: batch.prepare(u64::from(key), display_list, images.clone()),
    })
}

fn rect_valid(rect: Rect) -> bool {
    (-2_000_000.0..=2_000_000.0).contains(&rect.x)
        && (-2_000_000.0..=2_000_000.0).contains(&rect.y)
        && (0.0..=2_000_000.0).contains(&rect.width)
        && (0.0..=2_000_000.0).contains(&rect.height)
}

fn color_valid(color: Color) -> bool {
    [color.red, color.green, color.blue, color.alpha]
        .into_iter()
        .all(|value| (0.0..=1.0).contains(&value))
}

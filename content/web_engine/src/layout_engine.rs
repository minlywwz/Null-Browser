use std::collections::HashMap;

use display_list::{
    Color, Command, DisplayList, Rect, TextAlign, TextMeasurer, TextRange, TextRun,
};
use serde::{Deserialize, Serialize};

use crate::{
    css_parser::{AlignItems, Display, GridTrack, JustifyContent, Length, Position},
    dom::{Document, Kind, NodeId},
    style_engine::{ListMarker, Style, compute_styles},
};

const MAX_TEXT_UNITS: u32 = 65_536;
const MAX_HEIGHT: f32 = 2_000_000.0;

#[derive(Clone, Copy)]
pub struct ImageInfo {
    pub id: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Link {
    pub rect: Rect,
    pub href: String,
}

#[derive(Serialize, Deserialize)]
pub struct LaidOutPage {
    pub display_list: DisplayList,
    pub height: f32,
    pub links: Vec<Link>,
    pub anchors: HashMap<String, f32>,
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

pub fn layout(
    document: &Document,
    width: f32,
    text: &mut impl TextMeasurer,
    resolve_image: impl Fn(&str) -> Option<ImageInfo>,
) -> Result<LaidOutPage, String> {
    layout_with_stylesheets(
        document,
        Viewport {
            width,
            height: 720.0,
        },
        &HashMap::new(),
        text,
        resolve_image,
    )
}

pub fn layout_with_stylesheets(
    document: &Document,
    viewport: Viewport,
    external_stylesheets: &HashMap<String, String>,
    text: &mut impl TextMeasurer,
    resolve_image: impl Fn(&str) -> Option<ImageInfo>,
) -> Result<LaidOutPage, String> {
    let styles = compute_styles(document, viewport, external_stylesheets)?;
    let canvas = ["html", "body"].into_iter().find_map(|tag| {
        document.tree_order().find(|id| {
            document.nodes[*id].name() == tag
                && styles[*id].visible
                && styles[*id].display != Display::None
                && styles[*id].background.alpha > 0.0
        })
    });
    let background = canvas.map_or(Color::rgb(0xffffff), |id| styles[id].background);
    let mut engine = Layout {
        document,
        styles,
        text,
        resolve_image,
        viewport,
        canvas,
        intrinsic_widths: vec![None; document.nodes.len()],
        commands: Vec::new(),
        links: Vec::new(),
        anchors: HashMap::new(),
    };
    let flow_height = engine.children(
        0,
        0.0,
        0.0,
        ContainingBlock {
            width: viewport.width.clamp(1.0, 16_384.0),
            height: Some(viewport.height),
        },
        None,
    )?;
    let height = flow_height.max(painted_height(&engine.commands));
    if !(0.0..=MAX_HEIGHT).contains(&height) {
        return Err("The document exceeds the maximum layout height.".into());
    }
    Ok(LaidOutPage {
        display_list: DisplayList {
            background,
            commands: engine.commands,
        },
        height,
        links: engine.links,
        anchors: engine.anchors,
    })
}

fn painted_height(commands: &[Command]) -> f32 {
    let mut height: f32 = 0.0;
    let mut clips: Vec<Option<Rect>> = Vec::new();
    for command in commands {
        let bounds = match *command {
            Command::PushClip { rect } => {
                clips.push(match clips.last() {
                    Some(parent) => parent.and_then(|parent| parent.intersection(rect)),
                    None => Some(rect),
                });
                continue;
            }
            Command::PopClip => {
                clips.pop();
                continue;
            }
            Command::Fill { rect, .. } | Command::Image { rect, .. } => rect,
            Command::Text { bounds, .. } => bounds,
        };
        let visible = match clips.last() {
            Some(clip) => clip.and_then(|clip| clip.intersection(bounds)),
            None => Some(bounds),
        };
        if let Some(rect) = visible {
            height = height.max(rect.y + rect.height);
        }
    }
    height
}

fn paint(color: Color, opacity: f32) -> Color {
    if opacity >= 1.0 {
        color
    } else {
        Color {
            alpha: (color.alpha * opacity).clamp(0.0, 1.0),
            ..color
        }
    }
}

struct InlineText<'a> {
    text: String,
    utf16: u32,
    links: Vec<(&'a str, TextRange)>,
    runs: Vec<TextRun>,
    space: Option<(Option<&'a str>, Style)>,
}

impl<'a> InlineText<'a> {
    fn new() -> Self {
        Self {
            text: String::new(),
            utf16: 0,
            links: Vec::new(),
            runs: Vec::new(),
            space: None,
        }
    }

    fn push(&mut self, character: char, href: Option<&'a str>, style: Style) -> Result<(), String> {
        let length = character.len_utf16() as u32;
        if self.utf16 + length > MAX_TEXT_UNITS {
            return Err("A text block exceeds the text layout limit.".into());
        }
        if let Some(href) = href.filter(|_| style.visible) {
            if let Some((last_href, range)) = self.links.last_mut()
                && *last_href == href
                && range.start + range.length == self.utf16
            {
                range.length += length;
            } else {
                self.links.push((
                    href,
                    TextRange {
                        start: self.utf16,
                        length,
                    },
                ));
            }
        }
        let run = TextRun {
            range: TextRange {
                start: self.utf16,
                length,
            },
            color: paint(style.color, if style.visible { style.opacity } else { 0.0 }),
            size: style.text.size,
            weight: style.text.weight,
            family: style.family,
            italic: u32::from(style.italic),
            underline: u32::from(style.underline),
            strikethrough: u32::from(style.strikethrough),
        };
        match self.runs.last_mut() {
            Some(previous)
                if (TextRun {
                    range: previous.range,
                    ..run
                }) == *previous =>
            {
                previous.range.length += length;
            }
            _ => self.runs.push(run),
        }
        self.text.push(character);
        self.utf16 += length;
        Ok(())
    }

    fn append(&mut self, text: &str, href: Option<&'a str>, style: Style) -> Result<(), String> {
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            let character = match character {
                '\r' => {
                    characters.next_if_eq(&'\n');
                    '\n'
                }
                '\u{000c}' => '\n',
                '\0' => '\u{fffd}',
                character => character,
            };
            if character == '\n' && style.white_space.preserve_breaks() {
                self.space = None;
                self.push('\n', href, style)?;
                continue;
            }
            if matches!(character, ' ' | '\t' | '\n') && !style.white_space.preserve_spaces() {
                if self.space.is_none() && !self.text.is_empty() && !self.text.ends_with('\n') {
                    self.space = Some((href, style));
                }
                continue;
            }
            if let Some((space_href, space_style)) = self.space.take() {
                self.push(' ', space_href, space_style)?;
            }
            self.push(character, href, style)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ContainingBlock {
    width: f32,
    height: Option<f32>,
}

struct FlexItem {
    id: NodeId,
    width: f32,
    margin: f32,
    min_width: f32,
    max_width: f32,
}

struct Layout<'a, T, I> {
    document: &'a Document,
    styles: Vec<Style>,
    text: &'a mut T,
    resolve_image: I,
    viewport: Viewport,
    canvas: Option<NodeId>,
    intrinsic_widths: Vec<Option<f32>>,
    commands: Vec<Command>,
    links: Vec<Link>,
    anchors: HashMap<String, f32>,
}

impl<'a, T: TextMeasurer, I: Fn(&str) -> Option<ImageInfo>> Layout<'a, T, I> {
    fn children(
        &mut self,
        id: NodeId,
        x: f32,
        mut y: f32,
        containing: ContainingBlock,
        href: Option<&'a str>,
    ) -> Result<f32, String> {
        let style = self.styles[id];
        if style.display == Display::Flex && !style.flex_column {
            return self.flex_row(id, x, y, containing, href);
        }
        if style.display == Display::Grid {
            return self.grid(id, x, y, containing, href);
        }
        let width = containing.width;
        let origin_y = y;
        let start = y;
        let spacing = if style.display == Display::Flex {
            style.gap[0]
                .resolve(width, self.viewport, style.text.size)
                .unwrap_or(0.0)
                .max(0.0)
        } else {
            0.0
        };
        let mut flowed = false;
        let mut inline = InlineText::new();
        for child in &self.document.nodes[id].children {
            if self.styles[*child].position == Position::Absolute {
                if self.styles[*child].display != Display::None {
                    y += self.flush(&mut inline, style, x, y, width)?;
                    self.place_absolute(*child, x, origin_y, y, containing, href)?;
                }
                continue;
            }
            let node = &self.document.nodes[*child];
            let display = self.styles[*child].display;
            let has_block_content = node.name() == "img"
                || style.display == Display::Flex && matches!(&node.kind, Kind::Element { .. })
                || node
                    .children
                    .iter()
                    .any(|id| self.styles[*id].display.is_block())
                || self.document.clickable.contains(child);
            match if display == Display::Inline && has_block_content {
                Display::Block
            } else {
                display
            } {
                Display::None => {}
                Display::Block | Display::Flex | Display::Grid => {
                    y += self.flush(&mut inline, style, x, y, width)?;
                    if flowed {
                        y += spacing;
                    }
                    y += self.block(*child, x, y, containing, None, href)?;
                    flowed = true;
                }
                Display::Inline => self.collect(*child, &mut inline, href, y)?,
            }
            if y > MAX_HEIGHT {
                return Err("The document exceeds the maximum layout height.".into());
            }
        }
        y += self.flush(&mut inline, style, x, y, width)?;
        Ok(y - start)
    }

    fn grid(
        &mut self,
        id: NodeId,
        x: f32,
        mut y: f32,
        containing: ContainingBlock,
        href: Option<&'a str>,
    ) -> Result<f32, String> {
        let style = self.styles[id];
        let resolve = |length: Length| {
            length
                .resolve(containing.width, self.viewport, style.text.size)
                .unwrap_or_default()
        };
        let row_gap = resolve(style.gap[0]);
        let column_gap = resolve(style.gap[1]);
        let tracks = style.grid_columns.tracks();
        let (fixed, fractions) =
            tracks
                .iter()
                .fold((0.0, 0.0), |(fixed, fractions), track| match *track {
                    GridTrack::Length(length) => (fixed + resolve(length), fractions),
                    GridTrack::Fraction(value) => (fixed, fractions + value),
                });
        let free = (containing.width - fixed - column_gap * (tracks.len() - 1) as f32).max(0.0);
        let fraction = free / fractions.max(1.0);
        let widths: Vec<_> = tracks
            .iter()
            .map(|track| match *track {
                GridTrack::Length(length) => resolve(length),
                GridTrack::Fraction(value) => value * fraction,
            })
            .collect();
        let top = y;
        let mut cell_x = x;
        let mut row_height: f32 = 0.0;
        let mut column = 0;
        for &child in &self.document.nodes[id].children {
            let child_style = self.styles[child];
            let node = &self.document.nodes[child];
            if child_style.display == Display::None
                || matches!(&node.kind, Kind::StreamMarker(_))
                || matches!(&node.kind, Kind::Text(text) if text.trim().is_empty())
            {
                continue;
            }
            if child_style.position == Position::Absolute {
                self.place_absolute(child, x, top, y, containing, href)?;
                continue;
            }
            if column == widths.len() {
                column = 0;
                y += row_height + row_gap;
                row_height = 0.0;
                cell_x = x;
            }
            let width = widths[column].max(1.0);
            let height = if let Kind::Text(text) = &node.kind {
                let mut inline = InlineText::new();
                inline.append(text, href, child_style)?;
                self.flush(&mut inline, child_style, cell_x, y, width)?
            } else {
                self.block(
                    child,
                    cell_x,
                    y,
                    ContainingBlock {
                        width,
                        height: None,
                    },
                    None,
                    href,
                )?
            };
            row_height = row_height.max(height);
            cell_x += widths[column] + column_gap;
            column += 1;
        }
        Ok(y + row_height - top)
    }

    fn intrinsic_width(&mut self, id: NodeId) -> Result<f32, String> {
        self.outer_width(id, false)
    }

    /// Border-box size of the content, ignoring a specified width.
    /// Flex `min-width: auto` uses this so items cannot shrink below their text.
    fn content_width(&mut self, id: NodeId) -> Result<f32, String> {
        self.outer_width(id, true)
    }

    fn outer_width(&mut self, id: NodeId, content_only: bool) -> Result<f32, String> {
        if !content_only && let Some(width) = self.intrinsic_widths[id] {
            return Ok(width);
        }
        let node = &self.document.nodes[id];
        let style = self.styles[id];
        let viewport = self.viewport;
        let resolve = |length: Length| match length {
            Length::Percent(_) => None,
            _ => length.resolve(0.0, viewport, style.text.size),
        };
        let width = if let Kind::Text(text) = &node.kind {
            let mut inline = InlineText::new();
            inline.append(text, None, style)?;
            if inline.text.is_empty() {
                0.0
            } else {
                self.text
                    .measure_styled(&inline.text, style.text, 16_384.0, &[], &inline.runs, false)?
                    .width
            }
        } else {
            let padding = resolve(style.padding[1]).unwrap_or_default()
                + resolve(style.padding[3]).unwrap_or_default();
            let border = style.border.max(0.0) * 2.0;
            let specified = if content_only {
                None
            } else {
                resolve(style.width)
            };
            let width = if let Some(width) = specified {
                width
                    + if style.border_box {
                        0.0
                    } else {
                        padding + border
                    }
            } else if node.name() == "img" {
                node.attr("src")
                    .and_then(&self.resolve_image)
                    .map_or(0.0, |image| {
                        resolve(style.height).map_or(image.width as f32, |height| {
                            height * image.width as f32 / image.height as f32
                        })
                    })
                    + padding
                    + border
            } else {
                let row = style.display == Display::Flex && !style.flex_column;
                let mut widest: f32 = 0.0;
                let mut line: f32 = 0.0;
                for child in &node.children {
                    let display = self.styles[*child].display;
                    if display == Display::None
                        || self.styles[*child].position == Position::Absolute
                    {
                        continue;
                    }
                    let width = self.intrinsic_width(*child)?;
                    if !row && display.is_block() {
                        widest = widest.max(line).max(width);
                        line = 0.0;
                    } else {
                        line += width;
                    }
                }
                widest.max(line) + padding + border
            };
            let limit = if content_only {
                16_384.0
            } else {
                resolve(style.max_width)
                    .map(|maximum| {
                        maximum
                            + if style.border_box {
                                0.0
                            } else {
                                padding + border
                            }
                    })
                    .unwrap_or(16_384.0)
            };
            width.min(limit)
        };
        if !content_only {
            self.intrinsic_widths[id] = Some(width);
        }
        Ok(width)
    }

    fn flex_row(
        &mut self,
        id: NodeId,
        mut x: f32,
        y: f32,
        containing: ContainingBlock,
        href: Option<&'a str>,
    ) -> Result<f32, String> {
        let origin_x = x;
        let mut items = Vec::new();
        let mut absolute = Vec::new();
        for child in &self.document.nodes[id].children {
            let style = self.styles[*child];
            if style.position == Position::Absolute {
                if style.display != Display::None {
                    absolute.push(*child);
                }
                continue;
            }
            if style.display == Display::None
                || matches!(&self.document.nodes[*child].kind, Kind::StreamMarker(_))
                || matches!(&self.document.nodes[*child].kind, Kind::Text(text) if text.trim().is_empty())
            {
                continue;
            }
            let font = style.text.size;
            let viewport = self.viewport;
            let available = containing.width;
            let resolve = move |length: Length| length.resolve(available, viewport, font);
            let margin = resolve(style.margin[1]).unwrap_or_default()
                + resolve(style.margin[3]).unwrap_or_default();
            let padding = resolve(style.padding[1]).unwrap_or_default()
                + resolve(style.padding[3]).unwrap_or_default();
            let border = style.border.max(0.0) * 2.0;
            let extra = if style.border_box {
                0.0
            } else {
                padding + border
            };
            let content = self.content_width(*child)?;
            let preferred = resolve(style.width)
                .map(|width| width + extra)
                .unwrap_or(content);
            let min_width = match style.min_width {
                Length::Auto if style.overflow_clip => 0.0,
                Length::Auto if resolve(style.width).is_some() => content.min(preferred),
                Length::Auto => content,
                length => resolve(length).unwrap_or(0.0) + extra,
            };
            let max_width = resolve(style.max_width)
                .map_or(16_384.0, |width| width + extra)
                .max(min_width);
            let width = preferred;
            items.push(FlexItem {
                id: *child,
                width: width.clamp(min_width, max_width),
                margin,
                min_width,
                max_width,
            });
        }
        let spacing = self.styles[id].gap[1]
            .resolve(containing.width, self.viewport, self.styles[id].text.size)
            .unwrap_or(0.0)
            .max(0.0);
        let spacing_total = spacing * items.len().saturating_sub(1) as f32;
        let remaining = containing.width
            - items
                .iter()
                .map(|item| item.width + item.margin)
                .sum::<f32>()
            - spacing_total;
        let weight = |item: &FlexItem| {
            let style = self.styles[item.id];
            if remaining >= 0.0 {
                style.flex_grow
            } else {
                style.flex_shrink * item.width
            }
        };
        let total: f32 = items.iter().map(weight).sum();
        if total > 0.0 {
            for item in &mut items {
                item.width = (item.width + remaining * weight(item) / total)
                    .clamp(item.min_width, item.max_width);
            }
        }
        let remaining = (containing.width
            - items
                .iter()
                .map(|item| item.width + item.margin)
                .sum::<f32>())
        .max(0.0);
        let between = match self.styles[id].justify_content {
            JustifyContent::Start => 0.0,
            JustifyContent::Center => {
                x += remaining / 2.0;
                0.0
            }
            JustifyContent::End => {
                x += remaining;
                0.0
            }
            JustifyContent::SpaceBetween => remaining / items.len().saturating_sub(1).max(1) as f32,
        };
        let mut height: f32 = 0.0;
        for item in items {
            let command_from = self.commands.len();
            let link_from = self.links.len();
            let item_height = if let Kind::Text(text) = &self.document.nodes[item.id].kind {
                let mut inline = InlineText::new();
                inline.append(text, href, self.styles[item.id])?;
                self.flush(&mut inline, self.styles[item.id], x, y, item.width)?
            } else {
                self.block(item.id, x, y, containing, Some(item.width), href)?
            };
            let shift = match (self.styles[id].align, containing.height) {
                (AlignItems::Center, Some(available)) => ((available - item_height) / 2.0).max(0.0),
                (AlignItems::End, Some(available)) => (available - item_height).max(0.0),
                _ => 0.0,
            };
            if shift > 0.0 {
                self.shift(command_from, link_from, shift, y, item_height);
            }
            height = height.max(item_height + shift);
            x += item.width + item.margin + between + spacing;
        }
        for child in absolute {
            self.place_absolute(child, origin_x, y, y, containing, href)?;
        }
        Ok(height)
    }

    fn place_absolute(
        &mut self,
        id: NodeId,
        origin_x: f32,
        origin_y: f32,
        static_y: f32,
        containing: ContainingBlock,
        href: Option<&'a str>,
    ) -> Result<(), String> {
        let style = self.styles[id];
        let width = containing.width;
        let height = containing.height.unwrap_or(self.viewport.height);
        let left = style.inset[3].resolve(width, self.viewport, style.text.size);
        let right = style.inset[1].resolve(width, self.viewport, style.text.size);
        let top = style.inset[0].resolve(height, self.viewport, style.text.size);
        let x = origin_x + left.unwrap_or(0.0);
        let y = origin_y + top.unwrap_or(static_y - origin_y);
        let flex_width = if left.is_some() && right.is_some() && matches!(style.width, Length::Auto)
        {
            Some((width - left.unwrap_or(0.0) - right.unwrap_or(0.0)).max(1.0))
        } else {
            None
        };
        self.block(id, x, y, containing, flex_width, href)?;
        Ok(())
    }

    fn shift(&mut self, command_from: usize, link_from: usize, dy: f32, top: f32, height: f32) {
        for command in &mut self.commands[command_from..] {
            match command {
                Command::Fill { rect, .. }
                | Command::Image { rect, .. }
                | Command::PushClip { rect } => rect.y += dy,
                Command::Text { bounds, .. } => bounds.y += dy,
                Command::PopClip => {}
            }
        }
        for link in &mut self.links[link_from..] {
            link.rect.y += dy;
        }
        for anchor in self.anchors.values_mut() {
            if *anchor >= top - 0.1 && *anchor < top + height + 0.1 {
                *anchor += dy;
            }
        }
    }

    fn collect(
        &mut self,
        id: NodeId,
        inline: &mut InlineText<'a>,
        parent_link: Option<&'a str>,
        y: f32,
    ) -> Result<(), String> {
        let style = self.styles[id];
        if style.display == Display::None {
            return Ok(());
        }
        let node = &self.document.nodes[id];
        if let Kind::Text(text) = &node.kind {
            return inline.append(text, parent_link, style);
        }
        if let Some(anchor) = node.attr("id") {
            self.anchors.entry(anchor.to_owned()).or_insert(y.max(0.0));
        }
        if node.name() == "br" {
            inline.space = None;
            return inline.push('\n', None, style);
        }
        let href = if node.name() == "a" {
            node.attr("href")
        } else {
            parent_link
        };
        for child in &node.children {
            self.collect(*child, inline, href, y)?;
        }
        Ok(())
    }

    fn flush(
        &mut self,
        inline: &mut InlineText<'a>,
        style: Style,
        x: f32,
        y: f32,
        width: f32,
    ) -> Result<f32, String> {
        if inline.text.is_empty() {
            return Ok(0.0);
        }
        let ranges: Vec<_> = inline.links.iter().map(|(_, range)| *range).collect();
        let text = self.text.measure_styled(
            &inline.text,
            style.text,
            width,
            &ranges,
            &inline.runs,
            style.white_space.wrap(),
        )?;
        for (index, rect) in text.links {
            if let Some((href, _)) = inline.links.get(index) {
                self.links.push(Link {
                    rect: rect.translated(x, y),
                    href: (*href).to_owned(),
                });
            }
        }
        self.commands.push(Command::Text {
            bounds: Rect::new(x, y, width.max(text.width), text.height),
            color: paint(style.color, if style.visible { style.opacity } else { 0.0 }),
            layout: text.layout,
        });
        *inline = InlineText::new();
        Ok(text.height)
    }

    fn block(
        &mut self,
        id: NodeId,
        x: f32,
        y: f32,
        containing: ContainingBlock,
        flex_width: Option<f32>,
        parent_link: Option<&'a str>,
    ) -> Result<f32, String> {
        let node = &self.document.nodes[id];
        let style = self.styles[id];
        let (x, y) = if style.position == Position::Relative {
            let dx = style.inset[3]
                .resolve(containing.width, self.viewport, style.text.size)
                .unwrap_or_else(|| {
                    style.inset[1]
                        .resolve(containing.width, self.viewport, style.text.size)
                        .map(|value| -value)
                        .unwrap_or(0.0)
                });
            let basis = containing.height.unwrap_or(0.0);
            let dy = style.inset[0]
                .resolve(basis, self.viewport, style.text.size)
                .unwrap_or_else(|| {
                    style.inset[2]
                        .resolve(basis, self.viewport, style.text.size)
                        .map(|value| -value)
                        .unwrap_or(0.0)
                });
            (x + dx, y + dy)
        } else {
            (x, y)
        };
        let available = containing.width;
        let resolve = |length: Length| length.resolve(available, self.viewport, style.text.size);
        let [top, right, bottom, left] = style.margin.map(resolve);
        let border = style.border.clamp(0.0, 64.0);
        let [pad_top, pad_right, pad_bottom, pad_left] = style
            .padding
            .map(|side| resolve(side).unwrap_or(0.0) + border);
        let viewport = self.viewport;
        let resolve_height = |length: Length| {
            let basis = if matches!(length, Length::Percent(_)) {
                containing.height?
            } else {
                0.0
            };
            length
                .resolve(basis, viewport, style.text.size)
                .map(|height| {
                    (height
                        - if style.border_box {
                            pad_top + pad_bottom
                        } else {
                            0.0
                        })
                    .max(0.0)
                })
        };
        let specified_height = resolve_height(style.height);
        let fixed_margins = left.unwrap_or_default() + right.unwrap_or_default();
        let remaining = (available - fixed_margins - pad_left - pad_right).max(1.0);
        let adjustment = if style.border_box {
            pad_left + pad_right
        } else {
            0.0
        };
        let specified_width = flex_width
            .map(|width| width - pad_left - pad_right)
            .or_else(|| resolve(style.width).map(|value| value - adjustment));
        let image = (node.name() == "img")
            .then(|| node.attr("src").and_then(&self.resolve_image))
            .flatten();
        let auto_width = image.map_or(remaining, |image| {
            specified_height
                .map_or(image.width as f32, |height| {
                    height * image.width as f32 / image.height as f32
                })
                .min(remaining)
        });
        let width = specified_width
            .unwrap_or(auto_width)
            .min(
                resolve(style.max_width)
                    .map(|value| value - adjustment)
                    .unwrap_or(f32::MAX),
            )
            .max(resolve(style.min_width).map_or(0.0, |value| value - adjustment))
            .clamp(1.0, 16_384.0);
        let border_width = width + pad_left + pad_right;
        let auto = if flex_width.is_some() {
            0.0
        } else {
            (available - fixed_margins - border_width).max(0.0)
                / (u8::from(left.is_none()) + u8::from(right.is_none())).max(1) as f32
        };
        let x = x + left.unwrap_or(auto);
        let box_y = y + top.unwrap_or_default();
        if let Some(anchor) = node.attr("id") {
            self.anchors
                .entry(anchor.to_owned())
                .or_insert(box_y.max(0.0));
        }
        let background = if style.visible && style.background.alpha > 0.0 && self.canvas != Some(id)
        {
            let index = self.commands.len();
            self.commands.push(Command::Fill {
                rect: Rect::default(),
                color: paint(style.background, style.opacity),
                radius: style.radius,
            });
            Some(index)
        } else {
            None
        };
        let links_start = self.links.len();
        let clip = style.overflow_clip.then(|| {
            let index = self.commands.len();
            self.commands.push(Command::PushClip {
                rect: Rect::default(),
            });
            index
        });
        let content_x = x + pad_left;
        let content_y = box_y + pad_top;
        let href = if node.name() == "a" {
            node.attr("href")
        } else {
            parent_link
        };
        let child_commands = self.commands.len();
        let child_links = self.links.len();
        let marker_height = if let Some(marker) = style.list_marker {
            let label = match marker {
                ListMarker::Bullet => "•".to_owned(),
                ListMarker::Ordinal(number) => format!("{number}."),
            };
            let mut marker_style = style;
            marker_style.text.align = TextAlign::End;
            let marker_width = (style.text.size * 7.0).min(16_384.0);
            let mut inline = InlineText::new();
            inline.append(&label, None, marker_style)?;
            self.flush(
                &mut inline,
                marker_style,
                content_x - marker_width - 6.0,
                content_y,
                marker_width,
            )?
        } else {
            0.0
        };
        let content_height = if node.name() == "img" {
            if let Some(image) = image {
                let height =
                    specified_height.unwrap_or(width * image.height as f32 / image.width as f32);
                if style.visible {
                    self.commands.push(Command::Image {
                        rect: Rect::new(content_x, content_y, width, height),
                        image: image.id,
                    });
                }
                height
            } else {
                let mut alternative = InlineText::new();
                alternative.append(node.attr("alt").unwrap_or(""), href, style)?;
                self.flush(&mut alternative, style, content_x, content_y, width)?
            }
        } else if node.name() == "input" {
            let mut label = InlineText::new();
            let kind = node.attr("type").unwrap_or("text").to_ascii_lowercase();
            let value = node.attr("value").unwrap_or("");
            let placeholder = node.attr("placeholder").unwrap_or("");
            let text = match kind.as_str() {
                "password" if !value.is_empty() => "••••",
                "checkbox" if node.attr("checked").is_some() => "☑",
                "checkbox" => "☐",
                "radio" if node.attr("checked").is_some() => "◉",
                "radio" => "○",
                "submit" if value.is_empty() => "Submit",
                _ if value.is_empty() => placeholder,
                _ => value,
            };
            label.append(text, href, style)?;
            self.flush(&mut label, style, content_x, content_y, width)?
                .max(style.text.size)
        } else if node.name() == "select" {
            let mut options = node
                .children
                .iter()
                .flat_map(|id| {
                    let child = &self.document.nodes[*id];
                    if child.name() == "optgroup" {
                        child.children.as_slice()
                    } else {
                        std::slice::from_ref(id)
                    }
                })
                .map(|id| &self.document.nodes[*id])
                .filter(|node| node.name() == "option");
            let option = options
                .clone()
                .find(|node| node.attr("selected").is_some())
                .or_else(|| options.next());
            let mut label = InlineText::new();
            if let Some(option) = option {
                if let Some(value) = option.attr("label") {
                    label.append(value, href, style)?;
                } else {
                    for &id in &option.children {
                        if let Kind::Text(text) = &self.document.nodes[id].kind {
                            label.append(text, href, style)?;
                        }
                    }
                }
            }
            self.flush(&mut label, style, content_x, content_y, width)?
                .max(style.text.size)
        } else {
            self.children(
                id,
                content_x,
                content_y,
                ContainingBlock {
                    width,
                    height: specified_height,
                },
                href,
            )?
        };
        let content_height = content_height.max(marker_height);
        let used_height = specified_height
            .unwrap_or(content_height)
            .max(resolve_height(style.min_height).unwrap_or(0.0));
        let extra = (used_height - content_height).max(0.0);
        if specified_height.is_none()
            && extra > 0.0
            && style.display == Display::Flex
            && !style.flex_column
        {
            let dy = match style.align {
                AlignItems::Center => extra / 2.0,
                AlignItems::End => extra,
                AlignItems::Start => 0.0,
            };
            if dy > 0.0 {
                self.shift(child_commands, child_links, dy, content_y, content_height);
            }
        }
        let box_height = pad_top + used_height + pad_bottom;
        let rect = Rect::new(x, box_y, border_width, box_height);
        if let Some(index) = background {
            self.commands[index] = Command::Fill {
                rect,
                color: paint(style.background, style.opacity),
                radius: style.radius,
            };
        }
        if style.visible && border > 0.0 && style.border_color.alpha > 0.0 {
            let color = paint(style.border_color, style.opacity);
            let bottom = rect.y + rect.height - border;
            let right = rect.x + rect.width - border;
            self.commands.extend([
                Command::Fill {
                    rect: Rect::new(rect.x, rect.y, rect.width, border),
                    color,
                    radius: 0.0,
                },
                Command::Fill {
                    rect: Rect::new(rect.x, bottom, rect.width, border),
                    color,
                    radius: 0.0,
                },
                Command::Fill {
                    rect: Rect::new(
                        rect.x,
                        rect.y + border,
                        border,
                        (rect.height - border * 2.0).max(0.0),
                    ),
                    color,
                    radius: 0.0,
                },
                Command::Fill {
                    rect: Rect::new(
                        right,
                        rect.y + border,
                        border,
                        (rect.height - border * 2.0).max(0.0),
                    ),
                    color,
                    radius: 0.0,
                },
            ]);
        }
        if let Some(index) = clip {
            self.commands[index] = Command::PushClip { rect };
            self.commands.push(Command::PopClip);
            let visible: Vec<_> = self
                .links
                .drain(links_start..)
                .filter_map(|mut link| {
                    link.rect = link.rect.intersection(rect)?;
                    Some(link)
                })
                .collect();
            self.links.extend(visible);
        }
        if style.visible
            && node.name() == "a"
            && let Some(href) = href
        {
            self.links.push(Link {
                rect,
                href: href.to_owned(),
            });
        }
        if style.visible && self.document.clickable.contains(&id) {
            self.links.push(Link {
                rect,
                href: format!("ko4:click:{id}"),
            });
        }
        Ok(top.unwrap_or_default() + box_height + bottom.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_html;
    use display_list::{TextMetrics, TextStyle};

    struct FixedText;
    impl TextMeasurer for FixedText {
        fn measure(
            &mut self,
            text: &str,
            style: TextStyle,
            _width: f32,
            ranges: &[TextRange],
        ) -> Result<TextMetrics, String> {
            assert!(!text.contains("blocked"));
            Ok(TextMetrics {
                layout: 0,
                width: text.chars().count() as f32 * style.size / 2.0,
                height: style.size + 4.0,
                links: ranges
                    .iter()
                    .enumerate()
                    .map(|(index, range)| {
                        (
                            index,
                            Rect::new(range.start as f32, 0.0, range.length as f32, 20.0),
                        )
                    })
                    .collect(),
            })
        }
    }

    #[test]
    fn lays_out_cascade_background_and_utf16_links() {
        let page = parse_html(r##"
            <style>
              body { background: #202030; margin: 0 }
              #box { color: red; margin: 0 auto; max-width: 80px; padding: 10px }
              p { margin: 0; font-size: 18px }
              .override { color: blue }
            </style>
            <body><p id="box" class="override">🧭 <a href="#box">Мир</a></p>
            <script><p>blocked</p></script><img src="https://example.org/x.png" alt="Alternative"></body>
        "##).unwrap();
        let layout = layout(&page, 200.0, &mut FixedText, |_| None).unwrap();
        assert_eq!(layout.display_list.background, Color::rgb(0x202030));
        assert!(
            matches!(layout.display_list.commands[0], Command::Text { bounds, color, .. }
            if bounds == Rect::new(60.0, 10.0, 80.0, 22.0) && color == Color::rgb(0xff0000))
        );
        assert_eq!(layout.links[0].rect.x, 63.0);
        assert_eq!(layout.links[0].href, "#box");
        assert_eq!(layout.anchors["box"], 0.0);
        assert!(
            !layout
                .display_list
                .commands
                .iter()
                .any(|command| matches!(command, Command::Image { .. }))
        );
    }

    #[test]
    fn applies_network_styles_and_keeps_overflow_and_following_blocks_separate() {
        let page = parse_html(
            r#"
            <link rel="stylesheet" href="/style.css">
            <template><link rel="stylesheet" href="/ignored.css"></template>
            <body><main class="article"><div class="short"><p>Headline</p></div>
            <div class="crop"><p><a href="/cropped">Clipped link</a></p></div>
            <p id="tail">Following text <span class="sr">blocked</span></p></main></body>
        "#,
        )
        .unwrap();
        assert_eq!(
            page.external_stylesheets().collect::<Vec<_>>(),
            ["/style.css"]
        );
        let styles = HashMap::from([(
            "/style.css".into(),
            r#"
            body { margin: 0; background: rgb(235, 242, 250) }
            main.article > div { height: 1px; }
            main.article p { margin: 0; color: blue; }
            .crop { overflow: hidden; }
            .sr { width: 1px; height: 1px; overflow: hidden; }
            @media (max-width: 500px) { main.article p { font-size: 20px; } }
        "#
            .into(),
        )]);
        let result = layout_with_stylesheets(
            &page,
            Viewport {
                width: 420.0,
                height: 680.0,
            },
            &styles,
            &mut FixedText,
            |_| None,
        )
        .unwrap();
        assert_eq!(result.display_list.background, Color::rgb(0xebf2fa));
        assert_eq!(result.anchors["tail"], 2.0);
        assert!(result.display_list.commands.iter().any(|command| matches!(
            command, Command::PushClip { rect } if rect.height == 1.0
        )));
        assert_eq!(result.links[0].rect.height, 1.0);
    }

    #[test]
    fn places_grid_and_table_cells_in_columns() {
        let page = parse_html(
            r#"<body style="margin:0">
                <div style="display:grid;grid-template-columns:repeat(2, 1fr) 50px;gap:4px 10px">
                  <div style="background:red;height:10px"></div>
                  <div style="background:blue;height:20px"></div>
                  <div style="background:green;height:5px"></div>
                  <div id="second" style="height:12px"></div>
                </div>
                <table><tr><th>A<th>B<tr><td>C<td>D</table>
                <div id="tail">After the table</div>
            </body>"#,
        )
        .unwrap();
        let result = layout(&page, 250.0, &mut FixedText, |_| None).unwrap();
        let fill: Vec<_> = result
            .display_list
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Fill { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(
            fill,
            [
                Rect::new(0.0, 0.0, 90.0, 10.0),
                Rect::new(100.0, 0.0, 90.0, 20.0),
                Rect::new(200.0, 0.0, 50.0, 5.0),
            ]
        );
        assert_eq!(result.anchors["second"], 24.0);
        let text: Vec<_> = result
            .display_list
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Text { bounds, .. } => Some((bounds.x, bounds.y)),
                _ => None,
            })
            .collect();
        assert_eq!(
            text,
            [
                (0.0, 36.0),
                (126.0, 36.0),
                (0.0, 56.0),
                (126.0, 56.0),
                (0.0, 76.0)
            ]
        );
    }

    #[test]
    fn preserves_image_ratio_when_css_resets_its_html_height() {
        let page = parse_html(
            r#"<body style="margin:0">
                <img src="sample.svg" width="200" height="200" style="width:50%;height:auto">
                <img src="sample.svg" style="height:30px">
            </body>"#,
        )
        .unwrap();
        let result = layout(&page, 200.0, &mut FixedText, |_| {
            Some(ImageInfo {
                id: 0,
                width: 80,
                height: 40,
            })
        })
        .unwrap();
        let images: Vec<_> = result
            .display_list
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Image { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(
            images,
            [
                Rect::new(0.0, 0.0, 100.0, 50.0),
                Rect::new(0.0, 50.0, 60.0, 30.0)
            ]
        );
    }

    #[test]
    fn preserves_preformatted_whitespace_and_inline_utf16_ranges() {
        struct Recorder;
        impl TextMeasurer for Recorder {
            fn measure(
                &mut self,
                text: &str,
                style: TextStyle,
                width: f32,
                links: &[TextRange],
            ) -> Result<TextMetrics, String> {
                FixedText.measure(text, style, width, links)
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
                assert_eq!(text, "  🧭\n  line");
                assert!(!wrap);
                assert_eq!(runs[0].family, display_list::FontFamily::Monospace);
                assert_eq!(
                    runs[1].range,
                    TextRange {
                        start: 2,
                        length: 2
                    }
                );
                assert_eq!(runs[1].weight, 700);
                self.measure(text, style, width, links)
            }
        }
        let page = parse_html("<pre>\r\n  <b>🧭</b>\r\n  line</pre>").unwrap();
        layout(&page, 150.0, &mut Recorder, |_| None).unwrap();
    }

    #[test]
    fn lays_out_a_flex_row_and_resolves_percent_height_against_a_definite_parent() {
        let page = parse_html(
            r#"
            <body style="margin:0">
              <div><div style="height:100%">Before row</div></div>
              <div style="display:flex;height:80px">
                <div style="width:60px;height:20px;background:red"></div>
                <div style="flex:1 1 auto;height:50%;background:blue"></div>
              </div>
              <div id="tail">After row</div>
            </body>
        "#,
        )
        .unwrap();
        let result = layout(&page, 300.0, &mut FixedText, |_| None).unwrap();
        assert_eq!(result.anchors["tail"], 100.0);
        assert!(result.display_list.commands.iter().any(|command| matches!(
            command, Command::Fill { rect, color, .. }
            if *rect == Rect::new(60.0, 20.0, 240.0, 40.0) && *color == Color::rgb(0x0000ff)
        )));
    }

    #[test]
    fn visually_hidden_text_does_not_stretch_its_link() {
        struct Wrap;
        impl TextMeasurer for Wrap {
            fn measure(
                &mut self,
                text: &str,
                style: TextStyle,
                width: f32,
                _: &[TextRange],
            ) -> Result<TextMetrics, String> {
                let full = text.chars().count() as f32 * style.size / 2.0;
                let lines = if width >= full || text.is_empty() {
                    1.0
                } else {
                    (full / width.max(1.0)).ceil()
                };
                Ok(TextMetrics {
                    layout: 0,
                    width: full.min(width),
                    height: lines * (style.size + 4.0),
                    links: Vec::new(),
                })
            }
        }
        let page = parse_html(
            r#"
            <html><style>
              .visuallyHidden_k{border:0px;clip:rect(0,0,0,0);clip-path:inset(50%);height:1px;overflow:hidden;position:absolute;white-space:nowrap;width:1px}
              a{display:flex;align-items:center;min-height:48px;padding:6px}
            </style><body style="margin:0">
              <a href="/a"><div>About</div><div class="visuallyHidden_k">; Opens a new tab</div></a>
            </body></html>
        "#,
        )
        .unwrap();
        let result = layout(&page, 400.0, &mut Wrap, |_| None).unwrap();
        let link = result
            .links
            .iter()
            .find(|link| link.rect.height > 10.0)
            .unwrap();
        assert!(link.rect.height < 80.0, "{:?}", link.rect);
        assert!(
            result
                .display_list
                .commands
                .iter()
                .all(|command| match command {
                    Command::Text { bounds, .. } => bounds.width > 10.0,
                    _ => true,
                }),
            "hidden text was painted"
        );
    }

    #[test]
    fn a_flex_button_centers_its_label_inside_min_height() {
        let page = parse_html(
            r#"
            <html><body style="margin:0">
              <a href="/a" style="display:flex;align-items:center;min-height:48px;padding:6px;border:1px solid #ccc">About</a>
            </body></html>
        "#,
        )
        .unwrap();
        let result = layout(&page, 300.0, &mut FixedText, |_| None).unwrap();
        let text = result
            .display_list
            .commands
            .iter()
            .find_map(|command| match command {
                Command::Text { bounds, .. } => Some(*bounds),
                _ => None,
            });
        let bounds = text.expect("label");
        assert!(bounds.y > 12.0, "{bounds:?}");
    }

    #[test]
    fn logical_margins_apply_for_a_left_to_right_document() {
        let page = parse_html(
            r#"
            <style>
              html:not([dir=rtl]) .start { margin-left: 16px }
              html:not([dir=rtl]) .end { margin-right: 16px }
              html[dir=rtl] .start { margin-right: 40px }
              nav { display: flex; margin: 0 }
              a { margin: 0 }
            </style>
            <html><body style="margin:0"><nav>
              <div class="start end"><a href="/a">About</a></div>
              <div class="start end"><a href="/b">News</a></div>
            </nav></body></html>
        "#,
        )
        .unwrap();
        let result = layout(&page, 400.0, &mut FixedText, |_| None).unwrap();
        let about = result.links.iter().find(|link| link.href == "/a").unwrap();
        let news = result.links.iter().find(|link| link.href == "/b").unwrap();
        assert_eq!(about.rect.x, 16.0);
        assert!(
            news.rect.x >= about.rect.x + about.rect.width + 32.0,
            "{:?} {:?}",
            about.rect,
            news.rect
        );
    }

    #[test]
    fn inline_logical_margins_cancel_the_physical_margins() {
        let page = parse_html(
            r#"
            <style>
              html:not([dir=rtl]) .item { margin-left: 16px; margin-right: 16px }
              nav { display: flex; margin: 0 }
              a { margin: 0 }
            </style>
            <html><body style="margin:0"><nav>
              <div class="item" style="margin-inline-start:0;margin-inline-end:0"><a href="/a">About</a></div>
              <div class="item" style="margin-inline-start:0;margin-inline-end:0"><a href="/b">News</a></div>
            </nav></body></html>
        "#,
        )
        .unwrap();
        let result = layout(&page, 400.0, &mut FixedText, |_| None).unwrap();
        let about = result.links.iter().find(|link| link.href == "/a").unwrap();
        let news = result.links.iter().find(|link| link.href == "/b").unwrap();
        assert_eq!(about.rect.x, 0.0);
        assert_eq!(news.rect.x, 40.0, "{:?} {:?}", about.rect, news.rect);
    }

    #[test]
    fn a_late_padding_rule_still_widens_its_link() {
        let mut css = String::new();
        for index in 0..2200 {
            css.push_str(&format!(".filler{index}{{color:#111}}"));
        }
        css.push_str("a.lg{display:block;padding:14px;border:1px solid #ccc;margin:0}");
        let page = parse_html(&format!(
            "<style>{css}</style><body style=\"margin:0\"><a class=\"lg\" href=\"/a\">About</a></body>"
        ))
        .unwrap();
        let result = layout(&page, 400.0, &mut FixedText, |_| None).unwrap();
        let about = result
            .links
            .iter()
            .find(|link| link.rect.height > 20.0)
            .unwrap();
        assert!(
            about.rect.width >= 68.0,
            "padding was dropped: {:?}",
            about.rect
        );
    }

    #[test]
    fn flex_items_keep_labels_intact_when_the_row_is_narrow() {
        let page = parse_html(
            r#"
            <style>
              :root { --pad: 4px; --gap: calc(var(--wide) - 2px); --wide: 10px; --brand: #112233; }
              nav { display: flex; gap: var(--gap); margin: 0; background: var(--brand); white-space: nowrap; }
              a { padding: var(--pad); border: 1px solid #ccc; margin: 0; }
            </style>
            <body style="margin:0"><nav>
              <a href="/a">Businesses</a>
              <a href="/b">Create</a>
            </nav></body>
        "#,
        )
        .unwrap();
        let result = layout(&page, 120.0, &mut FixedText, |_| None).unwrap();
        let text: Vec<_> = result
            .display_list
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Text { bounds, .. } => Some(*bounds),
                _ => None,
            })
            .collect();
        assert!(
            text.iter()
                .any(|bounds| bounds.width >= 80.0 && bounds.x < 20.0),
            "{text:?}"
        );
        assert!(text.iter().any(|bounds| bounds.x >= 90.0), "{text:?}");
    }

    #[test]
    fn exposes_the_site_fallback_when_no_server_markup_is_available() {
        let page = parse_html(r#"<html><head><title>Shell</title><script src="/app.js"></script></head>
            <body><div id="root"></div><noscript><p><a href="/help">Open the site help</a></p></noscript></body></html>"#).unwrap();
        assert!(page.is_script_shell());
        let result = layout(&page, 400.0, &mut FixedText, |_| None).unwrap();
        assert!(result.height > 0.0);
        assert_eq!(result.links[0].href, "/help");
    }
}

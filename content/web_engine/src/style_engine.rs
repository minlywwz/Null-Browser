use std::collections::HashMap;

use display_list::{Color, FontFamily, TextAlign, TextStyle};

use crate::{
    Viewport,
    css_parser::{
        AlignItems, Compound, Declaration, Display, GridColumns, JustifyContent, Length,
        MAX_STYLESHEET_BYTES, Position, Rule, Selector, WhiteSpace, parse_inline, parse_selector,
        parse_stylesheet,
    },
    dom::{Document, Kind, Node, NodeId},
};

#[derive(Clone, Copy)]
pub(crate) struct Style {
    pub(crate) display: Display,
    pub(crate) flex_column: bool,
    pub(crate) flex_grow: f32,
    pub(crate) flex_shrink: f32,
    pub(crate) justify_content: JustifyContent,
    pub(crate) position: Position,
    pub(crate) inset: [Length; 4],
    pub(crate) opacity: f32,
    pub(crate) gap: [Length; 2],
    pub(crate) grid_columns: GridColumns,
    table_columns: usize,
    pub(crate) align: AlignItems,
    pub(crate) border: f32,
    pub(crate) border_color: Color,
    pub(crate) min_width: Length,
    pub(crate) text: TextStyle,
    pub(crate) color: Color,
    pub(crate) family: FontFamily,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) strikethrough: bool,
    pub(crate) list_marker: Option<ListMarker>,
    hide_list_marker: bool,
    pub(crate) background: Color,
    pub(crate) width: Length,
    pub(crate) max_width: Length,
    pub(crate) height: Length,
    pub(crate) min_height: Length,
    pub(crate) border_box: bool,
    pub(crate) overflow_clip: bool,
    pub(crate) visible: bool,
    clip_hidden: bool,
    pub(crate) margin: [Length; 4],
    pub(crate) padding: [Length; 4],
    pub(crate) radius: f32,
    pub(crate) white_space: WhiteSpace,
}

#[derive(Clone, Copy)]
pub(crate) enum ListMarker {
    Bullet,
    Ordinal(i32),
}

impl Default for Style {
    fn default() -> Self {
        Self {
            display: Display::Inline,
            flex_column: false,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            justify_content: JustifyContent::Start,
            position: Position::Static,
            inset: [Length::Auto; 4],
            opacity: 1.0,
            gap: [Length::Px(0.0); 2],
            grid_columns: GridColumns::equal(1),
            table_columns: 1,
            align: AlignItems::Start,
            border: 0.0,
            border_color: Color::rgb(0x151923),
            min_width: Length::Auto,
            text: TextStyle {
                size: 16.0,
                weight: 400,
                align: TextAlign::Start,
            },
            color: Color::rgb(0x151923),
            family: FontFamily::SansSerif,
            italic: false,
            underline: false,
            strikethrough: false,
            list_marker: None,
            hide_list_marker: false,
            background: Color::TRANSPARENT,
            width: Length::Auto,
            max_width: Length::Auto,
            height: Length::Auto,
            min_height: Length::Auto,
            border_box: false,
            overflow_clip: false,
            visible: true,
            clip_hidden: false,
            margin: [Length::Px(0.0); 4],
            padding: [Length::Px(0.0); 4],
            radius: 0.0,
            white_space: WhiteSpace::Normal,
        }
    }
}

impl Style {
    fn for_node(
        document: &Document,
        id: NodeId,
        parent: Self,
        rules: &[Rule],
        tokens: &HashMap<String, String>,
        viewport: Viewport,
    ) -> Self {
        let rtl = inherited_rtl(document, id);
        let node = &document.nodes[id];
        let mut style = Self {
            text: parent.text,
            color: parent.color,
            family: parent.family,
            italic: parent.italic,
            underline: parent.underline,
            strikethrough: parent.strikethrough,
            hide_list_marker: parent.hide_list_marker,
            white_space: parent.white_space,
            visible: parent.visible,
            table_columns: parent.table_columns,
            ..Self::default()
        };
        let tag = node.name();
        if node.attr("hidden").is_some() {
            style.display = Display::None;
            return style;
        }
        match tag {
            "html" | "main" | "article" | "aside" | "section" | "header" | "footer" | "div"
            | "nav" | "figure" | "figcaption" | "li" | "form" | "dl" | "dt" | "dd" | "address"
            | "details" | "summary" | "caption" | "thead" | "tbody" | "tfoot" | "td" => {
                style.display = Display::Block
            }
            "ol" | "ul" => {
                style.display = Display::Block;
                style.margin[0] = Length::Em(1.0);
                style.margin[2] = Length::Em(1.0);
                style.padding[3] = Length::Px(40.0);
            }
            "table" => {
                style.display = Display::Block;
                style.table_columns = table_columns(document, node);
            }
            "img" => {
                style.display = Display::Block;
                let size = |attribute| {
                    node.attr(attribute)
                        .and_then(|value| value.trim().parse::<u32>().ok())
                        .map_or(Length::Auto, |value| Length::Px(value.min(16_384) as f32))
                };
                style.width = size("width");
                style.height = size("height");
            }
            "tr" => {
                style.display = Display::Grid;
                style.grid_columns = GridColumns::equal(parent.table_columns);
                style.gap[1] = Length::Px(2.0);
            }
            "th" => {
                style.display = Display::Block;
                style.text.weight = 700;
                style.text.align = TextAlign::Center;
            }
            "body" => {
                style.display = Display::Block;
                style.margin = [Length::Px(8.0); 4];
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" => {
                style.display = Display::Block;
                style.margin[0] = Length::Px(8.0);
                style.margin[2] = Length::Px(12.0);
                if tag != "p" {
                    style.text.size = match tag {
                        "h1" => 32.0,
                        "h2" => 24.0,
                        "h3" => 20.0,
                        _ => 17.0,
                    };
                    style.text.weight = 700;
                }
            }
            "a" if node.attr("href").is_some() => {
                style.color = Color::rgb(0x1755c5);
                style.underline = true;
            }
            "strong" | "b" => style.text.weight = 700,
            "em" | "i" | "cite" | "var" | "dfn" => style.italic = true,
            "u" | "ins" => style.underline = true,
            "s" | "del" | "strike" => style.strikethrough = true,
            "code" | "kbd" | "samp" => style.family = FontFamily::Monospace,
            "pre" => {
                style.display = Display::Block;
                style.family = FontFamily::Monospace;
                style.white_space = WhiteSpace::Pre;
                style.margin[0] = Length::Em(1.0);
                style.margin[2] = Length::Em(1.0);
            }
            "blockquote" => {
                style.display = Display::Block;
                style.margin = [
                    Length::Em(1.0),
                    Length::Px(40.0),
                    Length::Em(1.0),
                    Length::Px(40.0),
                ];
            }
            "hr" => {
                style.display = Display::Block;
                style.height = Length::Px(1.0);
                style.background = Color::rgb(0xb3b8c2);
                style.margin[0] = Length::Em(0.5);
                style.margin[2] = Length::Em(0.5);
            }
            "button" | "input" | "textarea" | "select" => {
                style.display = Display::Block;
                style.padding = [
                    Length::Px(6.0),
                    Length::Px(12.0),
                    Length::Px(6.0),
                    Length::Px(12.0),
                ];
                style.background = Color::rgb(0xf4f6f8);
                style.border = 1.0;
                style.border_color = Color::rgb(0xc5cad3);
                style.radius = 6.0;
            }
            "option" | "optgroup" => style.display = Display::None,
            "noscript" if document.scripting => {
                style.display = Display::None;
                return style;
            }
            // These elements are deliberately inert in the supported subset.
            "head" | "title" | "style" | "script" | "meta" | "base" | "link" | "template"
            | "iframe" | "object" | "embed" | "video" | "audio" => {
                style.display = Display::None;
                return style;
            }
            _ => {}
        }
        let inline = node
            .attr("style")
            .map(|inline| parse_inline(inline, tokens));
        let mut important = Vec::new();
        for declaration in rules
            .iter()
            .filter(|rule| matches_selector(document, id, &rule.selector))
            .flat_map(|rule| rule.declarations.iter().copied())
            .chain(inline.into_iter().flatten())
        {
            if declaration.important {
                important.push(declaration.value);
            } else {
                style.apply(declaration.value, parent.text.size, viewport, rtl);
            }
        }
        for declaration in important {
            style.apply(declaration, parent.text.size, viewport, rtl);
        }
        style.underline |= parent.underline;
        style.strikethrough |= parent.strikethrough;
        style.opacity *= parent.opacity;
        let clipped_marker = style.overflow_clip
            && style
                .width
                .resolve(viewport.width, viewport, style.text.size)
                .is_some_and(|value| value <= 1.0)
            && style
                .height
                .resolve(viewport.height, viewport, style.text.size)
                .is_some_and(|value| value <= 1.0);
        if style.clip_hidden
            || clipped_marker
            || (tag == "input"
                && node
                    .attr("type")
                    .is_some_and(|value| value.eq_ignore_ascii_case("hidden")))
        {
            style.display = Display::None;
        }
        if style.position == Position::Absolute && style.display == Display::Inline {
            style.display = Display::Block;
        }
        style
    }

    fn apply(&mut self, declaration: Declaration, parent_font: f32, viewport: Viewport, rtl: bool) {
        match declaration {
            Declaration::Display(value) => self.display = value,
            Declaration::FlexColumn(value) => self.flex_column = value,
            Declaration::FlexGrow(value) => self.flex_grow = value,
            Declaration::FlexShrink(value) => self.flex_shrink = value,
            Declaration::JustifyContent(value) => self.justify_content = value,
            Declaration::Position(value) => self.position = value,
            Declaration::Inset(side, value) => self.inset[side] = value,
            Declaration::Opacity(value) => self.opacity = value,
            Declaration::Gap(axis, value) => self.gap[axis] = value,
            Declaration::GridColumns(value) => self.grid_columns = value,
            Declaration::Align(value) => self.align = value,
            Declaration::BorderWidth(value) => self.border = value,
            Declaration::BorderColor(value) => self.border_color = value,
            Declaration::MinWidth(value) => self.min_width = value,
            Declaration::Color(value) => self.color = value,
            Declaration::Background(value) => self.background = value,
            Declaration::FontSize(value) => {
                if let Some(size) = value.resolve(parent_font, viewport, parent_font) {
                    self.text.size = size.clamp(1.0, 256.0);
                }
            }
            Declaration::FontWeight(value) => self.text.weight = value,
            Declaration::FontFamily(value) => self.family = value,
            Declaration::Italic(value) => self.italic = value,
            Declaration::TextDecoration {
                underline,
                strikethrough,
            } => {
                self.underline = underline;
                self.strikethrough = strikethrough;
            }
            Declaration::HideListMarker(hidden) => self.hide_list_marker = hidden,
            Declaration::TextAlign(value) => self.text.align = value,
            Declaration::Width(value) => self.width = value,
            Declaration::MaxWidth(value) => self.max_width = value,
            Declaration::Height(value) => self.height = value,
            Declaration::MinHeight(value) => self.min_height = value,
            Declaration::BorderBox(value) => self.border_box = value,
            Declaration::OverflowClip(value) => self.overflow_clip = value,
            Declaration::Visibility(value) => self.visible = value,
            Declaration::ClipHidden => self.clip_hidden = true,
            Declaration::Margin(side, value) => self.margin[side] = value,
            Declaration::MarginInline(start, value) => {
                let side = if start ^ rtl { 3 } else { 1 };
                self.margin[side] = value;
            }
            Declaration::Padding(side, value) => self.padding[side] = value,
            Declaration::Radius(value) => self.radius = value,
            Declaration::WhiteSpace(value) => self.white_space = value,
        }
    }
}

fn table_columns(document: &Document, table: &Node) -> usize {
    let rows = table.children.iter().flat_map(|id| {
        let node = &document.nodes[*id];
        let children = if matches!(node.name(), "thead" | "tbody" | "tfoot") {
            node.children.as_slice()
        } else {
            std::slice::from_ref(id)
        };
        children.iter().map(|id| &document.nodes[*id])
    });
    rows.filter(|row| row.name() == "tr")
        .map(|row| {
            row.children
                .iter()
                .filter(|id| matches!(document.nodes[**id].name(), "th" | "td"))
                .count()
        })
        .max()
        .unwrap_or(1)
}

fn matches_compound(document: &Document, id: NodeId, part: &Compound) -> bool {
    let node = &document.nodes[id];
    let siblings = || {
        node.parent.map(|parent| {
            document.nodes[parent]
                .children
                .iter()
                .copied()
                .filter(|id| matches!(document.nodes[*id].kind, Kind::Element { .. }))
        })
    };
    matches!(node.kind, Kind::Element { .. })
        && part.tag.as_ref().is_none_or(|tag| node.name() == tag)
        && part
            .ids
            .iter()
            .all(|id| node.attr("id") == Some(id.as_str()))
        && part.classes.iter().all(|class| {
            node.attr("class")
                .is_some_and(|names| names.split_ascii_whitespace().any(|name| name == class))
        })
        && (!part.link || (node.name() == "a" && node.attr("href").is_some()))
        && (!part.root || (node.name() == "html" && node.parent == Some(0)))
        && (!part.first_child || siblings().and_then(|mut elements| elements.next()) == Some(id))
        && (!part.last_child
            || siblings().and_then(|mut elements| elements.next_back()) == Some(id))
        && part.attributes.iter().all(|(key, value)| {
            node.attr(key)
                .is_some_and(|attribute| value.as_deref().is_none_or(|value| value == attribute))
        })
        && (!part.not_dir_rtl || !direction_rtl(node))
        && (!part.dir_rtl || direction_rtl(node))
}

fn inherited_rtl(document: &Document, id: NodeId) -> bool {
    let mut current = Some(id);
    while let Some(node_id) = current {
        if let Some(value) = document.nodes[node_id].attr("dir") {
            return value.eq_ignore_ascii_case("rtl");
        }
        current = document.nodes[node_id].parent;
    }
    false
}

fn direction_rtl(node: &Node) -> bool {
    node.attr("dir")
        .is_some_and(|value| value.eq_ignore_ascii_case("rtl"))
}

fn match_segment(
    document: &Document,
    selector: &Selector,
    start: usize,
    end: usize,
    id: NodeId,
) -> Option<NodeId> {
    let mut node = id;
    for part in (start..=end).rev() {
        if !matches_compound(document, node, &selector.parts[part]) {
            return None;
        }
        if part != start {
            node = document.nodes[node].parent?;
        }
    }
    Some(node)
}

pub(crate) fn node_matches(document: &Document, id: NodeId, selector: &str) -> bool {
    parse_selector(selector).is_some_and(|selector| matches_selector(document, id, &selector))
}

fn matches_selector(document: &Document, id: NodeId, selector: &Selector) -> bool {
    let mut end = selector.parts.len() - 1;
    let mut candidate = Some(id);
    let mut first = true;
    loop {
        let start = (0..end)
            .rev()
            .find(|i| !selector.child[*i])
            .map_or(0, |i| i + 1);
        let mut match_start = None;
        while let Some(node) = candidate {
            match_start = match_segment(document, selector, start, end, node);
            if first || match_start.is_some() {
                break;
            }
            candidate = document.nodes[node].parent;
        }
        let Some(node) = match_start else {
            return false;
        };
        if start == 0 {
            return true;
        }
        candidate = document.nodes[node].parent;
        first = false;
        end = start - 1;
    }
}

pub(crate) fn compute_styles(
    document: &Document,
    viewport: Viewport,
    external: &HashMap<String, String>,
) -> Result<Vec<Style>, String> {
    let mut source = String::new();
    for style in document.stylesheet_nodes() {
        let node = &document.nodes[style];
        let sheet = if node.name() == "link" {
            node.attr("href").and_then(|href| external.get(href))
        } else {
            node.children.iter().find_map(|id| {
                if let Kind::Text(text) = &document.nodes[*id].kind {
                    Some(text)
                } else {
                    None
                }
            })
        };
        if let Some(text) = sheet
            && source.len() + text.len() < MAX_STYLESHEET_BYTES
        {
            source.push_str(text);
            source.push('\n');
        }
    }
    let (rules, tokens) = parse_stylesheet(&source, viewport)?;
    let mut styles = vec![Style::default(); document.nodes.len()];
    let mut next_number = vec![None; document.nodes.len()];
    for id in document.tree_order() {
        let node = &document.nodes[id];
        let Some(parent) = node.parent else { continue };
        let mut style = Style::for_node(document, id, styles[parent], &rules, &tokens, viewport);
        if node.name() == "li" && style.display != Display::None {
            let list = &document.nodes[parent];
            let marker = if list.name() == "ol" {
                let number: i32 = node
                    .attr("value")
                    .and_then(|value| value.parse().ok())
                    .or(next_number[parent])
                    .or_else(|| list.attr("start").and_then(|value| value.parse().ok()))
                    .unwrap_or(1);
                next_number[parent] = Some(number.saturating_add(1));
                ListMarker::Ordinal(number)
            } else {
                ListMarker::Bullet
            };
            style.list_marker = (!style.hide_list_marker).then_some(marker);
        }
        styles[id] = style;
    }
    Ok(styles)
}

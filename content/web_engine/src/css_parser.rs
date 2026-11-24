use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::Viewport;
use display_list::{Color, TextAlign};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontFace {
    pub family: String,
    pub src: String,
}

pub fn parse_font_faces(source: &str) -> Vec<FontFace> {
    if source.len() > MAX_STYLESHEET_BYTES {
        return Vec::new();
    }
    let source = strip_comments(source);
    let mut faces = Vec::new();
    let mut rest = source.as_str();
    while faces.len() < 64
        && let Some((prelude, tail)) = rest.split_once('{')
    {
        let Some(end) = matching_end(tail) else { break };
        let prelude = prelude.rsplit(';').next().unwrap_or(prelude).trim();
        let body = &tail[..end];
        if prelude.eq_ignore_ascii_case("@font-face") {
            let mut family = None;
            let mut src = None;
            for part in split_top_level(body, |c| c == ';').take(32) {
                let Some((name, value)) = part.split_once(':') else {
                    continue;
                };
                let name = name.trim().to_ascii_lowercase();
                let value = value.trim();
                match name.as_str() {
                    "font-family" => {
                        let fam = value.trim_matches(['\'', '"']).trim();
                        if !fam.is_empty() {
                            family = Some(fam.to_owned());
                        }
                    }
                    "src" => {
                        if let Some(url) = extract_font_url(value) {
                            src = Some(url);
                        }
                    }
                    _ => {}
                }
            }
            if let (Some(family), Some(src)) = (family, src)
                && !faces
                    .iter()
                    .any(|f: &FontFace| f.family.eq_ignore_ascii_case(&family) && f.src == src)
            {
                faces.push(FontFace { family, src });
            }
        }
        rest = &tail[end + 1..];
    }
    faces
}

fn extract_font_url(value: &str) -> Option<String> {
    if let Some(start) = value.find("url(") {
        let rest = &value[start + 4..];
        let end = matching_paren(rest.as_bytes(), 0).or_else(|| rest.find(')'))?;
        let url = rest[..end].trim().trim_matches(['\'', '"']).trim();
        if !url.is_empty() {
            return Some(url.to_owned());
        }
    }
    let clean = value
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(['\'', '"'])
        .trim();
    if !clean.is_empty()
        && (clean.starts_with("http") || clean.contains('/') || clean.contains('.'))
    {
        return Some(clean.to_owned());
    }
    None
}

const MAX_RULES: usize = 8192;
const MAX_CUSTOM_PROPERTIES: usize = 8192;
pub(crate) const MAX_STYLESHEET_BYTES: usize = 768 * 1024;

fn custom_properties(source: &str) -> HashMap<String, String> {
    let bytes = source.as_bytes();
    let mut map = HashMap::new();
    let mut index = 0;
    let mut quote = 0u8;
    let mut paren = 0i32;
    while index < bytes.len() && map.len() < MAX_CUSTOM_PROPERTIES {
        let byte = bytes[index];
        if quote != 0 {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == quote {
                quote = 0;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = byte,
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'{' | b';' if paren == 0 => {
                index += 1;
                if let Some((name, value, next)) = custom_declaration(bytes, index) {
                    map.entry(name).or_insert(value);
                    index = next;
                }
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    map
}

fn custom_declaration(bytes: &[u8], mut index: usize) -> Option<(String, String, usize)> {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    if !bytes.get(index..)?.starts_with(b"--") {
        return None;
    }
    let start = index;
    index += 2;
    while bytes
        .get(index)
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        index += 1;
    }
    if index == start + 2 {
        return None;
    }
    let name = std::str::from_utf8(&bytes[start..index]).ok()?.to_owned();
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    if bytes.get(index) != Some(&b':') {
        return None;
    }
    index += 1;
    let value_start = index;
    let mut quote = 0u8;
    let mut paren = 0i32;
    while index < bytes.len() {
        let byte = bytes[index];
        if quote != 0 {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == quote {
                quote = 0;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = byte,
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b';' | b'}' if paren == 0 => break,
            _ => {}
        }
        index += 1;
    }
    let mut value = std::str::from_utf8(&bytes[value_start..index])
        .ok()?
        .trim()
        .to_owned();
    if value.len() > 1024 {
        return None;
    }
    if let Some(stripped) = value.strip_suffix("!important") {
        value = stripped.trim().to_owned();
    }
    Some((name, value, index))
}

fn apply_tokens(source: &str, tokens: &HashMap<String, String>) -> String {
    let mut output = String::with_capacity(source.len());
    expand_variables(
        source,
        tokens,
        0,
        &mut output,
        source.len() + MAX_STYLESHEET_BYTES,
    )
    .map_or_else(|| source.to_owned(), |()| evaluate_calculations(&output))
}

fn expand_variables(
    mut source: &str,
    tokens: &HashMap<String, String>,
    depth: usize,
    output: &mut String,
    limit: usize,
) -> Option<()> {
    const MAX_VALUE_BYTES: usize = 4096;
    if depth > 8 {
        return None;
    }
    while let Some(start) = source.find("var(") {
        let end = matching_paren(source.as_bytes(), start + 3)?;
        if output.len() + start > limit {
            return None;
        }
        output.push_str(&source[..start]);
        let checkpoint = output.len();
        let value_limit = limit.min(checkpoint + MAX_VALUE_BYTES);
        let resolved = variable_replacement(&source[start + 4..end], tokens).is_some_and(|value| {
            expand_variables(value, tokens, depth + 1, output, value_limit).is_some()
        });
        if !resolved {
            output.truncate(checkpoint);
            if depth != 0 || output.len() + end + 1 - start > limit {
                return None;
            }
            output.push_str(&source[start..=end]);
        }
        source = &source[end + 1..];
    }
    if output.len() + source.len() > limit {
        return None;
    }
    output.push_str(source);
    Some(())
}

fn variable_replacement<'a>(
    inner: &'a str,
    tokens: &'a HashMap<String, String>,
) -> Option<&'a str> {
    let mut depth_parens = 0i32;
    let mut comma = None;
    for (offset, character) in inner.char_indices() {
        match character {
            '(' => depth_parens += 1,
            ')' => depth_parens -= 1,
            ',' if depth_parens == 0 => {
                comma = Some(offset);
                break;
            }
            _ => {}
        }
    }
    let (name, fallback) = comma.map_or((inner.trim(), None), |offset| {
        (inner[..offset].trim(), Some(inner[offset + 1..].trim()))
    });
    if !name.starts_with("--") || name.contains(char::is_whitespace) {
        return None;
    }
    tokens.get(name).map(String::as_str).or(fallback)
}

fn evaluate_calculations(source: &str) -> String {
    let mut source = source.to_owned();
    let mut resume = 0;
    for _ in 0..64 {
        let Some((start, end)) = innermost_calculation(&source, resume) else {
            break;
        };
        if let Some(value) = calculation_value(&source[start + 5..end]) {
            source.replace_range(start..=end, &value);
            resume = 0;
        } else {
            resume = end + 1;
        }
    }
    source
}

fn innermost_calculation(source: &str, from: usize) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut index = from.min(bytes.len());
    while index + 5 < bytes.len() {
        if bytes[index..].starts_with(b"calc(")
            && let Some(end) = matching_paren(bytes, index + 4)
        {
            if !source[index + 5..end].contains("calc(") {
                return Some((index, end));
            }
            index += 5;
            continue;
        }
        index += 1;
    }
    None
}

fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote = 0u8;
    let mut index = open;
    while index < bytes.len() {
        let byte = bytes[index];
        if quote != 0 {
            if byte == b'\\' {
                index += 2;
                continue;
            }
            if byte == quote {
                quote = 0;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => quote = byte,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CalcUnit {
    None,
    Px,
    Percent,
    Em,
    Rem,
    Vw,
    Vh,
}

fn calculation_value(body: &str) -> Option<String> {
    let mut parser = CalcParser {
        bytes: body.as_bytes(),
        index: 0,
    };
    let (value, unit) = parser.expression()?;
    parser.skip();
    if parser.index != parser.bytes.len() || !value.is_finite() || value.abs() > 1_000_000.0 {
        return None;
    }
    Some(format_calculation(value, unit))
}

fn format_calculation(value: f32, unit: CalcUnit) -> String {
    let number = if (value - value.round()).abs() < 0.001 {
        format!("{}", value.round() as i32)
    } else {
        let text = format!("{value:.3}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    };
    match unit {
        CalcUnit::None => number,
        CalcUnit::Px => format!("{number}px"),
        CalcUnit::Percent => format!("{number}%"),
        CalcUnit::Em => format!("{number}em"),
        CalcUnit::Rem => format!("{number}rem"),
        CalcUnit::Vw => format!("{number}vw"),
        CalcUnit::Vh => format!("{number}vh"),
    }
}

struct CalcParser<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl CalcParser<'_> {
    fn skip(&mut self) {
        while self
            .bytes
            .get(self.index)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.index += 1;
        }
    }

    fn expression(&mut self) -> Option<(f32, CalcUnit)> {
        let mut left = self.term()?;
        loop {
            self.skip();
            let Some(&operator) = self.bytes.get(self.index) else {
                break;
            };
            if operator != b'+' && operator != b'-' {
                break;
            }
            self.index += 1;
            let right = self.term()?;
            left = add_lengths(left, right, operator)?;
        }
        Some(left)
    }

    fn term(&mut self) -> Option<(f32, CalcUnit)> {
        let mut left = self.factor()?;
        loop {
            self.skip();
            let Some(&operator) = self.bytes.get(self.index) else {
                break;
            };
            if operator != b'*' && operator != b'/' {
                break;
            }
            self.index += 1;
            let right = self.factor()?;
            left = multiply_lengths(left, right, operator)?;
        }
        Some(left)
    }

    fn factor(&mut self) -> Option<(f32, CalcUnit)> {
        self.skip();
        match self.bytes.get(self.index) {
            Some(b'(') => {
                self.index += 1;
                let value = self.expression()?;
                self.skip();
                if self.bytes.get(self.index) != Some(&b')') {
                    return None;
                }
                self.index += 1;
                Some(value)
            }
            Some(b'+') => {
                self.index += 1;
                self.factor()
            }
            _ => self.number(),
        }
    }

    fn number(&mut self) -> Option<(f32, CalcUnit)> {
        self.skip();
        let start = self.index;
        if matches!(self.bytes.get(self.index), Some(b'+' | b'-')) {
            self.index += 1;
        }
        let digits = self.index;
        while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
            self.index += 1;
        }
        if self.bytes.get(self.index) == Some(&b'.') {
            self.index += 1;
            while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
                self.index += 1;
            }
        }
        if self.index == digits {
            return None;
        }
        let value = std::str::from_utf8(&self.bytes[start..self.index])
            .ok()?
            .parse()
            .ok()?;
        Some((value, self.unit()))
    }

    fn unit(&mut self) -> CalcUnit {
        let rest = &self.bytes[self.index..];
        for (name, unit) in [
            ("px", CalcUnit::Px),
            ("rem", CalcUnit::Rem),
            ("em", CalcUnit::Em),
            ("vw", CalcUnit::Vw),
            ("vh", CalcUnit::Vh),
            ("%", CalcUnit::Percent),
        ] {
            if rest.starts_with(name.as_bytes()) {
                self.index += name.len();
                return unit;
            }
        }
        CalcUnit::None
    }
}

fn add_lengths(
    (left, left_unit): (f32, CalcUnit),
    (right, right_unit): (f32, CalcUnit),
    operator: u8,
) -> Option<(f32, CalcUnit)> {
    let unit = match (left_unit, right_unit) {
        (CalcUnit::None, unit) | (unit, CalcUnit::None) => unit,
        (left, right) if left == right => left,
        _ => return None,
    };
    let value = if operator == b'+' {
        left + right
    } else {
        left - right
    };
    Some((value, unit))
}

fn multiply_lengths(
    (left, left_unit): (f32, CalcUnit),
    (right, right_unit): (f32, CalcUnit),
    operator: u8,
) -> Option<(f32, CalcUnit)> {
    if operator == b'*' {
        return match (left_unit, right_unit) {
            (CalcUnit::None, unit) => Some((left * right, unit)),
            (unit, CalcUnit::None) => Some((left * right, unit)),
            _ => None,
        };
    }
    (right_unit == CalcUnit::None && right != 0.0).then_some((left / right, left_unit))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Length {
    Auto,
    Px(f32),
    Percent(f32),
    Em(f32),
    Rem(f32),
    Vw(f32),
    Vh(f32),
}

impl Length {
    pub(crate) fn resolve(self, available: f32, viewport: Viewport, em: f32) -> Option<f32> {
        match self {
            Self::Auto => None,
            Self::Px(px) => Some(px),
            Self::Percent(percent) => Some(available * percent / 100.0),
            Self::Em(value) => Some(em * value),
            Self::Rem(value) => Some(16.0 * value),
            Self::Vw(value) => Some(viewport.width * value / 100.0),
            Self::Vh(value) => Some(viewport.height * value / 100.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Display {
    None,
    Inline,
    Block,
    InlineBlock,
    Flex,
    Grid,
}

impl Display {
    /// Block-level boxes force a new line in normal flow.
    pub(crate) fn is_block(self) -> bool {
        matches!(self, Self::Block | Self::Flex | Self::Grid)
    }

    /// Inline-level boxes that size like a block but flow with text.
    pub(crate) fn is_inline_block(self) -> bool {
        self == Self::InlineBlock
    }
}

const MAX_GRID_COLUMNS: usize = 16;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum GridTrack {
    Length(Length),
    Fraction(f32),
}

#[derive(Clone, Copy)]
pub(crate) struct GridColumns {
    tracks: [GridTrack; MAX_GRID_COLUMNS],
    count: usize,
}

impl GridColumns {
    pub(crate) const fn equal(count: usize) -> Self {
        Self {
            tracks: [GridTrack::Fraction(1.0); MAX_GRID_COLUMNS],
            count,
        }
    }

    pub(crate) fn tracks(&self) -> &[GridTrack] {
        &self.tracks[..self.count.clamp(1, MAX_GRID_COLUMNS)]
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Position {
    #[default]
    Static,
    Relative,
    Absolute,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum AlignItems {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Default)]
pub(crate) enum JustifyContent {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
}

#[derive(Clone, Copy)]
pub(crate) enum Declaration {
    Display(Display),
    FlexColumn(bool),
    FlexGrow(f32),
    FlexShrink(f32),
    JustifyContent(JustifyContent),
    Position(Position),
    Inset(usize, Length),
    Opacity(f32),
    Gap(usize, Length),
    GridColumns(GridColumns),
    Align(AlignItems),
    BorderWidth(f32),
    BorderColor(Color),
    MinWidth(Length),
    Color(Color),
    Background(Color),
    FontSize(Length),
    FontWeight(u32),
    FontFamily(display_list::FontFamily),
    Italic(bool),
    TextDecoration {
        underline: bool,
        strikethrough: bool,
    },
    HideListMarker(bool),
    TextAlign(TextAlign),
    Width(Length),
    MaxWidth(Length),
    Height(Length),
    MinHeight(Length),
    BorderBox(bool),
    OverflowClip(bool),
    Visibility(bool),
    ClipHidden,
    Margin(usize, Length),
    // start is the inline-start edge: left in a left-to-right element.
    MarginInline(bool, Length),
    Padding(usize, Length),
    Radius(f32),
    WhiteSpace(WhiteSpace),
    TextTransform(TextTransform),
    LineHeight(Length),
    FlexWrap(bool),
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum TextTransform {
    #[default]
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum AttrMatch {
    Exists,
    Exact(String),
    Includes(String),
    Prefix(String),
    Suffix(String),
    Substring(String),
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum WhiteSpace {
    #[default]
    Normal,
    NoWrap,
    Pre,
    PreWrap,
    PreLine,
}

impl WhiteSpace {
    pub(crate) fn wrap(self) -> bool {
        !matches!(self, Self::NoWrap | Self::Pre)
    }

    pub(crate) fn preserve_spaces(self) -> bool {
        matches!(self, Self::Pre | Self::PreWrap)
    }

    pub(crate) fn preserve_breaks(self) -> bool {
        matches!(self, Self::Pre | Self::PreWrap | Self::PreLine)
    }
}

#[derive(Default)]
pub(crate) struct Compound {
    pub(crate) tag: Option<String>,
    pub(crate) ids: Vec<String>,
    pub(crate) classes: Vec<String>,
    pub(crate) link: bool,
    pub(crate) root: bool,
    pub(crate) first_child: bool,
    pub(crate) last_child: bool,
    pub(crate) empty: bool,
    pub(crate) disabled: bool,
    pub(crate) checked: bool,
    pub(crate) attributes: Vec<(String, AttrMatch)>,
    pub(crate) not_dir_rtl: bool,
    pub(crate) dir_rtl: bool,
}

pub(crate) struct Selector {
    pub(crate) parts: Vec<Compound>,
    // child[i] describes the relationship between parts i and i + 1.
    pub(crate) child: Vec<bool>,
    specificity: (usize, usize, usize),
}

pub(crate) struct Rule {
    pub(crate) selector: Selector,
    pub(crate) declarations: Arc<[Declared]>,
}

#[derive(Clone, Copy)]
pub(crate) struct Declared {
    pub(crate) value: Declaration,
    pub(crate) important: bool,
}

#[allow(dead_code)]
pub(crate) fn parse_stylesheet(
    source: &str,
    viewport: Viewport,
) -> Result<(Vec<Rule>, HashMap<String, String>), String> {
    parse_stylesheet_with_fonts(source, viewport, &[])
}

pub(crate) fn parse_stylesheet_with_fonts(
    source: &str,
    viewport: Viewport,
    known_fonts: &[FontFace],
) -> Result<(Vec<Rule>, HashMap<String, String>), String> {
    if source.len() > MAX_STYLESHEET_BYTES {
        return Err("The CSS exceeds the stylesheet size limit.".into());
    }
    let source = strip_comments(source);
    let mut custom_fonts = known_fonts.to_vec();
    for face in parse_font_faces(&source) {
        if !custom_fonts
            .iter()
            .any(|f| f.family.eq_ignore_ascii_case(&face.family))
        {
            custom_fonts.push(face);
        }
    }
    let tokens = custom_properties(&source);
    let source = apply_tokens(&source, &tokens);
    let mut rules = Vec::new();
    parse_rules(&source, viewport, 0, &mut rules, &custom_fonts);
    // Stable sort leaves equal-specificity rules in source order.
    rules.sort_by_key(|rule| rule.selector.specificity);
    Ok((rules, tokens))
}

fn parse_rules(
    mut source: &str,
    viewport: Viewport,
    depth: usize,
    rules: &mut Vec<Rule>,
    custom_fonts: &[FontFace],
) {
    while rules.len() < MAX_RULES
        && let Some((prelude, tail)) = source.split_once('{')
    {
        let Some(end) = matching_end(tail) else { break };
        let prelude = prelude.rsplit(';').next().unwrap_or(prelude).trim();
        let body = &tail[..end];
        if let Some(query) = prelude.strip_prefix("@media") {
            if depth < 4 && media_matches(query.trim(), viewport) {
                parse_rules(body, viewport, depth + 1, rules, custom_fonts);
            }
        } else if !prelude.starts_with('@') && prelude.len() <= 4096 {
            let declarations = parse_declarations(body, custom_fonts);
            if !declarations.is_empty() {
                let declarations: Arc<[Declared]> = declarations.into();
                let remaining = MAX_RULES - rules.len();
                rules.extend(
                    split_top_level(prelude, |character| character == ',')
                        .filter_map(parse_selector)
                        .take(remaining)
                        .map(|selector| Rule {
                            selector,
                            declarations: Arc::clone(&declarations),
                        }),
                );
            }
        }
        source = &tail[end + 1..];
    }
}

fn media_matches(query: &str, viewport: Viewport) -> bool {
    query.split(',').any(|query| {
        query.split(" and ").all(|term| {
            let term = term.trim();
            if matches!(term, "screen" | "all") {
                return true;
            }
            let Some((feature, value)) = term
                .strip_prefix('(')
                .and_then(|term| term.strip_suffix(')'))
                .and_then(|term| term.split_once(':'))
            else {
                return false;
            };
            let (dimension, minimum) = match feature.trim() {
                "min-width" => (viewport.width, true),
                "max-width" => (viewport.width, false),
                "min-height" => (viewport.height, true),
                "max-height" => (viewport.height, false),
                _ => return false,
            };
            length(value.trim())
                .and_then(|value| value.resolve(dimension, viewport, 16.0))
                .is_some_and(|value| {
                    if minimum {
                        dimension >= value
                    } else {
                        dimension <= value
                    }
                })
        })
    })
}

fn strip_comments(source: &str) -> String {
    let mut clean = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut quote = None;
    while let Some(character) = chars.next() {
        match quote {
            Some(delimiter) => {
                clean.push(character);
                if character == '\\' {
                    clean.extend(chars.next());
                } else if character == delimiter {
                    quote = None;
                }
            }
            None if character == '/' && chars.next_if_eq(&'*').is_some() => {
                while let Some(character) = chars.next() {
                    if character == '*' && chars.next_if_eq(&'/').is_some() {
                        break;
                    }
                }
                clean.push(' ');
            }
            None => {
                if matches!(character, '\'' | '"') {
                    quote = Some(character);
                }
                clean.push(character);
            }
        }
    }
    clean
}

fn split_top_level(source: &str, separator: impl Fn(char) -> bool) -> impl Iterator<Item = &str> {
    let mut quote = None;
    let mut depth = 0usize;
    let mut escaped = false;
    source.split(move |character| {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            }
        } else {
            match character {
                '\'' | '"' => quote = Some(character),
                '(' | '[' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                _ => return depth == 0 && separator(character),
            }
        }
        false
    })
}

fn grid_columns(value: &str) -> Option<GridColumns> {
    fn track(value: &str) -> Option<GridTrack> {
        let value = value.trim();
        if value == "auto" {
            return Some(GridTrack::Fraction(1.0));
        }
        if let Some(fraction) = value.strip_suffix("fr") {
            return bounded(fraction, 0.0, 1024.0).map(GridTrack::Fraction);
        }
        padding(value).map(GridTrack::Length)
    }
    let mut columns = GridColumns::equal(0);
    for value in split_top_level(value, char::is_whitespace).filter(|value| !value.is_empty()) {
        let (count, pattern) = if let Some(inner) = value
            .strip_prefix("repeat(")
            .and_then(|s| s.strip_suffix(')'))
        {
            let (count, pattern) = inner.split_once(',')?;
            (count.trim().parse::<usize>().ok()?, pattern)
        } else {
            (1, value)
        };
        let pattern: Vec<_> = split_top_level(pattern.trim(), char::is_whitespace)
            .filter(|value| !value.is_empty())
            .map(track)
            .collect::<Option<_>>()?;
        if pattern.is_empty()
            || count == 0
            || count > MAX_GRID_COLUMNS
            || columns.count + count * pattern.len() > MAX_GRID_COLUMNS
        {
            return None;
        }
        for _ in 0..count {
            let end = columns.count + pattern.len();
            columns.tracks[columns.count..end].copy_from_slice(&pattern);
            columns.count = end;
        }
    }
    (columns.count > 0).then_some(columns)
}

fn matching_end(source: &str) -> Option<usize> {
    let mut depth = 1;
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in source.bytes().enumerate() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == delimiter {
                quote = None;
            } else if byte == b'\\' {
                escaped = true;
            }
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn parse_selector(input: &str) -> Option<Selector> {
    if input.len() > 512 {
        return None;
    }
    let mut selector = Selector {
        parts: Vec::new(),
        child: Vec::new(),
        specificity: (0, 0, 0),
    };
    for (index, group) in split_top_level(input, |character| character == '>').enumerate() {
        if group.trim().is_empty() {
            return None;
        }
        for (offset, token) in split_top_level(group, char::is_whitespace)
            .filter(|token| !token.is_empty())
            .enumerate()
        {
            if selector.parts.len() == 8 {
                return None;
            }
            let part = parse_compound(token)?;
            selector.specificity.0 += part.ids.len();
            selector.specificity.1 += part.classes.len()
                + part.attributes.len()
                + usize::from(part.link)
                + usize::from(part.root)
                + usize::from(part.first_child)
                + usize::from(part.last_child)
                + usize::from(part.empty)
                + usize::from(part.disabled)
                + usize::from(part.checked)
                + usize::from(part.not_dir_rtl)
                + usize::from(part.dir_rtl);
            selector.specificity.2 += usize::from(part.tag.is_some());
            if !selector.parts.is_empty() {
                selector.child.push(index != 0 && offset == 0);
            }
            selector.parts.push(part);
        }
    }
    (!selector.parts.is_empty()).then_some(selector)
}

fn parse_compound(input: &str) -> Option<Compound> {
    let mut part = Compound::default();
    let mark = |s: &str| s.find(['.', '#', ':', '[']).unwrap_or(s.len());
    let identifier = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
    };
    let (tag, mut rest) = input.split_at(mark(input));
    if !matches!(tag, "" | "*") {
        if !identifier(tag) {
            return None;
        }
        part.tag = Some(tag.to_ascii_lowercase());
    }
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix(":not([dir=rtl])") {
            part.not_dir_rtl = true;
            rest = tail;
            continue;
        }
        if let Some(tail) = rest.strip_prefix("[dir=rtl]") {
            part.dir_rtl = true;
            rest = tail;
            continue;
        }
        if let Some(attribute) = rest.strip_prefix('[') {
            let end = attribute.find(']')?;
            let raw = attribute[..end].trim();
            let (key, matcher) = if let Some((k, v)) = raw.split_once("~=") {
                (
                    k.trim(),
                    AttrMatch::Includes(v.trim().trim_matches(['\'', '"']).to_owned()),
                )
            } else if let Some((k, v)) = raw.split_once("^=") {
                (
                    k.trim(),
                    AttrMatch::Prefix(v.trim().trim_matches(['\'', '"']).to_owned()),
                )
            } else if let Some((k, v)) = raw.split_once("$=") {
                (
                    k.trim(),
                    AttrMatch::Suffix(v.trim().trim_matches(['\'', '"']).to_owned()),
                )
            } else if let Some((k, v)) = raw.split_once("*=") {
                (
                    k.trim(),
                    AttrMatch::Substring(v.trim().trim_matches(['\'', '"']).to_owned()),
                )
            } else if let Some((k, v)) = raw.split_once('=') {
                (
                    k.trim(),
                    AttrMatch::Exact(v.trim().trim_matches(['\'', '"']).to_owned()),
                )
            } else {
                (raw, AttrMatch::Exists)
            };
            if !identifier(key) {
                return None;
            }
            part.attributes.push((key.to_ascii_lowercase(), matcher));
            rest = &attribute[end + 1..];
            continue;
        }
        if rest.starts_with("::") {
            let after_colons = &rest[2..];
            let name_end = mark(after_colons);
            rest = &after_colons[name_end..];
            continue;
        }
        let Some(marker) = rest.as_bytes().first() else {
            break;
        };
        let (name, tail) = rest[1..].split_at(mark(&rest[1..]));
        if !identifier(name) {
            return None;
        }
        match marker {
            b'.' => part.classes.push(name.to_owned()),
            b'#' => part.ids.push(name.to_owned()),
            b':' if matches!(name, "link" | "any-link") => part.link = true,
            b':' if name == "root" => part.root = true,
            b':' if name == "first-child" => part.first_child = true,
            b':' if name == "last-child" => part.last_child = true,
            b':' if name == "empty" => part.empty = true,
            b':' if name == "disabled" => part.disabled = true,
            b':' if name == "checked" => part.checked = true,
            b':' if matches!(name, "hover" | "focus" | "focus-visible" | "active" | "visited") => {}
            _ => return None,
        }
        rest = tail;
    }
    Some(part)
}

fn bounded(value: &str, minimum: f32, maximum: f32) -> Option<f32> {
    let number: f32 = value.parse().ok()?;
    (minimum..=maximum).contains(&number).then_some(number)
}

fn px(value: &str, maximum: f32) -> Option<f32> {
    if value == "0" {
        return Some(0.0);
    }
    bounded(value.strip_suffix("px")?, 0.0, maximum)
}

fn signed(value: &str, maximum: f32) -> Option<f32> {
    let number: f32 = value.parse().ok()?;
    (-maximum..=maximum).contains(&number).then_some(number)
}

fn length(value: &str) -> Option<Length> {
    if matches!(value, "auto" | "none") {
        Some(Length::Auto)
    } else if let Some(percent) = value.strip_suffix('%') {
        bounded(percent, 0.0, 100.0).map(Length::Percent)
    } else if let Some(value) = value.strip_suffix("rem") {
        signed(value, 1024.0).map(Length::Rem)
    } else if let Some(value) = value.strip_suffix("em") {
        signed(value, 1024.0).map(Length::Em)
    } else if let Some(value) = value.strip_suffix("vw") {
        signed(value, 200.0).map(Length::Vw)
    } else if let Some(value) = value.strip_suffix("vh") {
        signed(value, 200.0).map(Length::Vh)
    } else if value == "0" {
        Some(Length::Px(0.0))
    } else {
        signed(value.strip_suffix("px")?, 16_384.0).map(Length::Px)
    }
}

fn padding(value: &str) -> Option<Length> {
    length(value).filter(|value| match *value {
        Length::Auto => false,
        Length::Px(value)
        | Length::Percent(value)
        | Length::Em(value)
        | Length::Rem(value)
        | Length::Vw(value)
        | Length::Vh(value) => value >= 0.0,
    })
}

fn border(value: &str) -> Option<(f32, Color)> {
    if matches!(value, "none" | "0" | "0px") {
        return Some((0.0, Color::TRANSPARENT));
    }
    let mut width = None;
    let mut paint = None;
    for part in value.split_ascii_whitespace() {
        if matches!(
            part,
            "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset" | "outset"
        ) {
            continue;
        }
        if part == "none" {
            return Some((0.0, Color::TRANSPARENT));
        }
        if width.is_none()
            && let Some(parsed) = px(part, 64.0)
        {
            width = Some(parsed);
            continue;
        }
        if paint.is_none()
            && let Some(parsed) = color(part)
        {
            paint = Some(parsed);
        }
    }
    Some((width.unwrap_or(1.0), paint.unwrap_or(Color::rgb(0x151923))))
}

fn color(value: &str) -> Option<Color> {
    if let Some(hex) = value.strip_prefix('#') {
        let hex = u32::from_str_radix(hex, 16).ok()?;
        return match value.len() {
            4 => Some(Color::rgb(
                (((hex >> 8) * 17) << 16) | ((((hex >> 4) & 15) * 17) << 8) | ((hex & 15) * 17),
            )),
            7 => Some(Color::rgb(hex)),
            5 => Some(Color::rgba(
                (((hex >> 12) * 17) << 24)
                    | ((((hex >> 8) & 15) * 17) << 16)
                    | ((((hex >> 4) & 15) * 17) << 8)
                    | ((hex & 15) * 17),
            )),
            9 => Some(Color::rgba(hex)),
            _ => None,
        };
    }
    if let Some(args) = value
        .strip_prefix("rgb(")
        .or_else(|| value.strip_prefix("rgba("))
        .and_then(|args| args.strip_suffix(')'))
    {
        let mut parts = args
            .split(|c: char| matches!(c, ',' | '/') || c.is_ascii_whitespace())
            .filter(|part| !part.is_empty());
        let component = |value: &str| {
            value.strip_suffix('%').map_or_else(
                || bounded(value, 0.0, 255.0).map(|value| value / 255.0),
                |value| bounded(value, 0.0, 100.0).map(|value| value / 100.0),
            )
        };
        let red = component(parts.next()?)?;
        let green = component(parts.next()?)?;
        let blue = component(parts.next()?)?;
        let alpha = match parts.next() {
            Some(value) => value.strip_suffix('%').map_or_else(
                || bounded(value, 0.0, 1.0),
                |value| bounded(value, 0.0, 100.0).map(|value| value / 100.0),
            )?,
            None => 1.0,
        };
        return parts.next().is_none().then_some(Color {
            red,
            green,
            blue,
            alpha,
        });
    }
    Some(match value {
        "transparent" => Color::TRANSPARENT,
        "black" => Color::rgb(0x000000),
        "white" => Color::rgb(0xffffff),
        "red" => Color::rgb(0xff0000),
        "green" => Color::rgb(0x008000),
        "blue" => Color::rgb(0x0000ff),
        "gray" | "grey" => Color::rgb(0x808080),
        "yellow" => Color::rgb(0xffff00),
        "navy" => Color::rgb(0x000080),
        "silver" => Color::rgb(0xc0c0c0),
        "orange" => Color::rgb(0xffa500),
        "purple" => Color::rgb(0x800080),
        "rebeccapurple" => Color::rgb(0x663399),
        "teal" => Color::rgb(0x008080),
        _ => return None,
    })
}

fn box_sides<T: Copy>(value: &str, parse: impl Fn(&str) -> Option<T>) -> Option<[T; 4]> {
    let mut values = value.split_ascii_whitespace();
    let top = parse(values.next()?)?;
    let right = values.next().map(&parse).unwrap_or(Some(top))?;
    let bottom = values.next().map(&parse).unwrap_or(Some(top))?;
    let left = values.next().map(&parse).unwrap_or(Some(right))?;
    values
        .next()
        .is_none()
        .then_some([top, right, bottom, left])
}

pub(crate) fn parse_inline(
    source: &str,
    tokens: &HashMap<String, String>,
    custom_fonts: &[FontFace],
) -> Vec<Declared> {
    if source.len() > 4096 {
        return Vec::new();
    }
    let source = apply_tokens(&strip_comments(source), tokens);
    parse_declarations(&source, custom_fonts)
}

fn parse_declarations(source: &str, custom_fonts: &[FontFace]) -> Vec<Declared> {
    use Declaration as D;
    use display_list::FontFamily;
    let mut declarations = Vec::new();
    let mut expanded = Vec::new();
    for part in split_top_level(source, |character| character == ';').take(128) {
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();
        let importance = value
            .rsplit_once('!')
            .filter(|(_, flag)| flag.trim() == "important");
        let value = importance.map_or(value.as_str(), |(value, _)| value.trim_end());
        let declaration = match name.as_str() {
            "display" => match value {
                "none" => Some(D::Display(Display::None)),
                "inline" => Some(D::Display(Display::Inline)),
                "inline-block" => Some(D::Display(Display::InlineBlock)),
                "block" | "flow-root" | "list-item" => Some(D::Display(Display::Block)),
                "grid" | "inline-grid" => Some(D::Display(Display::Grid)),
                "flex" | "inline-flex" => Some(D::Display(Display::Flex)),
                _ => None,
            },
            "flex-direction" if matches!(value, "row" | "column") => {
                Some(D::FlexColumn(value == "column"))
            }
            "flex-wrap" => match value {
                "wrap" | "wrap-reverse" => Some(D::FlexWrap(true)),
                "nowrap" => Some(D::FlexWrap(false)),
                _ => None,
            },
            "flex-grow" => bounded(value, 0.0, 1024.0).map(D::FlexGrow),
            "flex-shrink" => bounded(value, 0.0, 1024.0).map(D::FlexShrink),
            "flex" => {
                let mut values = value.split_ascii_whitespace();
                let (factors, basis_opt) =
                    match (values.next(), values.next(), values.next(), values.next()) {
                        (Some("none"), None, None, None) => (Some((0.0, 0.0)), None),
                        (Some("auto"), None, None, None) => (Some((1.0, 1.0)), None),
                        (Some(grow), None, None, None) => {
                            (bounded(grow, 0.0, 1024.0).map(|g| (g, 1.0)), None)
                        }
                        (Some(grow), Some(shrink), None, None) => (
                            bounded(grow, 0.0, 1024.0).zip(bounded(shrink, 0.0, 1024.0)),
                            None,
                        ),
                        (Some(grow), Some(shrink), Some(basis), None) => (
                            bounded(grow, 0.0, 1024.0).zip(bounded(shrink, 0.0, 1024.0)),
                            Some(basis),
                        ),
                        _ => (None, None),
                    };
                if let Some((grow, shrink)) = factors {
                    expanded.extend([D::FlexGrow(grow), D::FlexShrink(shrink)]);
                }
                if let Some(basis) = basis_opt
                    && let Some(basis_len) = length(basis)
                    && basis_len != Length::Auto
                {
                    expanded.push(D::Width(basis_len));
                }
                None
            }
            "flex-basis" => length(value).filter(|l| *l != Length::Auto).map(D::Width),
            "position" => match value {
                "relative" => Some(D::Position(Position::Relative)),
                "absolute" | "fixed" => Some(D::Position(Position::Absolute)),
                "static" | "sticky" => Some(D::Position(Position::Static)),
                _ => None,
            },
            "top" => length(value).map(|value| D::Inset(0, value)),
            "right" => length(value).map(|value| D::Inset(1, value)),
            "bottom" => length(value).map(|value| D::Inset(2, value)),
            "left" => length(value).map(|value| D::Inset(3, value)),
            "opacity" => bounded(value, 0.0, 1.0).map(D::Opacity),
            "grid-template-columns" => grid_columns(value).map(D::GridColumns),
            "gap" => {
                if let Some([row, column, _, _]) = box_sides(value, padding)
                    && value.split_ascii_whitespace().count() <= 2
                {
                    expanded.extend([D::Gap(0, row), D::Gap(1, column)]);
                }
                None
            }
            "row-gap" => padding(value).map(|value| D::Gap(0, value)),
            "column-gap" => padding(value).map(|value| D::Gap(1, value)),
            "align-items" => match value {
                "center" => Some(D::Align(AlignItems::Center)),
                "end" | "flex-end" => Some(D::Align(AlignItems::End)),
                "start" | "flex-start" | "normal" | "stretch" => Some(D::Align(AlignItems::Start)),
                _ => None,
            },
            "min-width" => padding(value).map(D::MinWidth),
            "border-width" => px(value, 64.0).map(D::BorderWidth),
            "border-color" => color(value).map(D::BorderColor),
            "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
                if let Some((width, color)) = border(value) {
                    expanded.extend([D::BorderWidth(width), D::BorderColor(color)]);
                }
                None
            }
            "justify-content" => match value {
                "start" | "flex-start" | "normal" => Some(D::JustifyContent(JustifyContent::Start)),
                "center" => Some(D::JustifyContent(JustifyContent::Center)),
                "end" | "flex-end" => Some(D::JustifyContent(JustifyContent::End)),
                "space-between" => Some(D::JustifyContent(JustifyContent::SpaceBetween)),
                _ => None,
            },
            "color" => color(value).map(D::Color),
            "background-color" | "background" => color(value).map(D::Background),
            "font-size" => length(value)
                .filter(|value| *value != Length::Auto)
                .map(D::FontSize),
            "font-weight" => match value {
                "normal" => Some(D::FontWeight(400)),
                "bold" => Some(D::FontWeight(700)),
                _ => value
                    .parse()
                    .ok()
                    .filter(|weight| (100..=900).contains(weight))
                    .map(D::FontWeight),
            },
            "font-family" => value.split(',').find_map(|family| {
                let name = family.trim().trim_matches(['\'', '"']).trim();
                if let Some(index) = custom_fonts
                    .iter()
                    .position(|face| face.family.eq_ignore_ascii_case(name))
                {
                    return Some(D::FontFamily(FontFamily::custom(index as u32)));
                }
                match name {
                    "serif" | "times new roman" | "georgia" => {
                        Some(D::FontFamily(FontFamily::Serif))
                    }
                    "monospace" | "consolas" | "courier" | "courier new" => {
                        Some(D::FontFamily(FontFamily::Monospace))
                    }
                    "sans-serif" | "system-ui" | "segoe ui" | "arial" => {
                        Some(D::FontFamily(FontFamily::SansSerif))
                    }
                    _ => None,
                }
            }),
            "font-style" if matches!(value, "normal" | "italic" | "oblique") => {
                Some(D::Italic(value != "normal"))
            }
            "text-decoration" | "text-decoration-line" => {
                let underline = value
                    .split_ascii_whitespace()
                    .any(|word| word == "underline");
                let strikethrough = value
                    .split_ascii_whitespace()
                    .any(|word| word == "line-through");
                (value == "none" || underline || strikethrough).then_some(D::TextDecoration {
                    underline,
                    strikethrough,
                })
            }
            "list-style" | "list-style-type" if matches!(value, "none" | "disc" | "decimal") => {
                Some(D::HideListMarker(value == "none"))
            }
            "text-align" => match value {
                "start" | "left" => Some(D::TextAlign(TextAlign::Start)),
                "center" => Some(D::TextAlign(TextAlign::Center)),
                "end" | "right" => Some(D::TextAlign(TextAlign::End)),
                _ => None,
            },
            "white-space" => match value {
                "normal" => Some(D::WhiteSpace(WhiteSpace::Normal)),
                "nowrap" => Some(D::WhiteSpace(WhiteSpace::NoWrap)),
                "pre" => Some(D::WhiteSpace(WhiteSpace::Pre)),
                "pre-wrap" => Some(D::WhiteSpace(WhiteSpace::PreWrap)),
                "pre-line" => Some(D::WhiteSpace(WhiteSpace::PreLine)),
                _ => None,
            },
            "text-transform" => match value {
                "uppercase" => Some(D::TextTransform(TextTransform::Uppercase)),
                "lowercase" => Some(D::TextTransform(TextTransform::Lowercase)),
                "capitalize" => Some(D::TextTransform(TextTransform::Capitalize)),
                "none" => Some(D::TextTransform(TextTransform::None)),
                _ => None,
            },
            "line-height" => {
                if let Some(num) = bounded(value, 0.5, 10.0) {
                    Some(D::LineHeight(Length::Em(num)))
                } else if value == "normal" {
                    Some(D::LineHeight(Length::Auto))
                } else {
                    length(value).filter(|l| *l != Length::Auto).map(D::LineHeight)
                }
            }
            "width" => length(value).map(D::Width),
            "max-width" => length(value).map(D::MaxWidth),
            "height" => length(value).map(D::Height),
            "min-height" => length(value).map(D::MinHeight),
            "box-sizing" if matches!(value, "border-box" | "content-box") => {
                Some(D::BorderBox(value == "border-box"))
            }
            "overflow" | "overflow-x" | "overflow-y"
                if matches!(
                    value,
                    "hidden" | "clip" | "visible" | "auto" | "scroll"
                ) =>
            {
                Some(D::OverflowClip(value != "visible"))
            }
            "visibility" if matches!(value, "visible" | "hidden" | "collapse") => {
                Some(D::Visibility(value == "visible"))
            }
            "clip-path" if matches!(value, "inset(50%)" | "inset(50% 50% 50% 50%)") => {
                Some(D::ClipHidden)
            }
            "clip"
                if value.starts_with("rect(")
                    && value
                        .strip_prefix("rect(")
                        .and_then(|value| value.strip_suffix(')'))
                        .is_some_and(|value| {
                            value
                                .split(|c: char| c == ',' || c.is_ascii_whitespace())
                                .filter(|value| !value.is_empty())
                                .all(|value| matches!(value, "0" | "0px"))
                        }) =>
            {
                Some(D::ClipHidden)
            }
            "border-radius" => {
                if value == "50%" || value == "100%" {
                    Some(D::Radius(9999.0))
                } else if let Some(px_val) = value.strip_suffix("px") {
                    px_val.parse::<f32>().ok().map(|r| D::Radius(r.clamp(0.0, 9999.0)))
                } else if let Some(em_val) = value.strip_suffix("em").or_else(|| value.strip_suffix("rem")) {
                    em_val.parse::<f32>().ok().map(|r| D::Radius((r * 16.0).clamp(0.0, 9999.0)))
                } else if value == "0" {
                    Some(D::Radius(0.0))
                } else {
                    None
                }
            }
            "margin" => {
                if let Some(sides) = box_sides(value, length) {
                    expanded.extend(
                        sides
                            .into_iter()
                            .enumerate()
                            .map(|(side, value)| D::Margin(side, value)),
                    );
                }
                None
            }
            "padding" => {
                if let Some(sides) = box_sides(value, padding) {
                    expanded.extend(
                        sides
                            .into_iter()
                            .enumerate()
                            .map(|(side, value)| D::Padding(side, value)),
                    );
                }
                None
            }
            "margin-top" => length(value).map(|value| D::Margin(0, value)),
            "margin-right" => length(value).map(|value| D::Margin(1, value)),
            "margin-bottom" => length(value).map(|value| D::Margin(2, value)),
            "margin-left" => length(value).map(|value| D::Margin(3, value)),
            "margin-inline-start" => length(value).map(|value| D::MarginInline(true, value)),
            "margin-inline-end" => length(value).map(|value| D::MarginInline(false, value)),
            "padding-top" => padding(value).map(|value| D::Padding(0, value)),
            "padding-right" => padding(value).map(|value| D::Padding(1, value)),
            "padding-bottom" => padding(value).map(|value| D::Padding(2, value)),
            "padding-left" => padding(value).map(|value| D::Padding(3, value)),
            _ => None,
        };
        expanded.extend(declaration);
        declarations.extend(expanded.drain(..).map(|value| Declared {
            value,
            important: importance.is_some(),
        }));
    }
    declarations
}

#[cfg(test)]
mod tests {
    use super::*;
    use display_list::FontFamily;

    #[test]
    fn parses_font_face_and_maps_custom_family() {
        let css = r#"
            @font-face {
                font-family: 'Open Sans';
                src: url('opensans.woff2');
            }
            @font-face {
                font-family: "Robo-Font";
                src: url("fonts/roboto.ttf") format("truetype");
            }
            h1 {
                font-family: 'Open Sans', sans-serif;
            }
            p {
                font-family: "Robo-Font", monospace;
            }
            code {
                font-family: Consolas, monospace;
            }
        "#;
        let faces = parse_font_faces(css);
        assert_eq!(faces.len(), 2);
        assert_eq!(faces[0].family, "Open Sans");
        assert_eq!(faces[0].src, "opensans.woff2");
        assert_eq!(faces[1].family, "Robo-Font");
        assert_eq!(faces[1].src, "fonts/roboto.ttf");

        let (rules, _) = parse_stylesheet(
            css,
            Viewport {
                width: 800.0,
                height: 600.0,
            },
        )
        .unwrap();
        assert_eq!(rules.len(), 3);

        // Find rule for h1: font-family should be custom(0)
        let h1_decl = rules
            .iter()
            .find(|r| r.selector.parts[0].tag.as_deref() == Some("h1"))
            .unwrap();
        let h1_family = h1_decl
            .declarations
            .iter()
            .find_map(|d| match d.value {
                Declaration::FontFamily(f) => Some(f),
                _ => None,
            })
            .unwrap();
        assert_eq!(h1_family, FontFamily::custom(0));

        // Find rule for p: font-family should be custom(1)
        let p_decl = rules
            .iter()
            .find(|r| r.selector.parts[0].tag.as_deref() == Some("p"))
            .unwrap();
        let p_family = p_decl
            .declarations
            .iter()
            .find_map(|d| match d.value {
                Declaration::FontFamily(f) => Some(f),
                _ => None,
            })
            .unwrap();
        assert_eq!(p_family, FontFamily::custom(1));

        // Find rule for code: font-family should be Monospace
        let code_decl = rules
            .iter()
            .find(|r| r.selector.parts[0].tag.as_deref() == Some("code"))
            .unwrap();
        let code_family = code_decl
            .declarations
            .iter()
            .find_map(|d| match d.value {
                Declaration::FontFamily(f) => Some(f),
                _ => None,
            })
            .unwrap();
        assert_eq!(code_family, FontFamily::Monospace);
    }

    #[test]
    fn custom_properties_preserves_first_entry() {
        let css = r#"
            :root {
                --color-base: #202122;
                --theme: light;
            }
            @media (prefers-color-scheme: dark) {
                :root {
                    --color-base: #ffffff;
                    --dark-only: yes;
                }
            }
            body {
                color: var(--color-base);
            }
        "#;
        let tokens = custom_properties(css);
        assert_eq!(
            tokens.get("--color-base").map(String::as_str),
            Some("#202122")
        );
        assert_eq!(tokens.get("--theme").map(String::as_str), Some("light"));
        assert_eq!(tokens.get("--dark-only").map(String::as_str), Some("yes"));

        let (rules, _) = parse_stylesheet(
            css,
            Viewport {
                width: 800.0,
                height: 600.0,
            },
        )
        .unwrap();
        let body_rule = rules
            .iter()
            .find(|r| r.selector.parts[0].tag.as_deref() == Some("body"))
            .unwrap();
        let color_decl = body_rule
            .declarations
            .iter()
            .find_map(|d| match d.value {
                Declaration::Color(c) => Some(c),
                _ => None,
            })
            .unwrap();
        assert_eq!(color_decl, Color::rgb(0x202122));
    }

    #[test]
    fn parses_flex_shorthand_variants() {
        // Test flex: 1
        let decls_grow_only = parse_inline("flex: 1", &HashMap::new(), &[]);
        let grow = decls_grow_only.iter().find_map(|d| match d.value {
            Declaration::FlexGrow(g) => Some(g),
            _ => None,
        });
        let shrink = decls_grow_only.iter().find_map(|d| match d.value {
            Declaration::FlexShrink(s) => Some(s),
            _ => None,
        });
        assert_eq!(grow, Some(1.0));
        assert_eq!(shrink, Some(1.0));

        // Test flex: 1 1 46%
        let decls_with_basis = parse_inline("flex: 1 1 46%", &HashMap::new(), &[]);
        let grow2 = decls_with_basis.iter().find_map(|d| match d.value {
            Declaration::FlexGrow(g) => Some(g),
            _ => None,
        });
        let shrink2 = decls_with_basis.iter().find_map(|d| match d.value {
            Declaration::FlexShrink(s) => Some(s),
            _ => None,
        });
        let width2 = decls_with_basis.iter().find_map(|d| match d.value {
            Declaration::Width(w) => Some(w),
            _ => None,
        });
        assert_eq!(grow2, Some(1.0));
        assert_eq!(shrink2, Some(1.0));
        assert_eq!(width2, Some(Length::Percent(46.0)));

        // Test flex: 2 0 200px
        let decls_fixed_basis = parse_inline("flex: 2 0 200px", &HashMap::new(), &[]);
        let grow3 = decls_fixed_basis.iter().find_map(|d| match d.value {
            Declaration::FlexGrow(g) => Some(g),
            _ => None,
        });
        let shrink3 = decls_fixed_basis.iter().find_map(|d| match d.value {
            Declaration::FlexShrink(s) => Some(s),
            _ => None,
        });
        let width3 = decls_fixed_basis.iter().find_map(|d| match d.value {
            Declaration::Width(w) => Some(w),
            _ => None,
        });
        assert_eq!(grow3, Some(2.0));
        assert_eq!(shrink3, Some(0.0));
        assert_eq!(width3, Some(Length::Px(200.0)));
    }

    #[test]
    fn parses_inline_block_display() {
        let decls = parse_inline("display: inline-block", &HashMap::new(), &[]);
        let display = decls.iter().find_map(|d| match d.value {
            Declaration::Display(d) => Some(d),
            _ => None,
        });
        assert_eq!(display, Some(Display::InlineBlock));
        assert!(!Display::InlineBlock.is_block());
        assert!(Display::InlineBlock.is_inline_block());
    }

    #[test]
    fn parses_attribute_selectors_and_pseudos() {
        let sel_exact = parse_selector("a[target='_blank']").unwrap();
        assert_eq!(
            sel_exact.parts[0].attributes,
            vec![("target".to_string(), AttrMatch::Exact("_blank".to_string()))]
        );

        let sel_prefix = parse_selector("[href^='https://']").unwrap();
        assert_eq!(
            sel_prefix.parts[0].attributes,
            vec![("href".to_string(), AttrMatch::Prefix("https://".to_string()))]
        );

        let sel_substring = parse_selector("div[class*='card-item']").unwrap();
        assert_eq!(
            sel_substring.parts[0].attributes,
            vec![(
                "class".to_string(),
                AttrMatch::Substring("card-item".to_string())
            )]
        );

        let sel_pseudo = parse_selector("button:disabled").unwrap();
        assert!(sel_pseudo.parts[0].disabled);

        let sel_hover = parse_selector("a:hover").unwrap();
        assert_eq!(sel_hover.parts[0].tag, Some("a".to_string()));

        let sel_before = parse_selector("div::before").unwrap();
        assert_eq!(sel_before.parts[0].tag, Some("div".to_string()));
    }

    #[test]
    fn parses_text_transform_line_height_and_pill_radius() {
        let decls = parse_inline(
            "text-transform: uppercase; line-height: 1.5; flex-wrap: wrap; border-radius: 50%",
            &HashMap::new(),
            &[],
        );
        let tt = decls.iter().find_map(|d| match d.value {
            Declaration::TextTransform(t) => Some(t),
            _ => None,
        });
        let lh = decls.iter().find_map(|d| match d.value {
            Declaration::LineHeight(l) => Some(l),
            _ => None,
        });
        let fw = decls.iter().find_map(|d| match d.value {
            Declaration::FlexWrap(w) => Some(w),
            _ => None,
        });
        let r = decls.iter().find_map(|d| match d.value {
            Declaration::Radius(r) => Some(r),
            _ => None,
        });
        assert_eq!(tt, Some(TextTransform::Uppercase));
        assert_eq!(lh, Some(Length::Em(1.5)));
        assert_eq!(fw, Some(true));
        assert_eq!(r, Some(9999.0));
    }
}

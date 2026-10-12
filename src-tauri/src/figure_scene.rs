//! Diagrams the writing assistant draws for PowerPoint. The model answers
//! with a scene: boxes with text, lines between them and framed groups, in
//! centimetres from the top left, at the size the figure is printed. The
//! scene is checked, laid out on a slide of its own size and written as a
//! PowerPoint file of native shapes and connectors, which stay editable
//! there. An SVG drawing of it serves as the preview when no PDF can be
//! made from the file.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::{Cursor, Write as _};

use base64::Engine as _;
use biwrite_core::assist::SkillIcon;
use serde::Deserialize;

/// EMU (the unit of PowerPoint files) per centimetre.
const EMU_PER_CM: f64 = 360_000.0;
/// Space around the drawing on the slide, in centimetres.
const MARGIN: f64 = 0.15;
/// The smallest slide side PowerPoint allows (one inch), in centimetres.
const MIN_SIDE: f64 = 2.54;
/// The largest drawing, in centimetres.
const MAX_SIDE: f64 = 60.0;
/// Most nodes, lines and groups of a scene, each.
const MAX_ITEMS: usize = 300;
/// Text size when the scene gives none, in points.
const FONT_SIZE: f64 = 8.0;
/// Thickness of every block arrow, in centimetres: one size throughout, so
/// the size says nothing about rank.
const ARROW: f64 = 0.3;
/// Space between a block arrow and the boxes it joins, in centimetres.
const ARROW_GAP: f64 = 0.06;
/// The longest gap a connection crosses as a block arrow unless the scene
/// says so, in centimetres; longer ones are lines.
const ARROW_REACH: f64 = 3.0;

/// What the model answers with.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    /// The figure's file name (letters, digits, `-` and `_`).
    #[serde(default)]
    pub name: String,
    /// It spans both columns of a two-column paper.
    #[serde(default)]
    pub wide: bool,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub groups: Vec<Group>,
    /// Icons the model drew, by name.
    #[serde(default)]
    pub svgs: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub shape: Shape,
    pub fill: Option<String>,
    pub line: Option<String>,
    #[serde(default)]
    pub dashed: bool,
    pub font_size: Option<f64>,
    #[serde(default)]
    pub bold: bool,
    pub color: Option<String>,
    #[serde(default)]
    pub align: Align,
    /// An icon in place of the shape: an id of the skill's icons or a name
    /// of the scene's `svgs`.
    pub icon: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    #[default]
    Rect,
    Rounded,
    Ellipse,
    Diamond,
    Parallelogram,
    Cylinder,
    Hexagon,
    /// Text alone, without a frame.
    Text,
    /// Native block arrows.
    #[serde(rename = "rightArrow", alias = "rightarrow")]
    RightArrow,
    #[serde(rename = "leftArrow", alias = "leftarrow")]
    LeftArrow,
    #[serde(rename = "upArrow", alias = "uparrow")]
    UpArrow,
    #[serde(rename = "downArrow", alias = "downarrow")]
    DownArrow,
    #[serde(rename = "leftRightArrow", alias = "leftrightarrow")]
    LeftRightArrow,
    #[serde(rename = "upDownArrow", alias = "updownarrow")]
    UpDownArrow,
    /// Any other name: drawn as a rectangle.
    #[serde(other)]
    Other,
}

impl Shape {
    /// PowerPoint's preset geometry.
    fn preset(self) -> &'static str {
        match self {
            Self::Rect | Self::Text | Self::Other => "rect",
            Self::Rounded => "roundRect",
            Self::Ellipse => "ellipse",
            Self::Diamond => "diamond",
            Self::Parallelogram => "parallelogram",
            Self::Cylinder => "can",
            Self::Hexagon => "hexagon",
            Self::RightArrow => "rightArrow",
            Self::LeftArrow => "leftArrow",
            Self::UpArrow => "upArrow",
            Self::DownArrow => "downArrow",
            Self::LeftRightArrow => "leftRightArrow",
            Self::UpDownArrow => "upDownArrow",
        }
    }

    fn is_arrow(self) -> bool {
        matches!(
            self,
            Self::RightArrow
                | Self::LeftArrow
                | Self::UpArrow
                | Self::DownArrow
                | Self::LeftRightArrow
                | Self::UpDownArrow
        )
    }

    /// The connection site of `side` (PowerPoint's order: top, left,
    /// bottom, right), for the shapes whose sites are those four.
    fn site(self, side: Side) -> Option<u32> {
        matches!(
            self,
            Self::Rect | Self::Rounded | Self::Diamond | Self::Text | Self::Other
        )
        .then_some(match side {
            Side::Top => 0,
            Side::Left => 1,
            Side::Bottom => 2,
            Side::Right => 3,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
    #[serde(other)]
    Other,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Top,
    Left,
    Bottom,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Head {
    /// An arrowhead at the end.
    #[default]
    End,
    Start,
    Both,
    None,
    #[serde(other)]
    Other,
}

/// How an edge is drawn.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    /// A native block arrow across the gap between two boxes.
    Arrow,
    /// A thin connector.
    Line,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub from: Option<String>,
    pub to: Option<String>,
    /// An arrow or a line; left out, a short connection between boxes that
    /// face each other is an arrow.
    pub kind: Option<EdgeKind>,
    /// A line from one point to another, when it joins no nodes.
    pub points: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub dashed: bool,
    #[serde(default)]
    pub head: Head,
    pub color: Option<String>,
    /// Horizontal, vertical, horizontal (between left and right sides).
    #[serde(default)]
    pub elbow: bool,
    pub from_side: Option<Side>,
    pub to_side: Option<Side>,
    pub font_size: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    #[serde(default)]
    pub label: String,
    pub fill: Option<String>,
    pub line: Option<String>,
    #[serde(default = "yes")]
    pub dashed: bool,
    pub font_size: Option<f64>,
}

fn yes() -> bool {
    true
}

/// A box on the slide, in centimetres from its top left.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Frame {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl Frame {
    fn point(self, side: Side) -> (f64, f64) {
        match side {
            Side::Top => (self.x + self.w / 2.0, self.y),
            Side::Bottom => (self.x + self.w / 2.0, self.y + self.h),
            Side::Left => (self.x, self.y + self.h / 2.0),
            Side::Right => (self.x + self.w, self.y + self.h / 2.0),
        }
    }

    fn center(self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// An icon on the slide: its SVG and the PNG programs without SVG show.
#[derive(Clone, Debug, PartialEq)]
struct Picture {
    name: String,
    svg: String,
    png: Vec<u8>,
}

/// A shape with text, ready to draw.
#[derive(Clone, Debug, PartialEq)]
struct Item {
    frame: Frame,
    shape: Shape,
    text: String,
    fill: Option<String>,
    line: Option<String>,
    dashed: bool,
    font_size: f64,
    bold: bool,
    color: String,
    align: Align,
    /// Text at the top left, as a group's label.
    top_left: bool,
    /// An icon drawn in place of the shape.
    picture: Option<Picture>,
}

/// A line, ready to draw.
#[derive(Clone, Debug, PartialEq)]
struct Line {
    start: (f64, f64),
    end: (f64, f64),
    /// The shapes and sites it is attached to, by item index.
    from: Option<(usize, u32)>,
    to: Option<(usize, u32)>,
    color: String,
    dashed: bool,
    head: Head,
    elbow: bool,
}

/// A scene checked and laid out on its slide.
#[derive(Clone, Debug, PartialEq)]
pub struct Laid {
    /// The slide, in centimetres.
    pub width: f64,
    pub height: f64,
    pub name: String,
    pub wide: bool,
    /// Groups first, then the nodes, then the labels of lines: the order
    /// they are drawn in.
    items: Vec<Item>,
    lines: Vec<Line>,
    /// Where the line labels start in `items`.
    labels_from: usize,
}

/// `#RRGGBB` (or `RRGGBB`) as `RRGGBB`.
fn color(value: Option<&str>) -> Option<String> {
    let v = value?.trim().trim_start_matches('#');
    (v.len() == 6 && v.chars().all(|c| c.is_ascii_hexdigit())).then(|| v.to_ascii_uppercase())
}

fn finite(values: &[f64]) -> bool {
    values.iter().all(|v| v.is_finite())
}

/// A file name of letters, digits, `-` and `_` from `name`.
pub fn file_stem(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c == '-' || c == '_' || c == ' ') && !out.ends_with(['-', '_']) {
            out.push(if c == ' ' { '-' } else { c });
        }
    }
    let out = out.trim_matches(['-', '_']).to_owned();
    out.chars().take(48).collect()
}

/// Text without characters a PowerPoint file cannot hold.
fn clean_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// The sides a line between `a` and `b` leaves and enters by.
fn sides(a: Frame, b: Frame) -> (Side, Side) {
    let (ax, ay) = a.center();
    let (bx, by) = b.center();
    let dx = (bx - ax) / ((a.w + b.w) / 2.0).max(0.01);
    let dy = (by - ay) / ((a.h + b.h) / 2.0).max(0.01);
    if dx.abs() >= dy.abs() {
        if dx >= 0.0 {
            (Side::Right, Side::Left)
        } else {
            (Side::Left, Side::Right)
        }
    } else if dy >= 0.0 {
        (Side::Bottom, Side::Top)
    } else {
        (Side::Top, Side::Bottom)
    }
}

/// The block arrow across the gap between `a` and `b` that face each
/// other (side by side, or one above the other, overlapping by an arrow's
/// thickness at least): its frame and shape, pointing at `b` (or both
/// ways). `None` when they do not face each other or touch.
fn block_arrow(a: Frame, b: Frame, both: bool) -> Option<(Frame, Shape)> {
    let across = |lo: f64, hi: f64, shapes: [Shape; 3]| {
        let len = hi - lo - 2.0 * ARROW_GAP;
        (len >= 0.25).then_some((len, shapes))
    };
    let (top, bottom) = (a.y.max(b.y), (a.y + a.h).min(b.y + b.h));
    if bottom - top >= ARROW {
        let cy = (top + bottom) / 2.0;
        let (x0, (len, shapes)) = if b.x >= a.x + a.w {
            (a.x + a.w, across(a.x + a.w, b.x, [Shape::RightArrow; 3])?)
        } else if a.x >= b.x + b.w {
            (b.x + b.w, across(b.x + b.w, a.x, [Shape::LeftArrow; 3])?)
        } else {
            return None;
        };
        let shape = if both {
            Shape::LeftRightArrow
        } else {
            shapes[0]
        };
        let frame = Frame {
            x: x0 + ARROW_GAP,
            y: cy - ARROW / 2.0,
            w: len,
            h: ARROW,
        };
        return Some((frame, shape));
    }
    let (left, right) = (a.x.max(b.x), (a.x + a.w).min(b.x + b.w));
    if right - left >= ARROW {
        let cx = (left + right) / 2.0;
        let (y0, (len, shapes)) = if b.y >= a.y + a.h {
            (a.y + a.h, across(a.y + a.h, b.y, [Shape::DownArrow; 3])?)
        } else if a.y >= b.y + b.h {
            (b.y + b.h, across(b.y + b.h, a.y, [Shape::UpArrow; 3])?)
        } else {
            return None;
        };
        let shape = if both { Shape::UpDownArrow } else { shapes[0] };
        let frame = Frame {
            x: cx - ARROW / 2.0,
            y: y0 + ARROW_GAP,
            w: ARROW,
            h: len,
        };
        return Some((frame, shape));
    }
    None
}

/// Largest icon a scene may draw itself, in bytes.
const MAX_SVG: usize = 30_000;
/// Pixels of the longer side of an icon's PNG.
const ICON_PNG: f32 = 256.0;

/// An SVG a scene drew, if it is one that holds nothing but drawing: no
/// script, no other document, no picture, nothing fetched from elsewhere.
fn safe_svg(svg: &str) -> Option<&str> {
    let t = svg.trim();
    let lower = t.to_ascii_lowercase();
    let fetched = [
        "href=\"http",
        "href='http",
        "href=\"//",
        "href=\"file",
        "url(http",
        "url(//",
        "@import",
    ];
    let ok = t.len() <= MAX_SVG
        && lower.starts_with("<svg")
        && lower.ends_with("</svg>")
        && !lower.contains("<script")
        && !lower.contains("<foreignobject")
        && !lower.contains("<image")
        && !has_event_handler(&lower)
        && !fetched.iter().any(|f| lower.contains(f));
    ok.then_some(t)
}

/// An attribute such as `onload=` in a tag of `svg` (lower case).
fn has_event_handler(svg: &str) -> bool {
    svg.match_indices(" on").any(|(at, _)| {
        let rest = &svg[at + 3..];
        let name: String = rest.chars().take_while(char::is_ascii_alphabetic).collect();
        !name.is_empty() && rest[name.len()..].trim_start().starts_with('=')
    })
}

/// The width over the height of `svg`, and its PNG; `None` when it does
/// not parse.
fn render_svg(svg: &str) -> Option<(f64, Vec<u8>)> {
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = ICON_PNG / size.width().max(size.height()).max(1.0);
    let w = (size.width() * scale).ceil().max(1.0) as u32;
    let h = (size.height() * scale).ceil().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let png = pixmap.encode_png().ok()?;
    Some((
        f64::from(size.width()) / f64::from(size.height()).max(0.001),
        png,
    ))
}

/// The icon a node names: one the scene drew (in `svgs`), else one of the
/// skill's `icons`; and its width over its height.
fn icon_of(
    name: &str,
    svgs: &BTreeMap<String, String>,
    icons: &[SkillIcon],
) -> Option<(Picture, f64)> {
    let (svg, png) = match svgs.get(name) {
        Some(svg) => (safe_svg(svg)?.to_owned(), None),
        None => {
            let icon = icons.iter().find(|i| i.id == name)?;
            (icon.svg.clone(), icon.png.clone())
        }
    };
    let (aspect, rendered) = render_svg(&svg)?;
    Some((
        Picture {
            name: name.to_owned(),
            svg,
            png: png.unwrap_or(rendered),
        },
        aspect,
    ))
}

/// `frame` shrunk to `aspect` (width over height), centred.
fn fit(frame: Frame, aspect: f64) -> Frame {
    if frame.w / frame.h > aspect {
        let w = frame.h * aspect;
        Frame {
            x: frame.x + (frame.w - w) / 2.0,
            w,
            ..frame
        }
    } else {
        let h = frame.w / aspect;
        Frame {
            y: frame.y + (frame.h - h) / 2.0,
            h,
            ..frame
        }
    }
}

/// The height of the label under an icon with `text` of `size` points.
fn label_height(text: &str, size: f64) -> f64 {
    text.lines().count().max(1) as f64 * size * 0.045 + 0.1
}

/// How an edge runs, before the shift onto the slide: a line from one
/// point to another (attached to the nodes and sides given), or a block
/// arrow.
enum Run {
    Line {
        start: (f64, f64),
        end: (f64, f64),
        from: Option<(usize, Side)>,
        to: Option<(usize, Side)>,
    },
    Arrow(Frame, Shape),
}

impl Scene {
    /// Read the scene from the model's JSON (inside a code fence or not).
    pub fn parse(text: &str) -> Result<Self, String> {
        let t = text.trim();
        let t = t
            .strip_prefix("```json")
            .or_else(|| t.strip_prefix("```"))
            .map_or(t, |rest| rest.trim_end().trim_end_matches("```"));
        let start = t.find('{').ok_or("the scene has no JSON object")?;
        let end = t.rfind('}').ok_or("the scene has no JSON object")?;
        serde_json::from_str(&t[start..=end])
            .map_err(|e| format!("the scene is not valid JSON: {e}"))
    }

    /// Check the scene and lay it out on a slide around it, without the
    /// skill's icons.
    #[cfg(test)]
    pub fn laid(&self) -> Result<Laid, String> {
        self.laid_with(&[])
    }

    /// Check the scene and lay it out on a slide around it; nodes may show
    /// the skill's `icons`.
    pub fn laid_with(&self, icons: &[SkillIcon]) -> Result<Laid, String> {
        if self.nodes.is_empty() {
            return Err("the scene has no nodes".into());
        }
        if self.nodes.len() > MAX_ITEMS
            || self.edges.len() > MAX_ITEMS
            || self.groups.len() > MAX_ITEMS
        {
            return Err(format!(
                "the scene has more than {MAX_ITEMS} items of a kind"
            ));
        }
        let mut ids: HashMap<&str, usize> = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if !finite(&[n.x, n.y, n.w, n.h]) || n.w <= 0.0 || n.h <= 0.0 {
                return Err(format!("node {} has no size", n.id));
            }
            if ids.insert(n.id.as_str(), i).is_some() {
                return Err(format!("two nodes are called {}", n.id));
            }
        }
        for g in &self.groups {
            if !finite(&[g.x, g.y, g.w, g.h]) || g.w <= 0.0 || g.h <= 0.0 {
                return Err("a group has no size".into());
            }
        }
        // Where each line runs, before the shift onto the slide.
        let node_frame = |i: usize| {
            let n = &self.nodes[i];
            Frame {
                x: n.x,
                y: n.y,
                w: n.w,
                h: n.h,
            }
        };
        let mut runs = Vec::new();
        for e in &self.edges {
            let run = match (&e.from, &e.to, &e.points) {
                (Some(a), Some(b), _) => {
                    let (Some(&ia), Some(&ib)) = (ids.get(a.as_str()), ids.get(b.as_str())) else {
                        return Err(format!(
                            "a line joins {a} and {b}, which are not both nodes"
                        ));
                    };
                    let (fa, fb) = (node_frame(ia), node_frame(ib));
                    // A short connection of boxes that face each other is a
                    // block arrow, unless the scene asks for a line.
                    let wants = match e.kind {
                        Some(EdgeKind::Arrow) => true,
                        Some(EdgeKind::Line) => false,
                        _ => !e.elbow && !e.dashed && e.from_side.is_none() && e.to_side.is_none(),
                    };
                    let arrow = match e.head {
                        _ if !wants => None,
                        Head::None | Head::Other => None,
                        Head::Start => block_arrow(fb, fa, false),
                        Head::Both => block_arrow(fa, fb, true),
                        Head::End => block_arrow(fa, fb, false),
                    }
                    .filter(|(f, _)| {
                        e.kind == Some(EdgeKind::Arrow) || f.w.max(f.h) <= ARROW_REACH
                    });
                    match arrow {
                        Some((frame, shape)) => Run::Arrow(frame, shape),
                        None => {
                            let (sa, sb) = sides(fa, fb);
                            let (sa, sb) = (e.from_side.unwrap_or(sa), e.to_side.unwrap_or(sb));
                            Run::Line {
                                start: fa.point(sa),
                                end: fb.point(sb),
                                from: Some((ia, sa)),
                                to: Some((ib, sb)),
                            }
                        }
                    }
                }
                (_, _, Some(points))
                    if points.len() == 2
                        && finite(&[points[0][0], points[0][1], points[1][0], points[1][1]]) =>
                {
                    Run::Line {
                        start: (points[0][0], points[0][1]),
                        end: (points[1][0], points[1][1]),
                        from: None,
                        to: None,
                    }
                }
                _ => return Err("a line has neither two nodes nor two points".into()),
            };
            runs.push(run);
        }
        // The drawing's extent.
        let mut min = (f64::MAX, f64::MAX);
        let mut max = (f64::MIN, f64::MIN);
        let mut extend = |x: f64, y: f64| {
            min = (min.0.min(x), min.1.min(y));
            max = (max.0.max(x), max.1.max(y));
        };
        for n in &self.nodes {
            extend(n.x, n.y);
            extend(n.x + n.w, n.y + n.h);
            if n.icon.is_some() && !n.text.trim().is_empty() {
                let size = n.font_size.unwrap_or(FONT_SIZE).clamp(4.0, 40.0);
                extend(n.x, n.y + n.h + 0.05 + label_height(n.text.trim(), size));
            }
        }
        for g in &self.groups {
            extend(g.x, g.y);
            extend(g.x + g.w, g.y + g.h);
        }
        for run in &runs {
            match run {
                Run::Line { start, end, .. } => {
                    extend(start.0, start.1);
                    extend(end.0, end.1);
                }
                Run::Arrow(f, _) => {
                    extend(f.x, f.y);
                    extend(f.x + f.w, f.y + f.h);
                }
            }
        }
        let (w, h) = (max.0 - min.0, max.1 - min.1);
        if w > MAX_SIDE || h > MAX_SIDE {
            return Err(format!("the drawing is larger than {MAX_SIDE} cm"));
        }
        let width = (w + 2.0 * MARGIN).max(MIN_SIDE);
        let height = (h + 2.0 * MARGIN).max(MIN_SIDE);
        // Centred on a slide that had to grow to the smallest size.
        let dx = -min.0 + (width - w) / 2.0;
        let dy = -min.1 + (height - h) / 2.0;
        let shift = |f: Frame| Frame {
            x: f.x + dx,
            y: f.y + dy,
            ..f
        };

        let mut items = Vec::new();
        for g in &self.groups {
            items.push(Item {
                frame: shift(Frame {
                    x: g.x,
                    y: g.y,
                    w: g.w,
                    h: g.h,
                }),
                shape: Shape::Rounded,
                text: clean_text(&g.label),
                fill: color(g.fill.as_deref()).or_else(|| Some("F4F6F8".into())),
                line: color(g.line.as_deref()).or_else(|| Some("8C96A0".into())),
                dashed: g.dashed,
                font_size: g.font_size.unwrap_or(FONT_SIZE).clamp(4.0, 40.0),
                bold: true,
                color: "404040".into(),
                align: Align::Left,
                top_left: true,
                picture: None,
            });
        }
        let first_node = items.len();
        let mut icon_labels = Vec::new();
        for n in &self.nodes {
            let framed = n.shape != Shape::Text;
            let frame = shift(Frame {
                x: n.x,
                y: n.y,
                w: n.w,
                h: n.h,
            });
            let font_size = n.font_size.unwrap_or(FONT_SIZE).clamp(4.0, 40.0);
            let text_color = color(n.color.as_deref()).unwrap_or_else(|| "000000".into());
            // An icon that cannot be found or read leaves the node a box.
            let icon = n.icon.as_deref().and_then(|name| {
                let found = icon_of(name, &self.svgs, icons);
                if found.is_none() {
                    log::warn!("the figure names an icon it does not have: {name}");
                }
                found
            });
            if let Some((picture, aspect)) = icon {
                let text = clean_text(n.text.trim());
                if !text.is_empty() {
                    let lh = label_height(&text, font_size);
                    icon_labels.push(Item {
                        frame: Frame {
                            x: frame.x - 0.5,
                            y: frame.y + frame.h + 0.05,
                            w: frame.w + 1.0,
                            h: lh,
                        },
                        shape: Shape::Text,
                        text,
                        fill: None,
                        line: None,
                        dashed: false,
                        font_size,
                        bold: n.bold,
                        color: text_color.clone(),
                        align: Align::Center,
                        top_left: false,
                        picture: None,
                    });
                }
                items.push(Item {
                    frame: fit(frame, aspect),
                    shape: Shape::Rect,
                    text: String::new(),
                    fill: None,
                    line: None,
                    dashed: false,
                    font_size,
                    bold: false,
                    color: text_color,
                    align: Align::Center,
                    top_left: false,
                    picture: Some(picture),
                });
                continue;
            }
            items.push(Item {
                frame,
                shape: n.shape,
                text: clean_text(&n.text),
                fill: if framed {
                    color(n.fill.as_deref()).or_else(|| Some("FFFFFF".into()))
                } else {
                    color(n.fill.as_deref())
                },
                line: if framed {
                    color(n.line.as_deref()).or_else(|| Some("404040".into()))
                } else {
                    color(n.line.as_deref())
                },
                dashed: n.dashed,
                font_size,
                bold: n.bold,
                color: text_color,
                align: n.align,
                top_left: false,
                picture: None,
            });
        }
        items.extend(icon_labels);
        let mut lines = Vec::new();
        let mut labels = Vec::new();
        for (e, run) in self.edges.iter().zip(runs) {
            let edge_color = color(e.color.as_deref()).unwrap_or_else(|| "404040".into());
            // Where the label goes: the middle of a line, or beside an arrow.
            let place: Box<dyn Fn(f64, f64) -> Frame> = match run {
                Run::Arrow(frame, shape) => {
                    let f = shift(frame);
                    items.push(Item {
                        frame: f,
                        shape,
                        text: String::new(),
                        fill: Some(edge_color),
                        line: None,
                        dashed: false,
                        font_size: FONT_SIZE,
                        bold: false,
                        color: "000000".into(),
                        align: Align::Center,
                        top_left: false,
                        picture: None,
                    });
                    if f.w >= f.h {
                        Box::new(move |lw, lh| Frame {
                            x: f.x + f.w / 2.0 - lw / 2.0,
                            y: f.y - lh - 0.03,
                            w: lw,
                            h: lh,
                        })
                    } else {
                        Box::new(move |lw, lh| Frame {
                            x: f.x + f.w + 0.05,
                            y: f.y + f.h / 2.0 - lh / 2.0,
                            w: lw,
                            h: lh,
                        })
                    }
                }
                Run::Line {
                    start,
                    end,
                    from,
                    to,
                } => {
                    let start = (start.0 + dx, start.1 + dy);
                    let end = (end.0 + dx, end.1 + dy);
                    let attach = |end: Option<(usize, Side)>| {
                        end.and_then(|(i, side)| {
                            self.nodes[i]
                                .shape
                                .site(side)
                                .map(|site| (first_node + i, site))
                        })
                    };
                    let horizontal = |s: Option<(usize, Side)>| {
                        s.is_some_and(|(_, side)| matches!(side, Side::Left | Side::Right))
                    };
                    lines.push(Line {
                        start,
                        end,
                        from: attach(from),
                        to: attach(to),
                        color: edge_color,
                        dashed: e.dashed,
                        head: e.head,
                        elbow: e.elbow && horizontal(from) && horizontal(to),
                    });
                    let mid = ((start.0 + end.0) / 2.0, (start.1 + end.1) / 2.0);
                    Box::new(move |lw, lh| Frame {
                        x: mid.0 - lw / 2.0,
                        y: mid.1 - lh / 2.0,
                        w: lw,
                        h: lh,
                    })
                }
            };
            let label = clean_text(e.label.trim());
            if !label.is_empty() {
                let size = e.font_size.unwrap_or(FONT_SIZE - 1.0).clamp(4.0, 40.0);
                let longest = label.lines().map(|l| l.chars().count()).max().unwrap_or(1);
                let lw = (longest as f64 * size * 0.022 + 0.2).max(0.6);
                let lh = label.lines().count() as f64 * size * 0.045 + 0.1;
                labels.push(Item {
                    frame: place(lw, lh),
                    shape: Shape::Text,
                    text: label,
                    fill: Some("FFFFFF".into()),
                    line: None,
                    dashed: false,
                    font_size: size,
                    bold: false,
                    color: "404040".into(),
                    align: Align::Center,
                    top_left: false,
                    picture: None,
                });
            }
        }
        let labels_from = items.len();
        items.extend(labels);
        let name = file_stem(&self.name);
        Ok(Laid {
            width,
            height,
            name: if name.is_empty() {
                "figure".into()
            } else {
                name
            },
            wide: self.wide,
            items,
            lines,
            labels_from,
        })
    }
}

fn emu(cm: f64) -> i64 {
    (cm * EMU_PER_CM).round() as i64
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// The typeface for Chinese text on this system.
fn east_asian_font() -> &'static str {
    if cfg!(target_os = "windows") {
        "Microsoft YaHei"
    } else {
        "PingFang SC"
    }
}

const NS: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;
const XML_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

fn solid(hex: &str) -> String {
    format!("<a:solidFill><a:srgbClr val=\"{hex}\"/></a:solidFill>")
}

fn outline(line: Option<&str>, dashed: bool, head: Head) -> String {
    match line {
        None => "<a:ln><a:noFill/></a:ln>".into(),
        Some(hex) => {
            let mut out = format!("<a:ln w=\"9525\">{}", solid(hex));
            if dashed {
                out.push_str("<a:prstDash val=\"dash\"/>");
            }
            if matches!(head, Head::Start | Head::Both) {
                out.push_str("<a:headEnd type=\"triangle\" w=\"med\" len=\"med\"/>");
            }
            if matches!(head, Head::End | Head::Both) {
                out.push_str("<a:tailEnd type=\"triangle\" w=\"med\" len=\"med\"/>");
            }
            out.push_str("</a:ln>");
            out
        }
    }
}

fn text_body(item: &Item) -> String {
    let anchor = if item.top_left { "t" } else { "ctr" };
    let algn = match item.align {
        Align::Left => "l",
        Align::Right => "r",
        Align::Center | Align::Other => "ctr",
    };
    let size = (item.font_size * 100.0).round() as i64;
    let bold = if item.bold { "1" } else { "0" };
    let ea = east_asian_font();
    let props = format!(
        "<a:rPr lang=\"en-US\" sz=\"{size}\" b=\"{bold}\" dirty=\"0\">{}<a:latin typeface=\"Arial\"/><a:ea typeface=\"{ea}\"/><a:cs typeface=\"Arial\"/></a:rPr>",
        solid(&item.color)
    );
    // Text alone grows with what it holds, so its size can be raised in
    // PowerPoint; a box keeps its size and the text in it.
    let fit = if item.shape == Shape::Text {
        "<a:spAutoFit/>"
    } else {
        "<a:noAutofit/>"
    };
    // A diamond or an ellipse holds text in its middle only: no inner
    // margin, and lines break where the text breaks them, never in a word.
    let narrow = matches!(item.shape, Shape::Diamond | Shape::Ellipse | Shape::Hexagon);
    let (inset, wrap) = if narrow {
        (0, "none")
    } else {
        (36000, "square")
    };
    let mut out = format!(
        "<p:txBody><a:bodyPr wrap=\"{wrap}\" lIns=\"{inset}\" tIns=\"18000\" rIns=\"{inset}\" bIns=\"18000\" anchor=\"{anchor}\" rtlCol=\"0\">{fit}</a:bodyPr><a:lstStyle/>"
    );
    let paragraphs: Vec<&str> = if item.text.is_empty() {
        vec![""]
    } else {
        item.text.split('\n').collect()
    };
    for p in paragraphs {
        let _ = write!(out, "<a:p><a:pPr algn=\"{algn}\"/>");
        if p.is_empty() {
            let _ = write!(
                out,
                "<a:endParaRPr lang=\"en-US\" sz=\"{size}\" dirty=\"0\"/>"
            );
        } else {
            let _ = write!(out, "<a:r>{props}<a:t>{}</a:t></a:r>", xml_escape(p));
        }
        out.push_str("</a:p>");
    }
    out.push_str("</p:txBody>");
    out
}

impl Laid {
    /// The slide's shapes and connectors.
    fn slide(&self) -> String {
        let mut out = format!(
            "{XML_HEAD}<p:sld {NS}><p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>"
        );
        // Shape ids start at 2 (1 is the slide's group).
        let id = |i: usize| i + 2;
        // The k-th icon's PNG and SVG are relationships 2 + 2k and 3 + 2k.
        let pictures: HashMap<usize, usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.picture.is_some())
            .enumerate()
            .map(|(k, (i, _))| (i, k))
            .collect();
        let shape = |out: &mut String, i: usize, item: &Item| {
            let f = item.frame;
            if let (Some(picture), Some(k)) = (&item.picture, pictures.get(&i)) {
                // One SVG picture with its PNG for programs without SVG.
                let _ = write!(
                    out,
                    "<p:pic><p:nvPicPr><p:cNvPr id=\"{}\" name=\"Icon {}\" descr=\"{}\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"rId{}\"><a:extLst><a:ext uri=\"{{96DAC541-7B7A-43D3-8B79-37D633B846F1}}\"><asvg:svgBlip xmlns:asvg=\"http://schemas.microsoft.com/office/drawing/2016/SVG/main\" r:embed=\"rId{}\"/></a:ext></a:extLst></a:blip><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic>",
                    id(i),
                    id(i),
                    xml_escape(&picture.name),
                    2 + 2 * k,
                    3 + 2 * k,
                    emu(f.x),
                    emu(f.y),
                    emu(f.w).max(1),
                    emu(f.h).max(1),
                );
                return;
            }
            let fill = item
                .fill
                .as_deref()
                .map_or_else(|| "<a:noFill/>".to_owned(), solid);
            let _ = write!(
                out,
                "<p:sp><p:nvSpPr><p:cNvPr id=\"{}\" name=\"Shape {}\"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr><p:spPr><a:xfrm><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm><a:prstGeom prst=\"{}\"><a:avLst/></a:prstGeom>{fill}{}</p:spPr>{}</p:sp>",
                id(i),
                id(i),
                emu(f.x),
                emu(f.y),
                emu(f.w).max(1),
                emu(f.h).max(1),
                item.shape.preset(),
                outline(item.line.as_deref(), item.dashed, Head::None),
                text_body(item)
            );
        };
        for (i, item) in self.items[..self.labels_from].iter().enumerate() {
            shape(&mut out, i, item);
        }
        for (k, line) in self.lines.iter().enumerate() {
            let (x1, y1) = (emu(line.start.0), emu(line.start.1));
            let (x2, y2) = (emu(line.end.0), emu(line.end.1));
            let mut flips = String::new();
            if x2 < x1 {
                flips.push_str(" flipH=\"1\"");
            }
            if y2 < y1 {
                flips.push_str(" flipV=\"1\"");
            }
            let mut ends = String::new();
            if let Some((i, site)) = line.from {
                let _ = write!(ends, "<a:stCxn id=\"{}\" idx=\"{site}\"/>", id(i));
            }
            if let Some((i, site)) = line.to {
                let _ = write!(ends, "<a:endCxn id=\"{}\" idx=\"{site}\"/>", id(i));
            }
            let n = id(self.items.len() + k);
            let geometry = if line.elbow && y1 != y2 {
                "bentConnector3"
            } else {
                "straightConnector1"
            };
            let _ = write!(
                out,
                "<p:cxnSp><p:nvCxnSpPr><p:cNvPr id=\"{n}\" name=\"Connector {n}\"/><p:cNvCxnSpPr>{ends}</p:cNvCxnSpPr><p:nvPr/></p:nvCxnSpPr><p:spPr><a:xfrm{flips}><a:off x=\"{}\" y=\"{}\"/><a:ext cx=\"{}\" cy=\"{}\"/></a:xfrm><a:prstGeom prst=\"{geometry}\"><a:avLst/></a:prstGeom>{}</p:spPr></p:cxnSp>",
                x1.min(x2),
                y1.min(y2),
                (x2 - x1).abs(),
                (y2 - y1).abs(),
                outline(Some(&line.color), line.dashed, line.head)
            );
        }
        for (i, item) in self.items.iter().enumerate().skip(self.labels_from) {
            shape(&mut out, i, item);
        }
        out.push_str(
            "</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>",
        );
        out
    }

    /// The PowerPoint file: one slide of the drawing's size.
    pub fn pptx(&self) -> Result<Vec<u8>, String> {
        let (cx, cy) = (emu(self.width), emu(self.height));
        let parts: Vec<(&str, String)> = vec![
            ("[Content_Types].xml", CONTENT_TYPES.to_owned()),
            ("_rels/.rels", ROOT_RELS.to_owned()),
            ("docProps/app.xml", APP.to_owned()),
            ("docProps/core.xml", CORE.to_owned()),
            (
                "ppt/presentation.xml",
                format!(
                    "{XML_HEAD}<p:presentation {NS} saveSubsetFonts=\"1\"><p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst><p:sldIdLst><p:sldId id=\"256\" r:id=\"rId2\"/></p:sldIdLst><p:sldSz cx=\"{cx}\" cy=\"{cy}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/><p:defaultTextStyle><a:lvl1pPr marL=\"0\" algn=\"l\" defTabSz=\"914400\"><a:defRPr sz=\"1800\" kern=\"1200\"><a:solidFill><a:schemeClr val=\"tx1\"/></a:solidFill><a:latin typeface=\"+mn-lt\"/><a:ea typeface=\"+mn-ea\"/><a:cs typeface=\"+mn-cs\"/></a:defRPr></a:lvl1pPr></p:defaultTextStyle></p:presentation>"
                ),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                PRESENTATION_RELS.to_owned(),
            ),
            (
                "ppt/presProps.xml",
                format!("{XML_HEAD}<p:presentationPr {NS}/>"),
            ),
            (
                "ppt/viewProps.xml",
                format!(
                    "{XML_HEAD}<p:viewPr {NS}><p:normalViewPr><p:restoredLeft sz=\"15620\"/><p:restoredTop sz=\"94660\"/></p:normalViewPr><p:gridSpacing cx=\"36000\" cy=\"36000\"/></p:viewPr>"
                ),
            ),
            (
                "ppt/tableStyles.xml",
                format!(
                    "{XML_HEAD}<a:tblStyleLst xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" def=\"{{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}}\"/>"
                ),
            ),
            ("ppt/theme/theme1.xml", THEME.to_owned()),
            ("ppt/slideMasters/slideMaster1.xml", MASTER.to_owned()),
            (
                "ppt/slideMasters/_rels/slideMaster1.xml.rels",
                MASTER_RELS.to_owned(),
            ),
            ("ppt/slideLayouts/slideLayout1.xml", LAYOUT.to_owned()),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
                LAYOUT_RELS.to_owned(),
            ),
            ("ppt/slides/slide1.xml", self.slide()),
        ];
        // The icons: a PNG and an SVG each, in the order the slide names them.
        let pictures: Vec<&Picture> = self
            .items
            .iter()
            .filter_map(|i| i.picture.as_ref())
            .collect();
        let mut rels = String::from(
            "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>",
        );
        let mut media: Vec<(String, Vec<u8>)> = Vec::new();
        for (k, picture) in pictures.iter().enumerate() {
            let n = k + 1;
            let _ = write!(
                rels,
                "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"../media/icon{n}.png\"/><Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" Target=\"../media/icon{n}.svg\"/>",
                2 + 2 * k,
                3 + 2 * k
            );
            media.push((format!("ppt/media/icon{n}.png"), picture.png.clone()));
            media.push((
                format!("ppt/media/icon{n}.svg"),
                picture.svg.clone().into_bytes(),
            ));
        }
        let slide_rels = format!(
            "{XML_HEAD}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>"
        );
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (name, body) in parts {
                zip.start_file(name, options).map_err(|e| e.to_string())?;
                zip.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
            }
            zip.start_file("ppt/slides/_rels/slide1.xml.rels", options)
                .map_err(|e| e.to_string())?;
            zip.write_all(slide_rels.as_bytes())
                .map_err(|e| e.to_string())?;
            for (name, bytes) in media {
                zip.start_file(name, options).map_err(|e| e.to_string())?;
                zip.write_all(&bytes).map_err(|e| e.to_string())?;
            }
            zip.finish().map_err(|e| e.to_string())?;
        }
        Ok(buf.into_inner())
    }

    /// The drawing as SVG, for the preview (96 pixels per inch).
    pub fn svg(&self) -> String {
        const PX: f64 = 96.0 / 2.54;
        let p = |cm: f64| format!("{:.1}", cm * PX);
        let mut out = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\" font-family=\"Arial, 'PingFang SC', 'Microsoft YaHei', sans-serif\"><defs><marker id=\"h\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" markerWidth=\"7\" markerHeight=\"7\" orient=\"auto-start-reverse\"><path d=\"M0 0L10 5L0 10z\" fill=\"context-stroke\"/></marker></defs><rect width=\"100%\" height=\"100%\" fill=\"#fff\"/>",
            p(self.width),
            p(self.height),
            p(self.width),
            p(self.height)
        );
        let paint = |hex: &Option<String>| {
            hex.as_deref()
                .map_or_else(|| "none".to_owned(), |h| format!("#{h}"))
        };
        let shape = |out: &mut String, item: &Item| {
            let f = item.frame;
            let style = format!(
                "fill=\"{}\" stroke=\"{}\" stroke-width=\"1\"{}",
                paint(&item.fill),
                paint(&item.line),
                if item.dashed {
                    " stroke-dasharray=\"4 3\""
                } else {
                    ""
                }
            );
            let (x, y, w, h) = (f.x * PX, f.y * PX, f.w * PX, f.h * PX);
            if let Some(picture) = &item.picture {
                let data = base64::engine::general_purpose::STANDARD.encode(picture.svg.as_bytes());
                let _ = write!(
                    out,
                    "<image x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" preserveAspectRatio=\"xMidYMid meet\" href=\"data:image/svg+xml;base64,{data}\"/>"
                );
                return;
            }
            let _ = match item.shape {
                Shape::Ellipse => write!(
                    out,
                    "<ellipse cx=\"{:.1}\" cy=\"{:.1}\" rx=\"{:.1}\" ry=\"{:.1}\" {style}/>",
                    x + w / 2.0,
                    y + h / 2.0,
                    w / 2.0,
                    h / 2.0
                ),
                Shape::Diamond => write!(
                    out,
                    "<polygon points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" {style}/>",
                    x + w / 2.0,
                    y,
                    x + w,
                    y + h / 2.0,
                    x + w / 2.0,
                    y + h,
                    x,
                    y + h / 2.0
                ),
                Shape::Parallelogram => {
                    let s = (w * 0.25).min(h);
                    write!(
                        out,
                        "<polygon points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" {style}/>",
                        x + s,
                        y,
                        x + w,
                        y,
                        x + w - s,
                        y + h,
                        x,
                        y + h
                    )
                }
                Shape::Hexagon => {
                    let s = (w * 0.25).min(h / 2.0);
                    write!(
                        out,
                        "<polygon points=\"{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}\" {style}/>",
                        x + s,
                        y,
                        x + w - s,
                        y,
                        x + w,
                        y + h / 2.0,
                        x + w - s,
                        y + h,
                        x + s,
                        y + h,
                        x,
                        y + h / 2.0
                    )
                }
                Shape::Cylinder => {
                    let ry = (h * 0.12).max(2.0);
                    write!(
                        out,
                        "<path d=\"M{x:.1} {:.1}v{:.1}a{:.1} {ry:.1} 0 0 0 {w:.1} 0v-{:.1}a{:.1} {ry:.1} 0 0 0 -{w:.1} 0a{:.1} {ry:.1} 0 0 0 {w:.1} 0\" {style}/>",
                        y + ry,
                        h - 2.0 * ry,
                        w / 2.0,
                        h - 2.0 * ry,
                        w / 2.0,
                        w / 2.0
                    )
                }
                Shape::Rounded => write!(
                    out,
                    "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" rx=\"{:.1}\" {style}/>",
                    (w.min(h) * 0.16).min(10.0)
                ),
                arrow if arrow.is_arrow() => write!(
                    out,
                    "<polygon points=\"{}\" {style}/>",
                    arrow_points(arrow, x, y, w, h)
                ),
                // A rectangle, text alone, and anything else.
                _ => write!(
                    out,
                    "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{w:.1}\" height=\"{h:.1}\" {style}/>"
                ),
            };
            if item.text.is_empty() {
                return;
            }
            let size = item.font_size * 96.0 / 72.0;
            let lines: Vec<&str> = item.text.split('\n').collect();
            let lead = size * 1.2;
            let (anchor, tx) = match (item.top_left, item.align) {
                (true, _) | (_, Align::Left) => ("start", x + 4.0),
                (_, Align::Right) => ("end", x + w - 4.0),
                _ => ("middle", x + w / 2.0),
            };
            let top = if item.top_left {
                y + 3.0 + size
            } else {
                y + h / 2.0 - lead * (lines.len() as f64 - 1.0) / 2.0 + size * 0.35
            };
            let _ = write!(
                out,
                "<text font-size=\"{size:.1}\" fill=\"#{}\" text-anchor=\"{anchor}\"{}>",
                item.color,
                if item.bold {
                    " font-weight=\"bold\""
                } else {
                    ""
                }
            );
            for (k, l) in lines.iter().enumerate() {
                let _ = write!(
                    out,
                    "<tspan x=\"{tx:.1}\" y=\"{:.1}\">{}</tspan>",
                    top + lead * k as f64,
                    xml_escape(l)
                );
            }
            out.push_str("</text>");
        };
        for item in &self.items[..self.labels_from] {
            shape(&mut out, item);
        }
        for line in &self.lines {
            let (x1, y1) = (line.start.0 * PX, line.start.1 * PX);
            let (x2, y2) = (line.end.0 * PX, line.end.1 * PX);
            let d = if line.elbow && (y1 - y2).abs() > 0.5 {
                let mx = (x1 + x2) / 2.0;
                format!("M{x1:.1} {y1:.1}H{mx:.1}V{y2:.1}H{x2:.1}")
            } else {
                format!("M{x1:.1} {y1:.1}L{x2:.1} {y2:.1}")
            };
            let start = matches!(line.head, Head::Start | Head::Both);
            let end = matches!(line.head, Head::End | Head::Both);
            let _ = write!(
                out,
                "<path d=\"{d}\" fill=\"none\" stroke=\"#{}\" stroke-width=\"1\"{}{}{}/>",
                line.color,
                if line.dashed {
                    " stroke-dasharray=\"4 3\""
                } else {
                    ""
                },
                if start {
                    " marker-start=\"url(#h)\""
                } else {
                    ""
                },
                if end { " marker-end=\"url(#h)\"" } else { "" }
            );
        }
        for item in &self.items[self.labels_from..] {
            shape(&mut out, item);
        }
        out.push_str("</svg>");
        out
    }
}

/// The outline of a block arrow in the box `x, y, w, h` (pixels), as
/// PowerPoint draws its presets: a shaft half as thick as the arrow and a
/// head as long as the arrow is thick.
fn arrow_points(shape: Shape, x: f64, y: f64, w: f64, h: f64) -> String {
    let horizontal = matches!(
        shape,
        Shape::RightArrow | Shape::LeftArrow | Shape::LeftRightArrow
    );
    // Along the arrow (u) and across it (v), in pixels.
    let (len, thick) = if horizontal { (w, h) } else { (h, w) };
    let head = (thick / 2.0).min(len / 2.0);
    let (lo, hi) = (thick / 4.0, thick * 3.0 / 4.0);
    let forward = matches!(shape, Shape::RightArrow | Shape::DownArrow);
    let both = matches!(shape, Shape::LeftRightArrow | Shape::UpDownArrow);
    let mut uv: Vec<(f64, f64)> = Vec::new();
    // A head at u = 0 pointing back, a shaft, and a head at u = len.
    if both || !forward {
        uv.extend([(0.0, thick / 2.0), (head, 0.0), (head, lo)]);
    } else {
        uv.extend([(0.0, lo)]);
    }
    if both || forward {
        uv.extend([
            (len - head, lo),
            (len - head, 0.0),
            (len, thick / 2.0),
            (len - head, thick),
            (len - head, hi),
        ]);
    } else {
        uv.extend([(len, lo), (len, hi)]);
    }
    if both || !forward {
        uv.extend([(head, hi), (head, thick)]);
    } else {
        uv.extend([(0.0, hi)]);
    }
    uv.iter()
        .map(|&(u, v)| {
            let (px, py) = if horizontal {
                (x + u, y + v)
            } else {
                (x + v, y + u)
            };
            format!("{px:.1},{py:.1}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="png" ContentType="image/png"/><Default Extension="svg" ContentType="image/svg+xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/><Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/><Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/><Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/><Override PartName="/ppt/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/ppt/presProps.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presProps+xml"/><Override PartName="/ppt/viewProps.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.viewProps+xml"/><Override PartName="/ppt/tableStyles.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.tableStyles+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/></Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/></Relationships>"#;

const APP: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>BiWrite</Application><Slides>1</Slides></Properties>"#;

const CORE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>Figure</dc:title><dc:creator>BiWrite</dc:creator></cp:coreProperties>"#;

const PRESENTATION_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/presProps" Target="presProps.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/viewProps" Target="viewProps.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="theme/theme1.xml"/><Relationship Id="rId6" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/tableStyles" Target="tableStyles.xml"/></Relationships>"#;

const MASTER: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:bg><p:bgRef idx="1001"><a:schemeClr val="bg1"/></p:bgRef></p:bg><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMap bg1="lt1" tx1="dk1" bg2="lt2" tx2="dk2" accent1="accent1" accent2="accent2" accent3="accent3" accent4="accent4" accent5="accent5" accent6="accent6" hlink="hlink" folHlink="folHlink"/><p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst><p:txStyles><p:titleStyle><a:lvl1pPr algn="l"><a:defRPr sz="4400" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mj-lt"/><a:ea typeface="+mj-ea"/><a:cs typeface="+mj-cs"/></a:defRPr></a:lvl1pPr></p:titleStyle><p:bodyStyle><a:lvl1pPr marL="0" algn="l"><a:defRPr sz="1800" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/><a:cs typeface="+mn-cs"/></a:defRPr></a:lvl1pPr></p:bodyStyle><p:otherStyle><a:lvl1pPr marL="0" algn="l"><a:defRPr sz="1800" kern="1200"><a:solidFill><a:schemeClr val="tx1"/></a:solidFill><a:latin typeface="+mn-lt"/><a:ea typeface="+mn-ea"/><a:cs typeface="+mn-cs"/></a:defRPr></a:lvl1pPr></p:otherStyle></p:txStyles></p:sldMaster>"#;

const MASTER_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="../theme/theme1.xml"/></Relationships>"#;

const LAYOUT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" type="blank" preserve="1"><p:cSld name="Blank"><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="0" cy="0"/><a:chOff x="0" y="0"/><a:chExt cx="0" cy="0"/></a:xfrm></p:grpSpPr></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>"#;

const LAYOUT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/></Relationships>"#;

const THEME: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="BiWrite"><a:themeElements><a:clrScheme name="Office"><a:dk1><a:srgbClr val="000000"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="Office"><a:majorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:majorFont><a:minorFont><a:latin typeface="Arial"/><a:ea typeface=""/><a:cs typeface=""/></a:minorFont></a:fontScheme><a:fmtScheme name="Office"><a:fillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:fillStyleLst><a:lnStyleLst><a:ln w="6350"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="12700"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln><a:ln w="19050"><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:ln></a:lnStyleLst><a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle><a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst><a:bgFillStyleLst><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill><a:solidFill><a:schemeClr val="phClr"/></a:solidFill></a:bgFillStyleLst></a:fmtScheme></a:themeElements><a:objectDefaults/><a:extraClrSchemeLst/></a:theme>"#;

#[cfg(test)]
#[path = "figure_scene_tests.rs"]
mod tests;

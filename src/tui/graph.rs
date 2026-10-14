//! Portable terminal graph drawing and camera transforms.
//!
//! Editable flowcharts keep their renderer-computed geometry and use terminal
//! text for labels. Other Mermaid families use a half-block raster preview.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::Span,
    widgets::Widget,
};

use crate::{
    flow::{EdgeKind, Flow, Shape},
    render::{NodeBounds, Rendered},
};

pub(super) const BACKGROUND: Color = Color::Rgb(19, 21, 27);
pub(super) const FOREGROUND: Color = Color::Rgb(229, 231, 239);
pub(super) const MUTED: Color = Color::Rgb(143, 151, 173);
pub(super) const ACCENT: Color = Color::Rgb(181, 164, 255);
const NODE: Color = Color::Rgb(135, 119, 185);
const EDGE: Color = Color::Rgb(143, 155, 180);
const CONNECT: Color = Color::Rgb(142, 208, 220);
const BASE_SCALE: f64 = 0.10;

#[derive(Clone, Copy, Debug)]
pub(super) struct Camera {
    pub pan: (f64, f64),
    pub zoom: f64,
}

impl Camera {
    fn scale(self) -> f64 {
        BASE_SCALE * self.zoom.clamp(0.10, 8.0)
    }

    fn screen(self, world: (f64, f64)) -> (f64, f64) {
        (
            (world.0 - self.pan.0) * self.scale(),
            (world.1 - self.pan.1) * self.scale() / 2.0,
        )
    }

    fn world(self, screen: (f64, f64)) -> (f64, f64) {
        (
            screen.0 / self.scale() + self.pan.0,
            screen.1 * 2.0 / self.scale() + self.pan.1,
        )
    }
}

/// Fit with a small margin and preserve the terminal's approximately 1:2 cells.
pub(super) fn fit_camera(rendered: &Rendered, area: Rect) -> ((f64, f64), f64) {
    let width = f64::from(rendered.width.max(1.0));
    let height = f64::from(rendered.height.max(1.0));
    let scale = (f64::from(area.width.saturating_sub(4)) / width)
        .min(f64::from(area.height.saturating_sub(2)) * 2.0 / height)
        .max(BASE_SCALE * 0.10);
    let zoom = (scale / BASE_SCALE).clamp(0.10, 8.0);
    let camera = Camera {
        pan: (0.0, 0.0),
        zoom,
    };
    let visible = camera.world((f64::from(area.width), f64::from(area.height)));
    (
        ((width - visible.0) / 2.0, (height - visible.1) / 2.0),
        zoom,
    )
}

pub(super) fn pan_by_cells(pan: &mut (f64, f64), zoom: f64, dx: f64, dy: f64) {
    let scale = Camera { pan: *pan, zoom }.scale();
    pan.0 += dx / scale;
    pan.1 += dy * 2.0 / scale;
}

/// Zoom about the viewport center, without jumping to the diagram's origin.
pub(super) fn zoom_at(pan: &mut (f64, f64), zoom: &mut f64, area: Rect, factor: f64) {
    let center = (f64::from(area.width) / 2.0, f64::from(area.height) / 2.0);
    let before = Camera {
        pan: *pan,
        zoom: *zoom,
    }
    .world(center);
    *zoom = (*zoom * factor).clamp(0.10, 8.0);
    let after = Camera {
        pan: *pan,
        zoom: *zoom,
    }
    .world(center);
    pan.0 += before.0 - after.0;
    pan.1 += before.1 - after.1;
}

/// Keep keyboard-selected nodes visible without moving an already-visible view.
pub(super) fn reveal_node(
    rendered: &Rendered,
    area: Rect,
    pan: &mut (f64, f64),
    zoom: f64,
    id: &str,
) {
    let Some(node) = rendered.nodes.iter().find(|node| node.id == id) else {
        return;
    };
    if area.is_empty() {
        return;
    }
    let camera = Camera { pan: *pan, zoom };
    let extent = (
        f64::from(area.width) / camera.scale(),
        f64::from(area.height) * 2.0 / camera.scale(),
    );
    let margin = (2.0 / camera.scale(), 2.0 / camera.scale());
    for (origin, size, viewport, padding, offset) in [
        (
            f64::from(node.x),
            f64::from(node.width),
            extent.0,
            margin.0,
            &mut pan.0,
        ),
        (
            f64::from(node.y),
            f64::from(node.height),
            extent.1,
            margin.1,
            &mut pan.1,
        ),
    ] {
        if size + padding * 2.0 > viewport {
            *offset = origin + (size - viewport) / 2.0;
        } else if origin < *offset + padding {
            *offset = origin - padding;
        } else if origin + size > *offset + viewport - padding {
            *offset = origin + size - viewport + padding;
        }
    }
}

pub(super) fn hit_node(
    rendered: &Rendered,
    area: Rect,
    pan: (f64, f64),
    zoom: f64,
    column: u16,
    row: u16,
) -> Option<String> {
    if !area.contains((column, row).into()) {
        return None;
    }
    let point = Camera { pan, zoom }.world((
        f64::from(column - area.x) + 0.5,
        f64::from(row - area.y) + 0.5,
    ));
    rendered
        .nodes
        .iter()
        .rev()
        .find(|node| {
            point.0 >= f64::from(node.x)
                && point.0 <= f64::from(node.x + node.width)
                && point.1 >= f64::from(node.y)
                && point.1 <= f64::from(node.y + node.height)
        })
        .map(|node| node.id.clone())
}

pub(super) struct GraphView<'a> {
    pub rendered: &'a Rendered,
    pub flow: Option<&'a Flow>,
    pub selected: Option<&'a str>,
    pub connect_from: Option<&'a str>,
    pub camera: Camera,
}

impl Widget for GraphView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let area = area.intersection(buffer.area);
        if area.is_empty() {
            return;
        }
        buffer.set_style(area, Style::default().bg(BACKGROUND).fg(FOREGROUND));
        if let Some(flow) = self.flow.filter(|_| !self.rendered.nodes.is_empty()) {
            self.draw_flow(flow, area, buffer);
        } else {
            self.draw_raster(area, buffer);
        }
    }
}

impl GraphView<'_> {
    fn draw_flow(&self, flow: &Flow, area: Rect, buffer: &mut Buffer) {
        let mut dots = Dots::new(area);
        let mut edge_labels = Vec::new();
        for edge in &flow.edges {
            let Some(from) = self.rendered.nodes.iter().find(|node| node.id == edge.from) else {
                continue;
            };
            let Some(to) = self.rendered.nodes.iter().find(|node| node.id == edge.to) else {
                continue;
            };
            let points = route_edge(from, to, &flow.direction);
            for pair in points.windows(2) {
                self.line(&mut dots, pair[0], pair[1], EDGE, edge.kind);
            }
            if edge.kind != EdgeKind::Line && points.len() >= 2 {
                let end = self.camera.screen(points[points.len() - 1]);
                let start = self.camera.screen(points[points.len() - 2]);
                let dx = end.0 - start.0;
                let dy = (end.1 - start.1) * 2.0;
                let length = dx.hypot(dy).max(0.01);
                let (ux, uy) = (dx / length, dy / length);
                for sign in [-1.0, 1.0] {
                    dots.line(
                        (end.0 * 2.0, end.1 * 4.0),
                        (
                            end.0 * 2.0 - ux * 2.8 + sign * uy * 1.5,
                            end.1 * 4.0 - uy * 2.8 - sign * ux * 1.5,
                        ),
                        EDGE,
                        EdgeKind::Arrow,
                    );
                }
            }
            if !edge.label.is_empty() {
                let middle = points.len() / 2;
                let a = points[middle.saturating_sub(1)];
                let b = points[middle];
                edge_labels.push((((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0), edge.label.as_str()));
            }
        }
        for node in &self.rendered.nodes {
            let shape = flow
                .nodes
                .iter()
                .find(|item| item.id == node.id)
                .map_or(Shape::Rectangle, |item| item.shape);
            let color = if self.connect_from == Some(node.id.as_str()) {
                CONNECT
            } else if self.selected == Some(node.id.as_str()) {
                ACCENT
            } else {
                NODE
            };
            let path = shape_points(node, shape);
            for pair in path.windows(2) {
                self.line(&mut dots, pair[0], pair[1], color, EdgeKind::Arrow);
            }
        }
        dots.paint(buffer);
        for (position, label) in edge_labels {
            let center = self.camera.screen(position);
            put_label(buffer, area, center, label, 24, MUTED);
        }
        for node in &self.rendered.nodes {
            let label = flow
                .nodes
                .iter()
                .find(|item| item.id == node.id)
                .map_or(node.id.as_str(), |item| item.label.as_str());
            let center = self.camera.screen((
                f64::from(node.x + node.width / 2.0),
                f64::from(node.y + node.height / 2.0),
            ));
            let max_width = (f64::from(node.width) * self.camera.scale())
                .floor()
                .max(0.0) as usize;
            let color = if self.connect_from == Some(node.id.as_str()) {
                CONNECT
            } else if self.selected == Some(node.id.as_str()) {
                ACCENT
            } else {
                FOREGROUND
            };
            put_label(
                buffer,
                area,
                center,
                label,
                max_width.saturating_sub(2),
                color,
            );
        }
    }

    fn line(
        &self,
        dots: &mut Dots,
        start: (f64, f64),
        end: (f64, f64),
        color: Color,
        kind: EdgeKind,
    ) {
        let start = self.camera.screen(start);
        let end = self.camera.screen(end);
        dots.line(
            (start.0 * 2.0, start.1 * 4.0),
            (end.0 * 2.0, end.1 * 4.0),
            color,
            kind,
        );
    }

    fn draw_raster(&self, area: Rect, buffer: &mut Buffer) {
        for y in 0..area.height {
            for x in 0..area.width {
                let top = self.sample((f64::from(x) + 0.5, f64::from(y) + 0.25));
                let bottom = self.sample((f64::from(x) + 0.5, f64::from(y) + 0.75));
                buffer[(area.x + x, area.y + y)]
                    .set_symbol("▀")
                    .set_fg(top)
                    .set_bg(bottom);
            }
        }
    }

    fn sample(&self, center: (f64, f64)) -> Color {
        // Four samples per terminal half-cell reduce downsampling shimmer.
        let mut rgb = [0_u32; 3];
        for (dx, dy) in [
            (-0.25, -0.125),
            (0.25, -0.125),
            (-0.25, 0.125),
            (0.25, 0.125),
        ] {
            let point = self.camera.world((center.0 + dx, center.1 + dy));
            let mut pixel = [19_u32, 21, 27];
            if point.0 >= 0.0
                && point.1 >= 0.0
                && point.0 < f64::from(self.rendered.width)
                && point.1 < f64::from(self.rendered.height)
            {
                let x = (point.0 / f64::from(self.rendered.width)
                    * f64::from(self.rendered.pixel_width)) as usize;
                let y = (point.1 / f64::from(self.rendered.height)
                    * f64::from(self.rendered.pixel_height)) as usize;
                let offset = (y * self.rendered.pixel_width as usize + x) * 4;
                if let Some(rgba) = self.rendered.pixels.get(offset..offset + 4) {
                    let alpha = u32::from(rgba[3]);
                    for channel in 0..3 {
                        pixel[channel] = (u32::from(rgba[channel])
                            + pixel[channel] * (255 - alpha) / 255)
                            .min(255);
                    }
                }
            }
            for channel in 0..3 {
                rgb[channel] += pixel[channel];
            }
        }
        Color::Rgb((rgb[0] / 4) as u8, (rgb[1] / 4) as u8, (rgb[2] / 4) as u8)
    }
}

fn route_edge(from: &NodeBounds, to: &NodeBounds, direction: &str) -> Vec<(f64, f64)> {
    let (fx, fy, fw, fh) = (
        f64::from(from.x),
        f64::from(from.y),
        f64::from(from.width),
        f64::from(from.height),
    );
    let (tx, ty, tw, th) = (
        f64::from(to.x),
        f64::from(to.y),
        f64::from(to.width),
        f64::from(to.height),
    );
    if from.id == to.id {
        return vec![
            (fx + fw, fy + fh / 2.0),
            (fx + fw + 24.0, fy + fh / 2.0),
            (fx + fw + 24.0, fy - 24.0),
            (fx + fw / 2.0, fy - 24.0),
            (fx + fw / 2.0, fy),
        ];
    }
    if matches!(direction, "LR" | "RL") {
        let forwards = tx + tw / 2.0 >= fx + fw / 2.0;
        if forwards != (direction == "LR") {
            let outside = (fy + fh).max(ty + th) + 34.0;
            return vec![
                (fx + fw / 2.0, fy + fh),
                (fx + fw / 2.0, outside),
                (tx + tw / 2.0, outside),
                (tx + tw / 2.0, ty + th),
            ];
        }
        let start = (if forwards { fx + fw } else { fx }, fy + fh / 2.0);
        let end = (if forwards { tx } else { tx + tw }, ty + th / 2.0);
        let middle = (start.0 + end.0) / 2.0;
        vec![start, (middle, start.1), (middle, end.1), end]
    } else {
        let forwards = ty + th / 2.0 >= fy + fh / 2.0;
        if forwards != (direction != "BT") {
            let outside = (fx + fw).max(tx + tw) + 34.0;
            return vec![
                (fx + fw, fy + fh / 2.0),
                (outside, fy + fh / 2.0),
                (outside, ty + th / 2.0),
                (tx + tw, ty + th / 2.0),
            ];
        }
        let start = (fx + fw / 2.0, if forwards { fy + fh } else { fy });
        let end = (tx + tw / 2.0, if forwards { ty } else { ty + th });
        let middle = (start.1 + end.1) / 2.0;
        vec![start, (start.0, middle), (end.0, middle), end]
    }
}

fn shape_points(node: &NodeBounds, shape: Shape) -> Vec<(f64, f64)> {
    let (x, y, w, h) = (
        f64::from(node.x),
        f64::from(node.y),
        f64::from(node.width),
        f64::from(node.height),
    );
    match shape {
        Shape::Diamond => vec![
            (x + w / 2.0, y),
            (x + w, y + h / 2.0),
            (x + w / 2.0, y + h),
            (x, y + h / 2.0),
            (x + w / 2.0, y),
        ],
        Shape::Circle => (0..=48)
            .map(|step| {
                let angle = f64::from(step) * std::f64::consts::TAU / 48.0;
                (
                    x + w / 2.0 + angle.cos() * w / 2.0,
                    y + h / 2.0 + angle.sin() * h / 2.0,
                )
            })
            .collect(),
        Shape::Rounded | Shape::Stadium => {
            let radius = if shape == Shape::Stadium {
                (h / 2.0).min(w / 2.0)
            } else {
                9.0_f64.min(w / 2.0).min(h / 2.0)
            };
            let mut points = Vec::new();
            for (cx, cy, start) in [
                (x + w - radius, y + radius, -90.0_f64),
                (x + w - radius, y + h - radius, 0.0),
                (x + radius, y + h - radius, 90.0),
                (x + radius, y + radius, 180.0),
            ] {
                for step in 0..=8 {
                    let angle = (start + f64::from(step) * 90.0 / 8.0).to_radians();
                    points.push((cx + radius * angle.cos(), cy + radius * angle.sin()));
                }
            }
            if let Some(first) = points.first().copied() {
                points.push(first);
            }
            points
        }
        Shape::Rectangle => vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)],
    }
}

fn text_width(text: &str) -> usize {
    Span::raw(text).width()
}

fn put_label(
    buffer: &mut Buffer,
    area: Rect,
    center: (f64, f64),
    text: &str,
    max_width: usize,
    color: Color,
) {
    if max_width == 0 || !center.0.is_finite() || !center.1.is_finite() {
        return;
    }
    let y = center.1.floor() as i64;
    if y < 0 || y >= i64::from(area.height) {
        return;
    }
    let text = text
        .replace("<br/>", " ")
        .replace("<br>", " ")
        .replace(['\n', '\r', '\t'], " ");
    let truncated = text_width(&text) > max_width;
    let limit = max_width.saturating_sub(usize::from(truncated));
    let mut label = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let next = ch.to_string();
        let next_width = text_width(&next);
        if width + next_width > limit {
            break;
        }
        label.push(ch);
        width += next_width;
    }
    if truncated {
        label.push('…');
        width += 1;
    }
    let mut x = center.0.floor() as i64 - width as i64 / 2;
    let mut visible = String::new();
    for ch in label.chars() {
        let next_width = text_width(&ch.to_string()) as i64;
        if x >= 0 && x + next_width <= i64::from(area.width) {
            visible.push(ch);
        } else if !visible.is_empty() {
            break;
        }
        x += next_width;
    }
    if visible.is_empty() {
        return;
    }
    let visible_width = text_width(&visible);
    let left = (x - visible_width as i64).max(0) as u16;
    buffer.set_stringn(
        area.x + left,
        area.y + y as u16,
        visible,
        usize::from(area.width.saturating_sub(left)),
        Style::default().fg(color).bg(BACKGROUND),
    );
}

struct Dots {
    area: Rect,
    cells: Vec<(u8, Color)>,
}

impl Dots {
    fn new(area: Rect) -> Self {
        Self {
            area,
            cells: vec![(0, EDGE); usize::from(area.width) * usize::from(area.height)],
        }
    }

    fn dot(&mut self, x: i32, y: i32, color: Color) {
        if x < 0
            || y < 0
            || x >= i32::from(self.area.width) * 2
            || y >= i32::from(self.area.height) * 4
        {
            return;
        }
        const BITS: [[u8; 4]; 2] = [[0, 1, 2, 6], [3, 4, 5, 7]];
        let cell = y as usize / 4 * usize::from(self.area.width) + x as usize / 2;
        self.cells[cell].0 |= 1 << BITS[x as usize % 2][y as usize % 4];
        self.cells[cell].1 = color;
    }

    fn line(&mut self, start: (f64, f64), end: (f64, f64), color: Color, kind: EdgeKind) {
        let Some((start, end)) = clip_line(
            start,
            end,
            f64::from(self.area.width) * 2.0 - 1.0,
            f64::from(self.area.height) * 4.0 - 1.0,
        ) else {
            return;
        };
        let (dx, dy) = (end.0 - start.0, end.1 - start.1);
        let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as usize;
        for step in 0..=steps {
            if kind == EdgeKind::Dotted && step % 6 >= 3 {
                continue;
            }
            let t = step as f64 / steps as f64;
            let (x, y) = (
                (start.0 + dx * t).round() as i32,
                (start.1 + dy * t).round() as i32,
            );
            self.dot(x, y, color);
            if kind == EdgeKind::Thick {
                if dx.abs() > dy.abs() {
                    self.dot(x, y + 1, color);
                } else {
                    self.dot(x + 1, y, color);
                }
            }
        }
    }

    fn paint(self, buffer: &mut Buffer) {
        for (index, (bits, color)) in self.cells.into_iter().enumerate() {
            if bits == 0 {
                continue;
            }
            let x = self.area.x + (index % usize::from(self.area.width)) as u16;
            let y = self.area.y + (index / usize::from(self.area.width)) as u16;
            if let Some(symbol) = char::from_u32(0x2800 + u32::from(bits)) {
                buffer[(x, y)]
                    .set_char(symbol)
                    .set_fg(color)
                    .set_bg(BACKGROUND);
            }
        }
    }
}

/// Liang–Barsky clipping bounds work before sampling even after a very long pan.
fn clip_line(
    start: (f64, f64),
    end: (f64, f64),
    width: f64,
    height: f64,
) -> Option<((f64, f64), (f64, f64))> {
    if ![start.0, start.1, end.0, end.1]
        .iter()
        .all(|value| value.is_finite())
    {
        return None;
    }
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let (mut low, mut high) = (0.0_f64, 1.0_f64);
    for (p, q) in [
        (-dx, start.0),
        (dx, width - start.0),
        (-dy, start.1),
        (dy, height - start.1),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                low = low.max(t);
            } else {
                high = high.min(t);
            }
            if low > high {
                return None;
            }
        }
    }
    Some((
        (start.0 + low * dx, start.1 + low * dy),
        (start.0 + high * dx, start.1 + high * dy),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Renderer;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn fit_hit_testing_and_centered_zoom_share_coordinates() {
        let rendered = Renderer::new()
            .render("flowchart LR\n A[Hello] --> B[World]")
            .expect("render");
        let area = Rect::new(5, 4, 100, 30);
        let (mut pan, mut zoom) = fit_camera(&rendered, area);
        let node = &rendered.nodes[0];
        let center = Camera { pan, zoom }.screen((
            f64::from(node.x + node.width / 2.0),
            f64::from(node.y + node.height / 2.0),
        ));
        assert_eq!(
            hit_node(
                &rendered,
                area,
                pan,
                zoom,
                area.x + center.0 as u16,
                area.y + center.1 as u16
            ),
            Some(node.id.clone())
        );
        assert_eq!(hit_node(&rendered, area, pan, zoom, 0, 0), None);
        let before = Camera { pan, zoom }.world((50.0, 15.0));
        zoom_at(&mut pan, &mut zoom, area, 1.5);
        let after = Camera { pan, zoom }.world((50.0, 15.0));
        assert!((before.0 - after.0).abs() < 0.0001);
        assert!((before.1 - after.1).abs() < 0.0001);
    }

    #[test]
    fn terminal_flowchart_has_readable_labels_and_braille_edges() {
        let source = "flowchart LR\n A[Start] -->|Yes| B{Ready}";
        let rendered = Renderer::new().render(source).expect("render");
        let flow = Flow::parse(source).expect("flow");
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("terminal");
        terminal
            .draw(|frame| {
                let (pan, zoom) = fit_camera(&rendered, frame.area());
                frame.render_widget(
                    GraphView {
                        rendered: &rendered,
                        flow: Some(&flow),
                        selected: Some("A"),
                        connect_from: None,
                        camera: Camera { pan, zoom },
                    },
                    frame.area(),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let output: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(output.contains("Start"));
        assert!(output.contains("Ready"));
        assert!(output.contains("Yes"));
        assert!(
            output
                .chars()
                .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
        );
        assert!(buffer.content.iter().any(|cell| cell.fg == ACCENT));
        assert!(buffer.content.iter().all(|cell| cell.bg == BACKGROUND));
    }

    #[test]
    fn partial_unicode_labels_and_extreme_pan_are_clipped() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 2));
        put_label(
            &mut buffer,
            Rect::new(0, 0, 6, 2),
            (0.0, 0.0),
            "日本語test",
            12,
            FOREGROUND,
        );
        let output: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(output.contains("test"));
        assert!(clip_line((-1e12, 2.0), (1e12, 2.0), 10.0, 10.0).is_some());
        assert!(clip_line((-1e12, -5.0), (1e12, -5.0), 10.0, 10.0).is_none());
    }

    #[test]
    fn backward_edges_take_a_distinct_route_in_every_direction() {
        let a = NodeBounds {
            id: "A".into(),
            x: 40.0,
            y: 40.0,
            width: 100.0,
            height: 50.0,
        };
        let b = NodeBounds {
            id: "B".into(),
            x: 40.0,
            y: 200.0,
            width: 100.0,
            height: 50.0,
        };
        for (from, to, direction) in [(&b, &a, "TD"), (&b, &a, "TB"), (&a, &b, "BT")] {
            let route = route_edge(from, to, direction);
            assert!(route.iter().any(|point| point.0 > 140.0));
        }
        let b = NodeBounds {
            x: 240.0,
            y: 40.0,
            ..b
        };
        for (from, to, direction) in [(&b, &a, "LR"), (&a, &b, "RL")] {
            let route = route_edge(from, to, direction);
            assert!(route.iter().any(|point| point.1 > 90.0));
        }
    }

    #[test]
    fn non_flow_diagram_has_portable_half_block_preview() {
        let rendered = Renderer::new()
            .render("sequenceDiagram\n Alice->>Bob: Hello")
            .expect("render");
        let mut buffer = Buffer::empty(Rect::new(0, 0, 50, 20));
        let (pan, zoom) = fit_camera(&rendered, buffer.area);
        GraphView {
            rendered: &rendered,
            flow: None,
            selected: None,
            connect_from: None,
            camera: Camera { pan, zoom },
        }
        .render(buffer.area, &mut buffer);
        assert!(buffer.content.iter().all(|cell| cell.symbol() == "▀"));
        assert!(buffer.content.iter().any(|cell| cell.fg != BACKGROUND));
    }
}

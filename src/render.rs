//! Mermaid layout and rasterization, independent of the UI thread.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use mermaid_rs_renderer::{
    DiagramKind, Graph, LayoutConfig, Theme, compute_layout, measure_svg_dimensions,
    parse_mermaid_strict, render::render_svg_with_dimensions,
};
use resvg::{tiny_skia, usvg};

const MAX_SOURCE_BYTES: usize = 128 * 1024;
const MAX_NODES: usize = 300;
const MAX_EDGES: usize = 1_000;
const MAX_PIXELS: u64 = 8_000_000;
const MAX_PIXEL_SIDE: f64 = 8_192.0;

/// A selectable flowchart node in logical image coordinates, before UI zoom.
#[derive(Debug, Clone)]
pub struct NodeBounds {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Fully owned output that can be sent from a rendering worker to the UI.
#[derive(Debug)]
pub struct Rendered {
    pub svg: String,
    pub tree: Arc<usvg::Tree>,
    pub width: f32,
    pub height: f32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// Premultiplied RGBA8, as required by Slint's premultiplied pixel buffer.
    pub pixels: Vec<u8>,
    pub nodes: Vec<NodeBounds>,
}

/// Reuses the system font database between edits. Construct once in the worker.
pub struct Renderer {
    options: usvg::Options<'static>,
    theme: Theme,
    layout: LayoutConfig,
}

impl Renderer {
    pub fn new() -> Self {
        let mut options = usvg::Options {
            font_family: "sans-serif".into(),
            // Diagrams never need to read arbitrary files or fetch network images.
            // Embedded icons supplied by the renderer remain supported.
            image_href_resolver: usvg::ImageHrefResolver {
                resolve_string: Box::new(|_, _| None),
                ..Default::default()
            },
            ..Default::default()
        };
        options.fontdb_mut().load_system_fonts();
        #[cfg(target_os = "windows")]
        options.fontdb_mut().set_sans_serif_family("Segoe UI");
        #[cfg(target_os = "macos")]
        options.fontdb_mut().set_sans_serif_family("Arial");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        options.fontdb_mut().set_sans_serif_family("DejaVu Sans");

        Self {
            options,
            theme: Theme {
                font_family: Theme::modern().font_family,
                font_size: 15.0,
                primary_color: "#29233d".into(),
                primary_border_color: "#a38de8".into(),
                primary_text_color: "#e5e7ef".into(),
                text_color: "#e5e7ef".into(),
                line_color: "#a6adbf".into(),
                background: "#13151b".into(),
                secondary_color: "#252938".into(),
                tertiary_color: "#1b1e27".into(),
                edge_label_background: "#13151b".into(),
                cluster_background: "#1b1e27".into(),
                cluster_border: "#333846".into(),
                sequence_actor_fill: "#29233d".into(),
                sequence_actor_border: "#a38de8".into(),
                sequence_actor_line: "#a6adbf".into(),
                sequence_note_fill: "#302b3e".into(),
                sequence_note_border: "#a38de8".into(),
                sequence_activation_fill: "#39324e".into(),
                sequence_activation_border: "#b5a4ff".into(),
                git_colors: [
                    "#685493", "#315f6b", "#49644e", "#7c5338", "#80465f", "#40597d", "#716334",
                    "#584976",
                ]
                .map(String::from),
                git_inv_colors: [
                    "#b5a4ff", "#8ed0dc", "#a6d4b3", "#e5b68e", "#e6a0bb", "#a8c4ef", "#e5d58e",
                    "#c4b4ed",
                ]
                .map(String::from),
                git_branch_label_colors: std::array::from_fn(|_| "#e5e7ef".into()),
                git_commit_label_color: "#e5e7ef".into(),
                git_commit_label_background: "#1b1e27".into(),
                git_tag_label_color: "#e5e7ef".into(),
                git_tag_label_background: "#29233d".into(),
                git_tag_label_border: "#a38de8".into(),
                pie_colors: [
                    "#685493", "#315f6b", "#49644e", "#7c5338", "#80465f", "#40597d", "#716334",
                    "#584976", "#3c6b68", "#665d7d", "#75554c", "#4e626f",
                ]
                .map(String::from),
                pie_title_text_color: "#e5e7ef".into(),
                pie_section_text_color: "#e5e7ef".into(),
                pie_legend_text_color: "#e5e7ef".into(),
                pie_stroke_color: "#13151b".into(),
                pie_outer_stroke_color: "#333846".into(),
                pie_stroke_width: 1.6,
                pie_outer_stroke_width: 1.6,
                pie_opacity: 1.0,
                ..Theme::dark()
            },
            layout: LayoutConfig {
                node_spacing: 48.0,
                rank_spacing: 64.0,
                node_padding_x: 26.0,
                node_padding_y: 16.0,
                ..LayoutConfig::default()
            },
        }
    }

    /// Returns an error for incomplete code, excessive diagrams or invalid SVG.
    /// A renderer panic cannot terminate the edit worker or discard the document.
    pub fn render(&mut self, source: &str) -> Result<Rendered, String> {
        if source.trim().is_empty() {
            return Err("Start with a diagram type, such as flowchart TD.".into());
        }
        if source.len() > MAX_SOURCE_BYTES {
            return Err("This diagram exceeds the 128 KiB editor limit.".into());
        }
        catch_unwind(AssertUnwindSafe(|| self.render_inner(source))).unwrap_or_else(|_| {
            Err(
                "The renderer could not lay out this diagram. Check its syntax or simplify it."
                    .into(),
            )
        })
    }

    fn render_inner(&self, source: &str) -> Result<Rendered, String> {
        let mut parsed = parse_mermaid_strict(source).map_err(|error| error.to_string())?;
        if parsed.graph.kind == DiagramKind::Flowchart {
            validate_flowchart_delimiters(source)?;
            decode_flowchart_labels(&mut parsed.graph);
        }
        check_complexity(&parsed.graph)?;
        let layout = compute_layout(&parsed.graph, &self.theme, &self.layout);
        if let mermaid_rs_renderer::layout::DiagramData::Error(error) = &layout.diagram {
            return Err(error.message.clone());
        }
        let dimensions = measure_svg_dimensions(&layout, &self.layout, None);
        let (width, height) = (dimensions.width, dimensions.height);
        let (pixel_width, pixel_height) = raster_size(width, height)?;

        // Explicit dimensions avoid percentage-sized SVGs for pie, mindmap,
        // gitGraph and C4. SVG, raster and hit bounds share this one layout.
        let svg =
            render_svg_with_dimensions(&layout, &self.theme, &self.layout, Some((width, height)));
        let tree = usvg::Tree::from_str(&svg, &self.options)
            .map_err(|error| format!("Could not display the diagram: {error}"))?;
        let mut pixmap = tiny_skia::Pixmap::new(pixel_width, pixel_height)
            .ok_or_else(|| "Could not allocate the diagram preview.".to_string())?;
        let transform = tiny_skia::Transform::from_scale(
            pixel_width as f32 / tree.size().width(),
            pixel_height as f32 / tree.size().height(),
        );
        resvg::render(&tree, transform, &mut pixmap.as_mut());

        let mut nodes = Vec::new();
        if layout.kind == DiagramKind::Flowchart {
            let scale = (width / dimensions.viewbox_width).min(height / dimensions.viewbox_height);
            let offset_x = (width - dimensions.viewbox_width * scale) / 2.0;
            let offset_y = (height - dimensions.viewbox_height * scale) / 2.0;
            for node in layout.nodes.values().filter(|node| !node.hidden) {
                nodes.push(NodeBounds {
                    id: node.id.clone(),
                    x: (node.x - dimensions.viewbox_x) * scale + offset_x,
                    y: (node.y - dimensions.viewbox_y) * scale + offset_y,
                    width: node.width * scale,
                    height: node.height * scale,
                });
            }
        }

        Ok(Rendered {
            svg,
            tree: Arc::new(tree),
            width,
            height,
            pixel_width,
            pixel_height,
            pixels: pixmap.take(),
            nodes,
        })
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

fn check_complexity(graph: &Graph) -> Result<(), String> {
    // Several diagram families keep elements outside the generic node map.
    let largest_collection = [
        graph.nodes.len(),
        graph.subgraphs.len(),
        graph.sequence_participants.len(),
        graph.sequence_frames.len(),
        graph.pie_slices.len(),
        graph.gantt_tasks.len(),
        graph.gitgraph.commits.len(),
        graph.c4.shapes.len(),
        graph.c4.boundaries.len(),
        graph.mindmap.nodes.len(),
        graph.timeline.events.len(),
        graph.block.as_ref().map_or(0, |block| block.nodes.len()),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    if largest_collection > MAX_NODES {
        return Err(format!(
            "Use at most {MAX_NODES} nodes or diagram elements."
        ));
    }
    if graph.edges.len().max(graph.c4.rels.len()) > MAX_EDGES {
        return Err(format!("Use at most {MAX_EDGES} connections."));
    }
    Ok(())
}

// The upstream strict parser checks block structure, but currently accepts an
// unfinished A[label as a bare node. Keep the last preview while a label is open.
fn validate_flowchart_delimiters(source: &str) -> Result<(), String> {
    let mut stack = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut started = false;
    for (line_index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        if !started {
            started = trimmed.starts_with("flowchart") || trimmed.starts_with("graph");
            if !started {
                continue;
            }
        }
        let mut characters = line.chars().peekable();
        while let Some(character) = characters.next() {
            if escaped {
                escaped = false;
                continue;
            }
            if quoted && character == '\\' {
                escaped = true;
                continue;
            }
            if character == '"' {
                quoted = !quoted;
                continue;
            }
            if quoted {
                continue;
            }
            if character == '%' && characters.peek() == Some(&'%') {
                break;
            }
            match character {
                '[' | '(' | '{' => {
                    stack.push((character, line_index + 1));
                    if stack.len() > 64 {
                        return Err("Diagram labels are nested too deeply.".into());
                    }
                }
                ']' | ')' | '}' => {
                    if let Some((opening, _)) = stack.pop()
                        && !matches!((opening, character), ('[', ']') | ('(', ')') | ('{', '}'))
                    {
                        return Err(format!(
                            "Mismatched label delimiter on line {}.",
                            line_index + 1
                        ));
                    }
                    // A>Asymmetric] legitimately closes without an opening [.
                }
                _ => {}
            }
        }
    }
    if quoted {
        return Err("Close the double quote in the diagram label.".into());
    }
    if let Some((opening, line)) = stack.last() {
        return Err(format!("Unclosed '{opening}' in a label on line {line}."));
    }
    Ok(())
}

fn decode_flowchart_labels(graph: &mut Graph) {
    for node in graph.nodes.values_mut() {
        node.label = decode_entities(&node.label);
    }
    for edge in &mut graph.edges {
        for label in [&mut edge.label, &mut edge.start_label, &mut edge.end_label]
            .into_iter()
            .flatten()
        {
            *label = decode_entities(label);
        }
    }
    for subgraph in &mut graph.subgraphs {
        subgraph.label = decode_entities(&subgraph.label);
    }
}

/// Decode Mermaid's #quot;/#34; labels after parsing, where quotes cannot alter
/// structure. Also accepts the common XML equivalents used in pasted diagrams.
fn decode_entities(label: &str) -> String {
    let mut decoded = String::with_capacity(label.len());
    let mut remaining = label;
    while !remaining.is_empty() {
        if (remaining.starts_with('#') || remaining.starts_with('&'))
            && let Some(end) = remaining.find(';').filter(|end| *end <= 12)
        {
            let entity = &remaining[1..end];
            let number = entity.strip_prefix('#').unwrap_or(entity);
            let character = match entity {
                "quot" => Some('"'),
                "apos" => Some('\''),
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "nbsp" => Some('\u{a0}'),
                _ => {
                    let code = if let Some(hex) = number
                        .strip_prefix('x')
                        .or_else(|| number.strip_prefix('X'))
                    {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        number.parse::<u32>().ok()
                    };
                    code.and_then(char::from_u32).filter(|character| {
                        !character.is_control() || matches!(character, '\n' | '\r' | '\t')
                    })
                }
            };
            if let Some(character) = character {
                decoded.push(character);
                remaining = &remaining[end + 1..];
                continue;
            }
        }
        if let Some(character) = remaining.chars().next() {
            decoded.push(character);
            remaining = &remaining[character.len_utf8()..];
        }
    }
    decoded
}

fn raster_size(width: f32, height: f32) -> Result<(u32, u32), String> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err("The diagram has invalid dimensions.".into());
    }
    let (width, height) = (f64::from(width), f64::from(height));
    // Render at 2x for sharp text while bounding a preview to 32 MB of pixels.
    let scale = 2.0_f64
        .min((MAX_PIXELS as f64 / (width * height)).sqrt())
        .min(MAX_PIXEL_SIDE / width.max(height));
    let pixel_width = (width * scale).floor().max(1.0) as u32;
    let pixel_height = (height * scale).floor().max(1.0) as u32;
    if u64::from(pixel_width) * u64::from(pixel_height) > MAX_PIXELS {
        return Err("The diagram is too large to preview.".into());
    }
    Ok((pixel_width, pixel_height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flowchart_bounds_match_raster_coordinates() {
        let rendered = Renderer::new()
            .render("flowchart LR\nA[Start] --> B[Finish]")
            .expect("valid flowchart");
        assert_eq!(rendered.nodes.len(), 2);
        assert_eq!(rendered.nodes[0].id, "A");
        assert!(rendered.nodes[0].x < rendered.nodes[1].x);
        assert_eq!(
            rendered.pixels.len(),
            rendered.pixel_width as usize * rendered.pixel_height as usize * 4
        );
        for node in &rendered.nodes {
            assert!(node.x >= 0.0 && node.y >= 0.0);
            assert!(node.width > 0.0 && node.height > 0.0);
            assert!(node.x + node.width <= rendered.width + 0.1);
            assert!(node.y + node.height <= rendered.height + 0.1);
            // Inside each rectangle must contain an opaque node, not canvas.
            let x = ((node.x + 8.0) * rendered.pixel_width as f32 / rendered.width) as usize;
            let y = ((node.y + 8.0) * rendered.pixel_height as f32 / rendered.height) as usize;
            let index = (y * rendered.pixel_width as usize + x) * 4;
            assert_eq!(rendered.pixels[index + 3], 255);
            assert_ne!(&rendered.pixels[index..index + 4], &rendered.pixels[..4]);
        }
    }

    #[test]
    fn malformed_input_returns_an_error_and_worker_can_render_again() {
        let mut renderer = Renderer::new();
        for source in [
            "",
            "notADiagram",
            "flowchart LR\nA[unfinished",
            "flowchart LR\nsubgraph S\nA-->B",
        ] {
            assert!(renderer.render(source).is_err(), "{source}");
        }
        assert!(renderer.render("flowchart TD\nA --> B").is_ok());
    }

    #[test]
    fn blank_flowchart_and_quoted_entities_are_editable() {
        let mut renderer = Renderer::new();
        assert!(
            renderer
                .render("flowchart TD")
                .expect("blank flowchart")
                .nodes
                .is_empty()
        );
        let rendered = renderer
            .render(
                "flowchart TD\nA[\"Say #quot;hi#quot; #38; #91;go#93;\"] -->|\"#34;yes#34;\"| B>Done]",
            )
            .expect("quoted and asymmetric nodes");
        assert_eq!(rendered.nodes.len(), 2);
        assert!(rendered.svg.contains("Say &quot;hi&quot; &amp; [go]"));
        assert!(rendered.svg.contains("&quot;yes&quot;"));
        assert!(!rendered.svg.contains("#quot;"));
        assert_eq!(decode_entities("&amp;lt; &#x22; #128512;"), "&lt; \" 😀");
    }

    #[test]
    fn multiple_diagram_families_render_at_their_natural_size() {
        let mut renderer = Renderer::new();
        for source in [
            "sequenceDiagram\nAlice->>Bob: Hello\nBob-->>Alice: Welcome",
            "classDiagram\nAnimal <|-- Dog\nclass Animal {\n+String name\n}",
            "stateDiagram-v2\n[*] --> Idle\nIdle --> Active : start",
            "erDiagram\nCUSTOMER ||--o{ ORDER : places",
            "pie title Work\n\"Design\" : 40\n\"Build\" : 60",
            "mindmap\n  root((Ideas))\n    Sketch\n    Build",
            "gitGraph\ncommit\nbranch develop\ncheckout develop\ncommit",
            "timeline\n  title Roadmap\n  2026 : Design : Build\n  2027 : Launch",
            "gantt\n  title Plan\n  dateFormat YYYY-MM-DD\n  section Work\n  Design :a1, 2026-01-01, 7d",
        ] {
            let rendered = renderer
                .render(source)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
            assert!(rendered.width > 50.0 && rendered.height > 20.0, "{source}");
            assert!(
                rendered.nodes.is_empty(),
                "only flowcharts expose visual edit handles"
            );
            assert!(!rendered.svg.contains("width=\"100%\""));
            assert!(
                rendered
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|rgba| rgba[3] > 0 && rgba != &rendered.pixels[..4])
            );
        }
    }

    #[test]
    fn resource_limits_are_checked_before_layout() {
        let mut renderer = Renderer::new();
        let oversized = "x".repeat(MAX_SOURCE_BYTES + 1);
        assert!(
            renderer
                .render(&oversized)
                .expect_err("limit")
                .contains("128 KiB")
        );
        let mut source = String::from("flowchart TD\n");
        for id in 0..=MAX_NODES {
            source.push_str(&format!("N{id}[Node {id}]\n"));
        }
        assert!(
            renderer
                .render(&source)
                .expect_err("node limit")
                .contains("300")
        );
        for (width, height) in [(10_000.0, 10_000.0), (1_000_000.0, 1.0), (0.5, 0.5)] {
            let (w, h) = raster_size(width, height).expect("bounded raster");
            assert!(u64::from(w) * u64::from(h) <= MAX_PIXELS);
            assert!(w > 0 && h > 0 && w <= 8192 && h <= 8192);
        }
        assert!(raster_size(f32::INFINITY, 10.0).is_err());
    }

    #[test]
    fn external_image_resolution_is_disabled() {
        let renderer = Renderer::new();
        let resolve = &renderer.options.image_href_resolver.resolve_string;
        assert!(resolve("/etc/passwd", &renderer.options).is_none());
        assert!(resolve("https://example.com/image.svg", &renderer.options).is_none());
    }
}

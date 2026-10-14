//! Fin: a native, offline Mermaid workspace.
#[cfg(test)]
mod app_tests;
use fin::document::Document;
use fin::flow::{Edge, EdgeKind, Flow, Shape};
use fin::preview::{self, Preview, Viewport};
use fin::render::{Rendered, Renderer};
use fin::{EMPTY, EXAMPLE};
use slint::{ComponentHandle, ModelRc, SharedPixelBuffer, VecModel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

slint::include_modules!();
const DEBOUNCE: Duration = Duration::from_millis(250);

struct RenderRequest {
    revision: u64,
    source: String,
}
struct RenderReply {
    revision: u64,
    result: Result<Rendered, String>,
}
struct PreviewRequest {
    generation: u64,
    viewport: Viewport,
    tree: Arc<resvg::usvg::Tree>,
}
struct PreviewReply {
    generation: u64,
    viewport: Viewport,
    result: Result<Option<Preview>, String>,
}
struct Controller {
    document: Document,
    flow: Option<Flow>,
    revision: u64,
    submitted: Option<u64>,
    last_edit: Instant,
    requests: Sender<RenderRequest>,
    replies: Receiver<RenderReply>,
    rendered: Option<Rendered>,
    preview_requests: Sender<PreviewRequest>,
    preview_replies: Receiver<PreviewReply>,
    preview_generation: u64,
    submitted_viewport: Option<Viewport>,
    fit_next: bool,
}

impl Controller {
    fn new(document: Document) -> Self {
        let (requests, incoming) = mpsc::channel::<RenderRequest>();
        let (outgoing, replies) = mpsc::channel();
        std::thread::spawn(move || {
            let mut renderer = Renderer::new();
            while let Ok(mut request) = incoming.recv() {
                // Typing can outpace layout: only render the newest queued revision.
                while let Ok(newer) = incoming.try_recv() {
                    request = newer;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    renderer.render(&request.source)
                }))
                .unwrap_or_else(|_| {
                    Err(
                        "The renderer could not process this diagram. Try simplifying the source."
                            .into(),
                    )
                });
                if outgoing
                    .send(RenderReply {
                        revision: request.revision,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        // View changes never compete with or discard a pending source layout.
        // The retained SVG tree already has fonts and embedded images resolved.
        let (preview_requests, incoming) = mpsc::channel::<PreviewRequest>();
        let (outgoing, preview_replies) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(mut request) = incoming.recv() {
                while let Ok(newer) = incoming.try_recv() {
                    request = newer;
                }
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    preview::rasterize(&request.tree, request.viewport)
                }))
                .unwrap_or_else(|_| Err("Could not redraw this view of the diagram.".into()));
                if outgoing
                    .send(PreviewReply {
                        generation: request.generation,
                        viewport: request.viewport,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            document,
            flow: None,
            revision: 0,
            submitted: None,
            last_edit: Instant::now() - DEBOUNCE,
            requests,
            replies,
            rendered: None,
            preview_requests,
            preview_replies,
            preview_generation: 0,
            submitted_viewport: None,
            fit_next: true,
        }
    }

    fn sync_document(&self, ui: &AppWindow) {
        if ui.get_source().as_str() != self.document.source {
            ui.set_source(self.document.source.as_str().into());
        }
        ui.set_dirty(self.document.is_dirty());
        ui.set_can_undo(self.document.can_undo());
        ui.set_can_redo(self.document.can_redo());
        let name = self
            .document
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled.mmd".into());
        ui.set_document_name(name.into());
    }

    fn changed(&mut self, ui: &AppWindow, immediate: bool) {
        self.revision += 1;
        self.last_edit = if immediate {
            Instant::now() - DEBOUNCE
        } else {
            Instant::now()
        };
        self.flow = None;
        ui.set_editable(false);
        ui.set_busy(true);
        ui.set_error_message("".into());
        ui.set_selected_edge(-1);
        ui.set_connect_from("".into());
        ui.set_connecting(false);
        self.sync_document(ui);
    }

    fn edit_source(&mut self, ui: &AppWindow, source: String) {
        if source != self.document.source {
            self.document.edit(source);
            self.changed(ui, false);
        }
    }

    fn mutate_flow(&mut self, ui: &AppWindow, edit: impl FnOnce(&mut Flow)) {
        if !ui.get_editable() {
            return;
        }
        if let Some(flow) = self.flow.as_mut() {
            edit(flow);
            let source = flow.to_mermaid();
            if source != self.document.source {
                self.document.edit(source);
                self.changed(ui, true);
            }
        }
    }

    fn tick(&mut self, ui: &AppWindow) {
        if self.submitted != Some(self.revision) && self.last_edit.elapsed() >= DEBOUNCE {
            self.submitted = Some(self.revision);
            ui.set_busy(true);
            if self
                .requests
                .send(RenderRequest {
                    revision: self.revision,
                    source: self.document.source.clone(),
                })
                .is_err()
            {
                ui.set_busy(false);
                ui.set_error_message(
                    "The rendering worker has stopped. Save your source and restart Fin.".into(),
                );
            }
        }
        while let Ok(reply) = self.replies.try_recv() {
            if reply.revision != self.revision {
                continue;
            }
            self.accept_render(ui, reply.result);
        }
        self.refresh_preview(ui);
    }

    fn viewport(ui: &AppWindow) -> Viewport {
        Viewport {
            x: ui.get_viewport_x(),
            y: ui.get_viewport_y(),
            width: ui.get_viewport_width(),
            height: ui.get_viewport_height(),
            zoom: ui.get_zoom(),
            scale_factor: ui.window().scale_factor(),
        }
    }

    fn refresh_preview(&mut self, ui: &AppWindow) {
        let viewport = Self::viewport(ui);
        if self.submitted_viewport != Some(viewport)
            && let Some(rendered) = &self.rendered
        {
            self.submitted_viewport = Some(viewport);
            if self
                .preview_requests
                .send(PreviewRequest {
                    generation: self.preview_generation,
                    viewport,
                    tree: Arc::clone(&rendered.tree),
                })
                .is_err()
            {
                ui.set_status_message("Save and restart Fin to restore detailed previews.".into());
            }
        }
        while let Ok(reply) = self.preview_replies.try_recv() {
            self.accept_preview(ui, reply);
        }
    }

    fn accept_preview(&self, ui: &AppWindow, reply: PreviewReply) {
        // The last valid graph stays zoomable even while its source is invalid.
        // A newer layout, pan, zoom, resize or monitor scale invalidates a reply.
        if reply.generation != self.preview_generation || reply.viewport != Self::viewport(ui) {
            return;
        }
        match reply.result {
            Ok(Some(preview)) => {
                let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &preview.pixels,
                    preview.pixel_width,
                    preview.pixel_height,
                );
                ui.set_detail(slint::Image::from_rgba8_premultiplied(buffer));
                ui.set_detail_x(preview.x);
                ui.set_detail_y(preview.y);
                ui.set_detail_width(preview.width);
                ui.set_detail_height(preview.height);
                ui.set_detail_zoom(reply.viewport.zoom);
                ui.set_detail_ready(true);
            }
            Ok(None) => ui.set_detail_ready(false),
            Err(message) => ui.set_status_message(message.into()),
        }
    }

    fn accept_render(&mut self, ui: &AppWindow, result: Result<Rendered, String>) {
        ui.set_busy(false);
        match result {
            Err(message) => {
                self.flow = None;
                ui.set_editable(false);
                ui.set_error_message(message.into());
                ui.set_status_message("Check the Mermaid source".into());
                ui.set_visual_note("PREVIEW PAUSED".into());
            }
            Ok(rendered) => {
                self.preview_generation += 1;
                self.submitted_viewport = None;
                ui.set_detail_ready(false);
                let buffer = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                    &rendered.pixels,
                    rendered.pixel_width,
                    rendered.pixel_height,
                );
                ui.set_diagram(slint::Image::from_rgba8_premultiplied(buffer));
                ui.set_diagram_width(rendered.width);
                ui.set_diagram_height(rendered.height);
                ui.set_nodes(ModelRc::new(VecModel::from(
                    rendered
                        .nodes
                        .iter()
                        .map(|n| HitNode {
                            id: n.id.as_str().into(),
                            x: n.x,
                            y: n.y,
                            w: n.width,
                            h: n.height,
                        })
                        .collect::<Vec<_>>(),
                )));
                match Flow::parse(&self.document.source) {
                    Ok(flow) => {
                        ui.set_direction(flow.direction.as_str().into());
                        ui.set_status_message(
                            format!(
                                "{} nodes · {} connections",
                                flow.nodes.len(),
                                flow.edges.len()
                            )
                            .into(),
                        );
                        ui.set_editable(true);
                        self.flow = Some(flow);
                    }
                    Err(_) => {
                        self.flow = None;
                        ui.set_editable(false);
                        ui.set_visual_note("SOURCE EDITING".into());
                        ui.set_status_message(
                            "Preview ready · Edit this diagram in the source panel".into(),
                        );
                    }
                }
                ui.set_error_message("".into());
                self.rendered = Some(rendered);
                self.sync_selection(ui);
                if self.fit_next {
                    ui.invoke_fit_diagram();
                    self.fit_next = false;
                }
            }
        }
    }

    fn sync_selection(&self, ui: &AppWindow) {
        let id = ui.get_selected_id();
        let node = self.flow.as_ref().and_then(|f| f.node(id.as_str()));
        if let Some(node) = node {
            ui.set_edit_label(node.label.as_str().into());
            ui.set_edit_shape(node.shape.index());
            let rows = self
                .flow
                .as_ref()
                .map(|flow| {
                    flow.edges
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.from == node.id || e.to == node.id)
                        .map(|(index, edge)| EdgeRow {
                            index: index as i32,
                            description: format!("{} → {}", edge.from, edge.to).into(),
                            label: edge.label.as_str().into(),
                            kind: edge.kind.index(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            ui.set_edges(ModelRc::new(VecModel::from(rows)));
        } else {
            ui.set_selected_id("".into());
            ui.set_edges(ModelRc::default());
        }
        ui.set_selected_edge(-1);
    }

    fn select_node(&mut self, ui: &AppWindow, id: String) {
        if !ui.get_editable() {
            return;
        }
        if ui.get_connecting() && !id.is_empty() {
            let from = ui.get_connect_from().to_string();
            if from.is_empty() {
                ui.set_connect_from(id.into());
                return;
            }
            self.mutate_flow(ui, |flow| {
                if flow.node(&from).is_some()
                    && flow.node(&id).is_some()
                    && !flow
                        .edges
                        .iter()
                        .any(|edge| edge.from == from && edge.to == id)
                {
                    flow.edges.push(Edge {
                        from,
                        to: id.clone(),
                        label: String::new(),
                        kind: EdgeKind::Arrow,
                    });
                }
            });
            ui.set_connect_from("".into());
            ui.set_connecting(false);
        }
        ui.set_selected_id(id.into());
        if self.flow.is_some() {
            self.sync_selection(ui);
        }
    }

    fn save(&mut self, ui: &AppWindow, save_as: bool) -> bool {
        let path = if save_as {
            None
        } else {
            self.document.path.clone()
        };
        let path = path.or_else(|| {
            rfd::FileDialog::new()
                .set_title("Save Mermaid diagram")
                .add_filter("Mermaid", &["mmd", "mermaid"])
                .set_file_name(ui.get_document_name().as_str())
                .save_file()
        });
        let Some(mut path) = path else {
            return false;
        };
        if !complete_extension(&mut path, "mmd") {
            return false;
        }
        match self.document.save(&path) {
            Ok(()) => {
                self.sync_document(ui);
                ui.set_status_message("Saved".into());
                true
            }
            Err(error) => {
                show_error(&error);
                false
            }
        }
    }

    fn confirm_replace(&mut self, ui: &AppWindow) -> bool {
        if !self.document.is_dirty() {
            return true;
        }
        match rfd::MessageDialog::new()
            .set_title("Save your changes?")
            .set_description("Save changes to this diagram before continuing?")
            .set_buttons(rfd::MessageButtons::YesNoCancel)
            .show()
        {
            rfd::MessageDialogResult::Yes => self.save(ui, false),
            rfd::MessageDialogResult::No => true,
            _ => false,
        }
    }

    fn command(&mut self, ui: &AppWindow, command: &str) {
        match command {
            "save" | "save-as" => {
                self.save(ui, command == "save-as");
            }
            "new" => {
                if self.confirm_replace(ui) {
                    self.document.reset(EMPTY.into());
                    self.reset_view(ui);
                }
            }
            "open" => {
                let Some(path) = rfd::FileDialog::new()
                    .add_filter("Mermaid", &["mmd", "mermaid", "txt"])
                    .pick_file()
                else {
                    return;
                };
                if self.confirm_replace(ui) {
                    match self.document.load(&path) {
                        Ok(()) => self.reset_view(ui),
                        Err(error) => show_error(&error),
                    }
                }
            }
            "undo" => {
                if self.document.undo() {
                    self.changed(ui, true);
                }
            }
            "redo" => {
                if self.document.redo() {
                    self.changed(ui, true);
                }
            }
            "export" => {
                if ui.get_busy() || !ui.get_error_message().is_empty() {
                    return;
                }
                let Some(rendered) = &self.rendered else {
                    return;
                };
                let Some(mut path) = rfd::FileDialog::new()
                    .add_filter("SVG image", &["svg"])
                    .set_file_name("diagram.svg")
                    .save_file()
                else {
                    return;
                };
                if !complete_extension(&mut path, "svg") {
                    return;
                }
                match Document::new(rendered.svg.clone()).save(&path) {
                    Ok(()) => ui.set_status_message("SVG exported".into()),
                    Err(error) => show_error(&error),
                }
            }
            _ => {}
        }
    }

    fn reset_view(&mut self, ui: &AppWindow) {
        ui.set_selected_id("".into());
        ui.set_connecting(false);
        ui.set_connect_from("".into());
        self.fit_next = true;
        self.changed(ui, true);
    }
}

fn show_error(error: &str) {
    rfd::MessageDialog::new()
        .set_title("Fin")
        .set_description(error)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

// The dialog approves the path it returns. Appending an extension can name a
// different, existing file, so that replacement needs its own confirmation.
fn complete_extension(path: &mut PathBuf, extension: &str) -> bool {
    if path.extension().is_some() {
        return true;
    }
    path.set_extension(extension);
    !path.exists()
        || rfd::MessageDialog::new()
            .set_title("Replace existing file?")
            .set_description(format!("{} already exists. Replace it?", path.display()))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
}

fn wire(ui: &AppWindow, controller: &Rc<RefCell<Controller>>) {
    macro_rules! bind {
        ($callback:ident, |$app:ident, $state:ident $(, $arg:ident)*| $body:block) => {{
            let weak = ui.as_weak();
            let controller = controller.clone();
            ui.$callback(move |$($arg),*| { if let Some($app) = weak.upgrade() { let mut $state = controller.borrow_mut(); $body } });
        }};
    }
    bind!(on_source_edited, |app, state, source| {
        state.edit_source(&app, source.to_string());
    });
    bind!(on_command, |app, state, command| {
        state.command(&app, command.as_str());
    });
    bind!(on_select_node, |app, state, id| {
        state.select_node(&app, id.to_string());
    });
    bind!(on_add_node, |app, state, shape| {
        let mut id = String::new();
        state.mutate_flow(&app, |flow| {
            id = flow.add_node(Shape::from_index(shape));
        });
        if !id.is_empty() {
            app.set_selected_id(id.into());
        }
    });
    bind!(on_apply_node, |app, state, label, shape| {
        let id = app.get_selected_id().to_string();
        state.mutate_flow(&app, |flow| {
            if let Some(node) = flow.node_mut(&id) {
                node.label = label.to_string();
                node.shape = Shape::from_index(shape);
            }
        });
    });
    bind!(on_delete_node, |app, state| {
        let id = app.get_selected_id().to_string();
        state.mutate_flow(&app, |flow| {
            flow.remove_node(&id);
        });
        app.set_selected_id("".into());
    });
    bind!(on_set_direction, |app, state, direction| {
        state.mutate_flow(&app, |flow| {
            if ["TD", "TB", "LR", "RL", "BT"].contains(&direction.as_str()) {
                flow.direction = direction.to_string();
            }
        });
        state.fit_next = true;
    });
    bind!(on_apply_edge, |app, state, index, label, kind| {
        state.mutate_flow(&app, |flow| {
            if let Some(edge) = usize::try_from(index)
                .ok()
                .and_then(|index| flow.edges.get_mut(index))
            {
                edge.label = label.to_string();
                edge.kind = EdgeKind::from_index(kind);
            }
        });
    });
    bind!(on_delete_edge, |app, state, index| {
        state.mutate_flow(&app, |flow| {
            if let Ok(index) = usize::try_from(index)
                && index < flow.edges.len()
            {
                flow.edges.remove(index);
            }
        });
    });
    let weak = ui.as_weak();
    let controller = controller.clone();
    ui.window().on_close_requested(move || {
        if weak
            .upgrade()
            .is_none_or(|ui| controller.borrow_mut().confirm_replace(&ui))
        {
            slint::CloseRequestResponse::HideWindow
        } else {
            slint::CloseRequestResponse::KeepWindowShown
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref().is_some_and(|a| a == "--tui") {
        #[cfg(feature = "tui")]
        return fin::tui_cli::run(std::iter::once("fin-tui".into()).chain(args));
        #[cfg(not(feature = "tui"))]
        return Err("Terminal support is disabled. Rebuild with --features tui.".into());
    }
    if first
        .as_deref()
        .is_some_and(|a| a == "--version" || a == "-V")
    {
        println!("fin {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if first.as_deref().is_some_and(|a| a == "--help" || a == "-h") {
        println!(
            "Fin — native Mermaid workspace\n\nUsage: fin [diagram.mmd]\n       fin --tui [diagram.mmd]\n       fin --tui --help\n       fin --snapshot image.png\n       fin --version\n\nCtrl/Cmd+S saves, +O opens, +N starts a diagram, +Z undoes, +Shift+Z redoes.\nCtrl/Cmd+0 fits the canvas; +\\ toggles the source panel."
        );
        return Ok(());
    }
    let snapshot = first
        .as_deref()
        .filter(|a| *a == "--snapshot")
        .map(|_| {
            args.next()
                .map(PathBuf::from)
                .ok_or("--snapshot requires a PNG path")
        })
        .transpose()?;
    let snapshot_node = if snapshot.is_some() {
        args.next().map(|s| s.to_string_lossy().into_owned())
    } else {
        None
    };
    let mut document = Document::new(EXAMPLE.into());
    if snapshot.is_none()
        && let Some(path) = first
    {
        document.load(Path::new(&path))?;
    }
    let ui = AppWindow::new()?;
    let controller = Rc::new(RefCell::new(Controller::new(document)));
    controller.borrow().sync_document(&ui);
    wire(&ui, &controller);
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let snapshot_requested = Rc::new(std::cell::Cell::new(false));
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(40),
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            controller.borrow_mut().tick(&ui);
            if let Some(path) = snapshot.as_ref()
                && controller.borrow().rendered.is_some()
                && !snapshot_requested.replace(true)
            {
                if let Some(node) = snapshot_node.as_ref() {
                    ui.invoke_select_node(node.as_str().into());
                    ui.invoke_fit_diagram();
                    if let Some(edge) = controller.borrow().flow.as_ref().and_then(|f| {
                        f.edges
                            .iter()
                            .enumerate()
                            .find(|(_, e)| &e.from == node || &e.to == node)
                    }) {
                        ui.set_selected_edge(edge.0 as i32);
                        ui.set_edge_label(edge.1.label.as_str().into());
                        ui.set_edge_kind(edge.1.kind.index());
                    }
                }
                let weak = ui.as_weak();
                let path = path.clone();
                slint::Timer::single_shot(Duration::from_millis(500), move || {
                    if let Some(ui) = weak.upgrade() {
                        let result = ui
                            .window()
                            .take_snapshot()
                            .map_err(|e| e.to_string())
                            .and_then(|buffer| {
                                resvg::tiny_skia::Pixmap::from_vec(
                                    buffer.as_bytes().to_vec(),
                                    resvg::tiny_skia::IntSize::from_wh(
                                        buffer.width(),
                                        buffer.height(),
                                    )
                                    .ok_or("Invalid snapshot size")?,
                                )
                                .ok_or("Invalid snapshot buffer".to_string())?
                                .save_png(path)
                                .map_err(|e| e.to_string())
                            });
                        if let Err(error) = result {
                            eprintln!("Snapshot failed: {error}");
                        }
                        let _ = slint::quit_event_loop();
                    }
                });
            }
        },
    );
    ui.run()?;
    Ok(())
}

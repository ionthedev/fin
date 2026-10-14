//! Exercise the real Slint callbacks and their source/preview synchronization.

use super::*;
use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementRoot};
use slint::Model;
use slint::platform::{Key, PointerEventButton, WindowEvent};

type TestController = Rc<RefCell<Controller>>;

fn setup(source: &str) -> (AppWindow, TestController, Renderer) {
    i_slint_backend_testing::init_no_event_loop();
    let ui = AppWindow::new().expect("headless app window");
    let controller = Rc::new(RefCell::new(Controller::new(Document::new(
        source.to_owned(),
    ))));
    wire(&ui, &controller);
    controller.borrow().sync_document(&ui);
    let mut renderer = Renderer::new();
    finish_render(&ui, &controller, &mut renderer);
    (ui, controller, renderer)
}

/// Run the same acceptance path as a worker reply without a timer or a sleep.
fn finish_render(ui: &AppWindow, controller: &TestController, renderer: &mut Renderer) {
    let source = controller.borrow().document.source.clone();
    controller
        .borrow_mut()
        .accept_render(ui, renderer.render(&source));
}

/// Complete the same viewport reply path synchronously for deterministic tests.
fn finish_preview(ui: &AppWindow, controller: &TestController) {
    let state = controller.borrow();
    let viewport = Controller::viewport(ui);
    let rendered = state.rendered.as_ref().expect("last valid graph");
    state.accept_preview(
        ui,
        PreviewReply {
            generation: state.preview_generation,
            viewport,
            result: preview::rasterize(&rendered.tree, viewport),
        },
    );
}

fn flow_source(controller: &TestController) -> Flow {
    Flow::parse(&controller.borrow().document.source).expect("editable generated Mermaid")
}

fn image_pixels(ui: &AppWindow) -> Vec<u8> {
    ui.get_diagram()
        .to_rgba8_premultiplied()
        .expect("native diagram pixel buffer")
        .as_bytes()
        .to_vec()
}

fn show_for_input(ui: &AppWindow) {
    ui.window().set_size(slint::LogicalSize::new(1280.0, 820.0));
    ui.show()
        .expect("show a headless window for input dispatch");
}

/// One pointer press/release, with no accessibility-action shortcut or retries.
fn click_accessible(ui: &AppWindow, label: &str) {
    let element = ElementHandle::find_by_accessible_label(ui, label)
        .next()
        .unwrap_or_else(|| panic!("missing accessible control: {label}"));
    let size = element.size();
    assert!(
        size.width > 0.0 && size.height > 0.0,
        "empty control: {label}"
    );
    element.mock_single_click(PointerEventButton::Left);
}

fn source_editor(ui: &AppWindow) -> ElementHandle {
    let source = ui.get_source();
    ui.root_element()
        .query_descendants()
        .match_accessible_role(AccessibleRole::TextInput)
        .match_predicate(move |element| element.accessible_value().as_ref() == Some(&source))
        .find_first()
        .expect("source editor with current document text")
}

fn send_text_key(ui: &AppWindow, text: &str) {
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
}

fn send_control_shortcut(ui: &AppWindow, text: &str, shift: bool) {
    ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    if shift {
        ui.window().dispatch_event(WindowEvent::KeyPressed {
            text: Key::Shift.into(),
        });
    }
    send_text_key(ui, text);
    if shift {
        ui.window().dispatch_event(WindowEvent::KeyReleased {
            text: Key::Shift.into(),
        });
    }
    ui.window().dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
}

#[test]
fn first_pointer_click_activates_controls_and_canvas_node_targets() {
    let initial = "flowchart TD\nA[Start] --> B[Finish]\n";
    let (ui, controller, mut renderer) = setup(initial);
    show_for_input(&ui);

    assert!(ui.get_code_visible());
    click_accessible(&ui, "Collapse source editor");
    assert!(
        !ui.get_code_visible(),
        "the first click must collapse the editor"
    );
    assert_eq!(controller.borrow().document.source, initial);
    click_accessible(&ui, "Expand source editor");
    assert!(ui.get_code_visible());

    click_accessible(&ui, "Node A");
    assert_eq!(
        ui.get_selected_id(),
        "A",
        "canvas hit target selects on its first click"
    );
    assert_eq!(ui.get_edit_label(), "Start");
    click_accessible(&ui, "Node B");
    assert_eq!(ui.get_selected_id(), "B");
    assert_eq!(ui.get_edit_label(), "Finish");

    click_accessible(&ui, "+ Step");
    assert_eq!(flow_source(&controller).nodes.len(), 3);
    finish_render(&ui, &controller, &mut renderer);
    let added = ui.get_selected_id().to_string();
    assert!(!added.is_empty());
    ui.set_edit_label("Applied with one click".into());
    click_accessible(&ui, "Apply changes");
    assert_eq!(
        flow_source(&controller)
            .node(&added)
            .expect("added step")
            .label,
        "Applied with one click"
    );
    finish_render(&ui, &controller, &mut renderer);

    click_accessible(&ui, "Undo");
    assert_eq!(
        flow_source(&controller)
            .node(&added)
            .expect("undo retains step")
            .label,
        "New step"
    );
    finish_render(&ui, &controller, &mut renderer);
    click_accessible(&ui, "Redo");
    assert_eq!(
        flow_source(&controller)
            .node(&added)
            .expect("redo retains step")
            .label,
        "Applied with one click"
    );
}

#[test]
fn focused_source_uses_controller_history_for_uppercase_shifted_redo() {
    let initial = "flowchart TD\nA[Original]\n";
    let (ui, controller, mut renderer) = setup(initial);
    show_for_input(&ui);

    // Actual TextEdit input gives the widget its own edit history. A later undo
    // must still use the shared document history, including visual mutations.
    source_editor(&ui).mock_single_click(PointerEventButton::Left);
    send_control_shortcut(&ui, "a", false);
    let typed = "flowchart TD\nA[Typed in the editor]\n";
    send_text_key(&ui, typed);
    assert_eq!(controller.borrow().document.source, typed);
    assert_eq!(ui.get_source(), typed);
    finish_render(&ui, &controller, &mut renderer);

    click_accessible(&ui, "Node A");
    ui.set_edit_label("Changed visually".into());
    ui.set_edit_shape(Shape::Diamond.index());
    click_accessible(&ui, "Apply changes");
    finish_render(&ui, &controller, &mut renderer);
    let generated = controller.borrow().document.source.clone();
    assert_eq!(
        flow_source(&controller)
            .node("A")
            .expect("edited node")
            .label,
        "Changed visually"
    );

    source_editor(&ui).mock_single_click(PointerEventButton::Left);
    send_control_shortcut(&ui, "z", false);
    assert_eq!(
        controller.borrow().document.source,
        typed,
        "undo must reverse the last visual edit, not TextEdit's older local edit"
    );
    assert_eq!(ui.get_source(), typed);
    finish_render(&ui, &controller, &mut renderer);

    send_control_shortcut(&ui, "Z", true);
    assert_eq!(
        controller.borrow().document.source,
        generated,
        "uppercase Ctrl+Shift+Z must redo the visual edit"
    );
    assert_eq!(ui.get_source(), generated);
    finish_render(&ui, &controller, &mut renderer);
    assert!(!ui.get_can_redo());

    // Platforms may deliver a lowercase key text while the shift flag is set.
    send_control_shortcut(&ui, "z", false);
    assert_eq!(controller.borrow().document.source, typed);
    finish_render(&ui, &controller, &mut renderer);
    send_control_shortcut(&ui, "z", true);
    assert_eq!(controller.borrow().document.source, generated);
    assert_eq!(ui.get_source(), generated);
}

#[test]
fn source_selection_and_visual_edits_round_trip_through_undo_redo() {
    let initial = "flowchart TD\nA[Start] --> B[Finish]\n";
    let (ui, controller, mut renderer) = setup(initial);
    assert!(ui.get_editable());
    assert_eq!(ui.get_nodes().row_count(), 2);
    assert!(!ui.get_dirty());

    let typed = "flowchart LR\nA[Typed step] --> B[Finish]\n";
    ui.invoke_source_edited(typed.into());
    assert_eq!(controller.borrow().document.source, typed);
    assert_eq!(ui.get_source(), typed);
    assert!(ui.get_busy());
    assert!(!ui.get_editable());
    finish_render(&ui, &controller, &mut renderer);
    assert!(ui.get_editable());
    assert_eq!(ui.get_direction(), "LR");

    ui.invoke_select_node("A".into());
    assert_eq!(ui.get_selected_id(), "A");
    assert_eq!(ui.get_edit_label(), "Typed step");
    assert_eq!(ui.get_edges().row_count(), 1);
    let label = "Approve \"release\" & [ship]";
    ui.invoke_apply_node(label.into(), Shape::Diamond.index());
    assert!(!ui.get_editable());
    let generated = controller.borrow().document.source.clone();
    let flow = flow_source(&controller);
    let node = flow.node("A").expect("selected node survives edit");
    assert_eq!(node.label, label);
    assert_eq!(node.shape, Shape::Diamond);
    assert_eq!(flow.direction, "LR");
    assert_eq!(flow.edges.len(), 1);
    assert_eq!(ui.get_source(), generated);
    finish_render(&ui, &controller, &mut renderer);
    assert_eq!(ui.get_selected_id(), "A");
    assert_eq!(ui.get_edit_label(), label);
    assert_eq!(ui.get_edit_shape(), Shape::Diamond.index());
    assert!(ui.get_dirty());
    assert!(ui.get_can_undo());

    ui.invoke_command("undo".into());
    assert_eq!(ui.get_source(), typed);
    assert!(ui.get_can_redo());
    finish_render(&ui, &controller, &mut renderer);
    assert_eq!(ui.get_edit_label(), "Typed step");
    assert_eq!(ui.get_edit_shape(), Shape::Rectangle.index());

    ui.invoke_command("redo".into());
    assert_eq!(ui.get_source(), generated);
    finish_render(&ui, &controller, &mut renderer);
    assert_eq!(ui.get_edit_label(), label);
    assert_eq!(ui.get_edit_shape(), Shape::Diamond.index());
    assert!(!ui.get_can_redo());
}

#[test]
fn visual_callbacks_add_connect_edit_and_remove_elements() {
    let (ui, controller, mut renderer) = setup(EMPTY);
    assert!(ui.get_editable());
    assert_eq!(ui.get_nodes().row_count(), 0);

    ui.invoke_add_node(Shape::Rectangle.index());
    finish_render(&ui, &controller, &mut renderer);
    let first = ui.get_selected_id().to_string();
    assert!(!first.is_empty());
    ui.invoke_add_node(Shape::Diamond.index());
    finish_render(&ui, &controller, &mut renderer);
    let second = ui.get_selected_id().to_string();
    assert_ne!(first, second);
    assert_eq!(ui.get_nodes().row_count(), 2);

    ui.set_connecting(true);
    ui.set_connect_from("".into());
    ui.invoke_select_node(first.as_str().into());
    assert_eq!(ui.get_connect_from(), first);
    ui.invoke_select_node(second.as_str().into());
    assert!(!ui.get_connecting());
    assert!(ui.get_connect_from().is_empty());
    finish_render(&ui, &controller, &mut renderer);
    let flow = flow_source(&controller);
    assert_eq!(flow.edges.len(), 1);
    assert_eq!(flow.edges[0].from, first);
    assert_eq!(flow.edges[0].to, second);

    ui.invoke_select_node(first.as_str().into());
    assert_eq!(ui.get_edges().row_count(), 1);
    ui.invoke_apply_edge(0, "if approved".into(), EdgeKind::Dotted.index());
    finish_render(&ui, &controller, &mut renderer);
    let flow = flow_source(&controller);
    assert_eq!(flow.edges[0].label, "if approved");
    assert_eq!(flow.edges[0].kind, EdgeKind::Dotted);
    ui.invoke_delete_edge(0);
    finish_render(&ui, &controller, &mut renderer);
    assert!(flow_source(&controller).edges.is_empty());
    assert_eq!(ui.get_edges().row_count(), 0);

    // Reconnect and remove its endpoint: dangling edges must disappear as well.
    ui.set_connecting(true);
    ui.invoke_select_node(first.as_str().into());
    ui.invoke_select_node(second.as_str().into());
    finish_render(&ui, &controller, &mut renderer);
    ui.invoke_select_node(second.as_str().into());
    ui.invoke_delete_node();
    finish_render(&ui, &controller, &mut renderer);
    let flow = flow_source(&controller);
    assert_eq!(flow.nodes.len(), 1);
    assert!(flow.node(&first).is_some());
    assert!(flow.node(&second).is_none());
    assert!(flow.edges.is_empty());
    assert!(ui.get_selected_id().is_empty());
    assert_eq!(ui.get_nodes().row_count(), 1);

    ui.invoke_set_direction("BT".into());
    finish_render(&ui, &controller, &mut renderer);
    assert_eq!(flow_source(&controller).direction, "BT");
    assert_eq!(ui.get_direction(), "BT");
}

#[test]
fn malformed_source_preserves_preview_and_rejects_visual_mutations() {
    let (ui, controller, mut renderer) = setup("flowchart TD\nA[Start] --> B[Finish]");
    ui.invoke_select_node("A".into());
    let previous_pixels = image_pixels(&ui);
    let previous_size = ui.get_diagram().size();
    let previous_svg = controller
        .borrow()
        .rendered
        .as_ref()
        .expect("initial preview")
        .svg
        .clone();

    let malformed = "flowchart TD\nA[unfinished";
    ui.invoke_source_edited(malformed.into());
    finish_render(&ui, &controller, &mut renderer);
    assert!(!ui.get_editable());
    assert!(!ui.get_busy());
    assert!(!ui.get_error_message().is_empty());
    assert_eq!(ui.get_diagram().size(), previous_size);
    assert_eq!(image_pixels(&ui), previous_pixels);
    assert_eq!(
        controller
            .borrow()
            .rendered
            .as_ref()
            .expect("last valid preview")
            .svg,
        previous_svg
    );

    ui.invoke_add_node(Shape::Rectangle.index());
    ui.invoke_apply_node("Should not replace the source".into(), 0);
    ui.invoke_delete_node();
    ui.invoke_set_direction("LR".into());
    ui.invoke_apply_edge(0, "No".into(), 0);
    ui.invoke_delete_edge(0);
    assert_eq!(controller.borrow().document.source, malformed);
    assert_eq!(ui.get_source(), malformed);
    assert_eq!(image_pixels(&ui), previous_pixels);

    ui.invoke_source_edited("flowchart TD\nA[Repaired] --> B[Finish]".into());
    finish_render(&ui, &controller, &mut renderer);
    assert!(ui.get_editable());
    assert!(ui.get_error_message().is_empty());
    assert_eq!(
        flow_source(&controller)
            .node("A")
            .expect("repaired node")
            .label,
        "Repaired"
    );
}

#[test]
fn advanced_flowcharts_render_without_lossy_visual_edits() {
    let styled = "flowchart LR\nA[Styled] --> B[Finish]\nstyle A fill:#bbddff,stroke:#225588\n";
    let (ui, controller, _) = setup(styled);
    assert!(ui.get_error_message().is_empty());
    assert!(!ui.get_editable());
    assert!(controller.borrow().flow.is_none());
    assert!(controller.borrow().rendered.is_some());
    assert_eq!(ui.get_nodes().row_count(), 2);
    let pixels = image_pixels(&ui);

    ui.invoke_select_node("A".into());
    ui.invoke_add_node(Shape::Circle.index());
    ui.invoke_apply_node("Changed".into(), Shape::Diamond.index());
    ui.invoke_delete_node();
    ui.invoke_set_direction("TD".into());
    ui.invoke_apply_edge(0, "Changed".into(), EdgeKind::Thick.index());
    ui.invoke_delete_edge(0);
    assert_eq!(controller.borrow().document.source, styled);
    assert_eq!(ui.get_source(), styled);
    assert!(!ui.get_dirty());
    assert!(!ui.get_can_undo());
    assert_eq!(image_pixels(&ui), pixels);
}

#[test]
fn stale_worker_replies_cannot_replace_a_newer_preview_or_error_state() {
    let (ui, controller, mut renderer) = setup("flowchart TD\nA[Original]");
    let current_source = "flowchart LR\nC[Current] --> D[Result]";
    ui.invoke_source_edited(current_source.into());
    let revision = controller.borrow().revision;
    let (sender, replies) = mpsc::channel();
    {
        let mut state = controller.borrow_mut();
        state.replies = replies;
        // Exercise reply draining without submitting work to the real worker.
        state.submitted = Some(revision);
    }
    sender
        .send(RenderReply {
            revision,
            result: renderer.render(current_source),
        })
        .expect("current result");
    sender
        .send(RenderReply {
            revision: revision - 1,
            result: renderer.render("flowchart TD\nOLD[Stale result]"),
        })
        .expect("stale successful result");
    sender
        .send(RenderReply {
            revision: revision - 1,
            result: Err("An obsolete error".into()),
        })
        .expect("stale failed result");
    controller.borrow_mut().tick(&ui);
    assert!(ui.get_editable());
    assert!(!ui.get_busy());
    assert!(ui.get_error_message().is_empty());
    assert_eq!(ui.get_source(), current_source);
    assert_eq!(ui.get_nodes().row_count(), 2);
    assert_eq!(
        ui.get_nodes().row_data(0).expect("current hit target").id,
        "C"
    );
    assert!(
        controller
            .borrow()
            .rendered
            .as_ref()
            .expect("current preview")
            .nodes
            .iter()
            .all(|node| node.id != "OLD")
    );
}

/// Use the real software renderer at known sizes, independent of a desktop
/// compositor that may force newly launched windows into smaller tiles.
#[test]
fn software_renderer_draws_source_and_inspector_at_supported_sizes() {
    use i_slint_backend_testing::testing_backend::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .expect("software test backend");
    let ui = AppWindow::new().expect("window");
    let controller = Rc::new(RefCell::new(Controller::new(Document::new(EXAMPLE.into()))));
    wire(&ui, &controller);
    controller.borrow().sync_document(&ui);
    ui.window().set_size(slint::LogicalSize::new(1280.0, 820.0));
    ui.show().expect("show software window");
    let mut renderer = Renderer::new();
    finish_render(&ui, &controller, &mut renderer);
    for (name, width, height, selected) in [
        ("preview", 1280, 820, ""),
        ("inspector", 1280, 820, "review"),
        ("compact", 900, 600, "review"),
    ] {
        ui.window()
            .set_size(slint::LogicalSize::new(width as f32, height as f32));
        ui.invoke_select_node(selected.into());
        if !selected.is_empty() {
            ui.set_selected_edge(1);
            ui.set_edge_label("".into());
        }
        ui.invoke_fit_diagram();
        finish_preview(&ui, &controller);
        let pixels = ui.window().take_snapshot().expect("render actual UI");
        assert_eq!((pixels.width(), pixels.height()), (width, height));
        assert!(
            pixels
                .as_bytes()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[0] > 180 && p[1] > 180 && p[2] > 180)
        );
        if let Some(directory) = std::env::var_os("FIN_SNAPSHOT_DIR") {
            std::fs::create_dir_all(&directory).expect("snapshot directory");
            resvg::tiny_skia::Pixmap::from_vec(
                pixels.as_bytes().to_vec(),
                resvg::tiny_skia::IntSize::from_wh(width, height).expect("valid dimensions"),
            )
            .expect("valid RGBA buffer")
            .save_png(PathBuf::from(directory).join(format!("{name}.png")))
            .expect("write UI snapshot");
        }
    }
}

#[test]
fn zoom_renders_at_display_resolution_and_keeps_node_targets_aligned() {
    use i_slint_backend_testing::testing_backend::{TestingBackend, TestingBackendOptions};
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        renderer_name: Some("software".into()),
        ..Default::default()
    })))
    .expect("software test backend");
    let ui = AppWindow::new().expect("window");
    let source = "flowchart TD\nA{Approve build?} -->|yes| B[Ship]";
    let controller = Rc::new(RefCell::new(Controller::new(Document::new(source.into()))));
    wire(&ui, &controller);
    controller.borrow().sync_document(&ui);
    show_for_input(&ui);
    let mut renderer = Renderer::new();
    finish_render(&ui, &controller, &mut renderer);
    let initial_svg = controller
        .borrow()
        .rendered
        .as_ref()
        .expect("graph")
        .svg
        .clone();

    for (zoom, scale) in [(1.0, 1.0), (1.75, 1.25), (3.0, 2.0)] {
        ui.window().dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        });
        // TestingBackend::set_size assumes scale 1. Set its physical buffer,
        // then dispatch the logical resize as a desktop backend would do.
        ui.window().set_size(slint::PhysicalSize::new(
            (1280.0 * scale) as u32,
            (820.0 * scale) as u32,
        ));
        ui.window().dispatch_event(WindowEvent::Resized {
            size: slint::LogicalSize::new(1280.0, 820.0),
        });
        ui.set_zoom(zoom);
        finish_preview(&ui, &controller);
        assert!(ui.get_detail_ready());
        let image = ui.get_detail().size();
        assert!((image.width as f32 - ui.get_detail_width() * zoom * scale).abs() < 0.01);
        assert!((image.height as f32 - ui.get_detail_height() * zoom * scale).abs() < 0.01);
        assert_eq!(ui.get_detail_zoom(), zoom);
        assert_eq!(ui.get_source(), source);
        assert_eq!(
            controller.borrow().rendered.as_ref().expect("graph").svg,
            initial_svg
        );

        let crisp = ui.window().take_snapshot().expect("detailed zoom view");
        assert_eq!(crisp.width(), (1280.0 * scale) as u32);
        assert_eq!(crisp.height(), (820.0 * scale) as u32);
        ui.set_detail_ready(false);
        let stretched = ui
            .window()
            .take_snapshot()
            .expect("original bitmap zoom view");
        ui.set_detail_ready(true);
        assert!(
            crisp
                .as_bytes()
                .iter()
                .zip(stretched.as_bytes())
                .filter(|(a, b)| a != b)
                .count()
                > 100
        );
        if let Some(directory) = std::env::var_os("FIN_SNAPSHOT_DIR") {
            std::fs::create_dir_all(&directory).expect("snapshot directory");
            for (name, pixels) in [("crisp", crisp), ("stretched", stretched)] {
                resvg::tiny_skia::Pixmap::from_vec(
                    pixels.as_bytes().to_vec(),
                    resvg::tiny_skia::IntSize::from_wh(pixels.width(), pixels.height())
                        .expect("size"),
                )
                .expect("RGBA snapshot")
                .save_png(
                    PathBuf::from(&directory).join(format!("zoom-{zoom}-dpi-{scale}-{name}.png")),
                )
                .expect("save zoom comparison");
            }
        }
    }
    // The sharper image is an overlay; actual pointer hit targets remain in
    // diagram coordinates and must still select the same node at 300% / 2x DPI.
    click_accessible(&ui, "Node A");
    assert_eq!(ui.get_selected_id(), "A");
    assert_eq!(ui.get_edit_label(), "Approve build?");
    assert!(!ui.get_dirty());
}

#[test]
fn viewport_replies_reject_old_graphs_zoom_and_dpi_but_allow_invalid_source_zoom() {
    let (ui, controller, mut renderer) = setup("flowchart TD\nA[Original]");
    show_for_input(&ui);
    ui.set_zoom(2.0);
    finish_preview(&ui, &controller);
    let old_viewport = Controller::viewport(&ui);
    let old_generation = controller.borrow().preview_generation;
    let old_tree = Arc::clone(&controller.borrow().rendered.as_ref().expect("graph").tree);
    let stale_reply = |generation, viewport| PreviewReply {
        generation,
        viewport,
        result: preview::rasterize(&old_tree, viewport),
    };

    ui.set_zoom(3.0);
    finish_preview(&ui, &controller);
    let zoomed_pixels = ui.get_detail().to_rgba8_premultiplied().expect("detail");
    controller
        .borrow()
        .accept_preview(&ui, stale_reply(old_generation, old_viewport));
    assert_eq!(
        ui.get_detail()
            .to_rgba8_premultiplied()
            .expect("detail")
            .as_bytes(),
        zoomed_pixels.as_bytes()
    );
    let before_dpi = Controller::viewport(&ui);
    ui.window()
        .dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    finish_preview(&ui, &controller);
    let hidpi_size = ui.get_detail().size();
    controller
        .borrow()
        .accept_preview(&ui, stale_reply(old_generation, before_dpi));
    assert_eq!(ui.get_detail().size(), hidpi_size);

    ui.invoke_source_edited("flowchart TD\nA[New graph]".into());
    finish_render(&ui, &controller, &mut renderer);
    assert!(
        !ui.get_detail_ready(),
        "new graph must remove the old detail overlay"
    );
    finish_preview(&ui, &controller);
    let current_pixels = ui.get_detail().to_rgba8_premultiplied().expect("detail");
    controller
        .borrow()
        .accept_preview(&ui, stale_reply(old_generation, Controller::viewport(&ui)));
    assert_eq!(
        ui.get_detail()
            .to_rgba8_premultiplied()
            .expect("detail")
            .as_bytes(),
        current_pixels.as_bytes()
    );

    ui.invoke_source_edited("flowchart TD\nA[unfinished".into());
    finish_render(&ui, &controller, &mut renderer);
    let error = ui.get_error_message();
    assert!(!error.is_empty());
    ui.set_zoom(2.5);
    finish_preview(&ui, &controller);
    assert!(ui.get_detail_ready());
    assert_eq!(ui.get_detail_zoom(), 2.5);
    assert_eq!(ui.get_error_message(), error);
    assert!(!ui.get_editable());
}

#[test]
fn preview_worker_tracks_scroll_resize_and_zoom_controls_without_editing_source() {
    let source = "flowchart TD\nA[Start] --> B[Review] --> C[Build] --> D[Ship]";
    let (ui, controller, _) = setup(source);
    show_for_input(&ui);
    ui.set_zoom(2.4);
    click_accessible(&ui, "+");
    assert_eq!(ui.get_zoom(), 3.0);
    let wait_for_detail = || {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            controller.borrow_mut().refresh_preview(&ui);
            if ui.get_detail_ready() {
                break;
            }
            assert!(Instant::now() < deadline, "viewport worker did not reply");
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    wait_for_detail();
    let before_scroll = Controller::viewport(&ui);
    // Scroll over empty canvas, through the real Flickable input path.
    ui.window().dispatch_event(WindowEvent::PointerScrolled {
        position: slint::LogicalPosition::new(1180.0, 400.0),
        delta_x: 0.0,
        delta_y: -180.0,
    });
    let scrolled = Controller::viewport(&ui);
    assert!(scrolled.y > before_scroll.y);
    ui.set_detail_ready(false);
    wait_for_detail();
    assert!(
        ui.get_detail_y() > 0.0,
        "scrolled crop follows graph coordinates"
    );

    ui.window().set_size(slint::LogicalSize::new(1050.0, 650.0));
    assert_ne!(Controller::viewport(&ui), scrolled);
    ui.set_detail_ready(false);
    wait_for_detail();
    assert_eq!(
        controller.borrow().submitted_viewport,
        Some(Controller::viewport(&ui))
    );
    assert_eq!(ui.get_source(), source);
    assert!(!ui.get_dirty());
}

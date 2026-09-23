//! Real client buffers, compositor rendering and IPC. No running desktop is touched.
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::{Duration, Instant};

use niri_config::Config;
use niri_ipc::{DockEdge, Reply};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Element;
use smithay::output::Output;
use smithay::utils::{Logical, Rectangle, Scale, Transform};
use wayland_client::protocol::wl_surface::WlSurface;

use super::animations::set_time;
use super::client::ClientId;
use super::Fixture;
use crate::ipc::server::IpcServer;
use crate::layout::minimize_animation::AnimationTarget;
use crate::render_helpers::{render_to_vec, RenderCtx, RenderTarget};
use crate::utils::output_size;
use crate::window::mapped::MappedId;

fn setup(extra: &str) -> Fixture {
    setup_with_effect(extra, "scale")
}

fn setup_with_effect(extra: &str, effect: &str) -> Fixture {
    setup_with_timing(
        extra,
        effect,
        "window-minimize { duration-ms 1000; curve \"linear\"; }",
    )
}

fn setup_with_timing(extra: &str, effect: &str, timing: &str) -> Fixture {
    let config = Config::parse_mem(&format!(
        r#"
        layout {{ gaps 0; border {{ off; }}; focus-ring {{ off; }}; shadow {{ off; }}; }}
        hotkey-overlay {{ skip-at-startup; }}
        animations {{ window-open {{ off; }}; {timing}
 window-minimize-effect "{effect}"; }}
        {extra}
    "#
    ))
    .unwrap();
    let mut f = Fixture::with_config(config);
    f.niri_state().backend.headless().add_renderer().unwrap();
    f.add_output(1, (800, 600));
    f.add_output(2, (1200, 900));
    f
}

fn window(f: &mut Fixture, client: ClientId, color: [u32; 3]) -> (WlSurface, MappedId) {
    let w = f.client(client).create_window();
    let surface = w.surface.clone();
    w.set_title("same-title");
    w.xdg_toplevel.set_app_id("same-app".into());
    w.commit();
    f.double_roundtrip(client);
    let w = f.client(client).window(&surface);
    let buffer = w
        .spbm
        .create_u32_rgba_buffer(color[0], color[1], color[2], u32::MAX, &w.qh, ());
    w.surface.attach(Some(&buffer), 0, 0);
    w.set_size(240, 180);
    w.ack_last_and_commit();
    f.double_roundtrip(client);
    let id = f.niri().layout.focus().unwrap().id();
    (surface, id)
}

fn elements(f: &mut Fixture, output: &Output) -> Vec<Rectangle<f64, Logical>> {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let mut result = Vec::new();
    f.niri()
        .layout
        .render_minimize_animations(output, RenderTarget::Output, &mut |element| {
            result.push(element.geometry(scale).to_f64().to_logical(scale));
        });
    result
}

fn pixels(f: &mut Fixture, output: &Output, target: RenderTarget) -> Vec<u8> {
    let state = f.niri_state();
    state.niri.update_render_elements(Some(output));
    state
        .backend
        .with_primary_renderer(|renderer| {
            let elements = state.niri.render_to_vec(
                RenderCtx {
                    renderer,
                    target,
                    xray: None,
                },
                output,
                false,
            );
            let scale = Scale::from(output.current_scale().fractional_scale());
            render_to_vec(
                renderer,
                output_size(output).to_physical_precise_round(scale),
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                elements.into_iter().rev(),
            )
            .unwrap()
        })
        .unwrap()
}

fn red_pixels(f: &mut Fixture, output: &Output, target: RenderTarget) -> usize {
    pixels(f, output, target)
        .chunks_exact(4)
        .filter(|p| is_red(p))
        .count()
}

fn is_red(p: &[u8]) -> bool {
    p[0] > 180 && p[1] < 80 && p[2] < 80
}

fn stamp(f: &mut Fixture, client: ClientId, parent: &WlSurface, x: i32, y: i32, color: [u32; 3]) {
    let c = f.client(client);
    let surface = c
        .state
        .compositor
        .as_ref()
        .unwrap()
        .create_surface(&c.qh, ());
    let sub = c
        .state
        .subcompositor
        .as_ref()
        .unwrap()
        .get_subsurface(&surface, parent, &c.qh, ());
    sub.set_position(x, y);
    let viewport = c
        .state
        .viewporter
        .as_ref()
        .unwrap()
        .get_viewport(&surface, &c.qh, ());
    viewport.set_destination(20, 20);
    let buffer = c.state.spbm.as_ref().unwrap().create_u32_rgba_buffer(
        color[0],
        color[1],
        color[2],
        u32::MAX,
        &c.qh,
        (),
    );
    surface.attach(Some(&buffer), 0, 0);
    surface.commit();
    c.window(parent).commit();
    f.double_roundtrip(client);
}

// Optional full-compositor frames for visual inspection, never from the running desktop.
fn save_frame(name: &str, pixels: &[u8]) {
    if let Some(directory) = std::env::var_os("NIRI_TEST_ANIMATION_FRAMES") {
        let file =
            std::fs::File::create(std::path::Path::new(&directory).join(format!("{name}.png")))
                .unwrap();
        let mut encoder = png::Encoder::new(file, 800, 600);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(pixels)
            .unwrap();
    }
}

#[test]
fn egl_scale_endpoints_all_edges_and_live_handoff() {
    for edge in [
        DockEdge::Left,
        DockEdge::Right,
        DockEdge::Top,
        DockEdge::Bottom,
    ] {
        let mut f = setup("");
        let client = f.add_client();
        let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        let output = f.niri_output(1);
        let win = f.niri().find_window_by_id(id).unwrap();
        let target = match edge {
            DockEdge::Left => Rectangle::new((8., 240.).into(), (40., 40.).into()),
            DockEdge::Right => Rectangle::new((752., 240.).into(), (40., 40.).into()),
            DockEdge::Top => Rectangle::new((380., 8.).into(), (40., 40.).into()),
            DockEdge::Bottom => Rectangle::new((380., 552.).into(), (40., 40.).into()),
        };
        let owner = Rc::new(());
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win.clone(),
                output: output.clone(),
                rect: target,
                edge,
                layer: None,
            }],
        );
        assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
        assert!(f.niri_state().minimize_window(Some(id)));
        assert!(f.niri().layout.is_minimized(&win));
        let source = elements(&mut f, &output)[0];
        assert!(source.size.w >= 200. && source.size.h >= 150.);
        // Publishing a new endpoint while running cannot redirect this transition.
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win.clone(),
                output: output.clone(),
                rect: Rectangle::from_size((1., 1.).into()),
                edge,
                layer: None,
            }],
        );
        set_time(f.niri(), Duration::from_millis(500));
        f.niri().advance_animations();
        let middle = elements(&mut f, &output)[0];
        assert!((middle.loc.x - (source.loc.x + target.loc.x) / 2.).abs() <= 1.);
        assert!((middle.loc.y - (source.loc.y + target.loc.y) / 2.).abs() <= 1.);
        assert!((middle.size.w - (source.size.w + target.size.w) / 2.).abs() <= 1.);
        let red = red_pixels(&mut f, &output, RenderTarget::Output);
        assert!(
            red > 1000,
            "{edge:?}: source={source:?}, middle={middle:?}, red={red}"
        );
        set_time(f.niri(), Duration::from_millis(1001));
        f.niri().advance_animations();
        assert!(elements(&mut f, &output).is_empty());
        assert_eq!(red_pixels(&mut f, &output, RenderTarget::Output), 0);
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win.clone(),
                output: output.clone(),
                rect: target,
                edge,
                layer: None,
            }],
        );
        assert!(f.niri_state().restore_window(Some(id), Some(&output), true));
        // Exactly one snapshot; the real tile is hidden, including when its client stalls.
        assert_eq!(elements(&mut f, &output).len(), 1);
        assert_eq!(red_pixels(&mut f, &output, RenderTarget::Output), 0);
        set_time(f.niri(), Duration::from_millis(1501));
        f.niri().advance_animations();
        assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 1000);
        set_time(f.niri(), Duration::from_millis(2102));
        f.niri().advance_animations();
        assert!(elements(&mut f, &output).is_empty());
        assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
        assert_eq!(f.niri().find_window_by_id(id).unwrap(), win);
    }
}

#[test]
fn egl_cross_output_fractional_scale_restore_uses_new_layout() {
    for effect in ["scale", "genie"] {
        let mut f = setup_with_effect(
            r#"output "headless-2" { scale 1.5; transform "90"; }"#,
            effect,
        );
        let client = f.add_client();
        let (_, first) = window(&mut f, client, [u32::MAX, 0, 0]);
        let (_, second) = window(&mut f, client, [0, 0, u32::MAX]);
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        let a = f.niri_output(1);
        let b = f.niri_output(2);
        let win = f.niri().find_window_by_id(first).unwrap();
        let owner = Rc::new(());
        let target = Rectangle::new((8., 120.).into(), (40., 40.).into());
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win.clone(),
                output: b.clone(),
                rect: target,
                edge: DockEdge::Left,
                layer: None,
            }],
        );
        assert!(f.niri_state().minimize_window(Some(first)));
        assert_eq!(f.niri().layout.windows().count(), 1);
        assert!(f.niri().find_window_by_id(second).is_some());
        assert!(f.niri_state().restore_window(Some(first), Some(&b), true));
        assert!(elements(&mut f, &a).is_empty());
        assert_eq!(elements(&mut f, &b).len(), 1);
        if effect == "scale" {
            assert_eq!(elements(&mut f, &b), vec![target]);
        }
        assert_eq!(red_pixels(&mut f, &b, RenderTarget::Output), 0);
        assert!(f.niri_state().minimize_window(Some(first)));
        assert!(f.niri_state().restore_window(Some(first), Some(&b), true));
        assert_eq!(elements(&mut f, &b).len(), 1);
        f.niri_complete_animations();
        assert!(elements(&mut f, &b).is_empty());
        assert!(red_pixels(&mut f, &b, RenderTarget::Output) > 10000);
        let (_, output) = f
            .niri()
            .layout
            .find_window_and_output(win.toplevel().unwrap().wl_surface())
            .unwrap();
        assert_eq!(output, Some(&b));
        f.niri_state().minimize_window(Some(first));
        f.niri().remove_output(&b);
        assert!(elements(&mut f, &b).is_empty());
        assert!(f.niri_state().restore_window(Some(first), Some(&a), true));
        f.niri_complete_animations();
        assert!(red_pixels(&mut f, &a, RenderTarget::Output) > 10000);
    }
}

#[test]
fn egl_capture_privacy_and_disabled_animation_fallback() {
    for effect in ["scale", "genie"] {
        let mut f = setup_with_effect(
            r#"window-rule { block-out-from "screen-capture"; }"#,
            effect,
        );
        let client = f.add_client();
        let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        let output = f.niri_output(1);
        f.niri_state().minimize_window(Some(id));
        set_time(f.niri(), Duration::from_millis(500));
        f.niri().advance_animations();
        assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 1000);
        assert_eq!(red_pixels(&mut f, &output, RenderTarget::ScreenCapture), 0);
        f.niri().clock.set_complete_instantly(true);
        f.niri().advance_animations();
        assert!(f.niri_state().restore_window(Some(id), Some(&output), true));
        assert!(elements(&mut f, &output).is_empty());
        assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
    }
}

#[test]
fn egl_ipc_target_owner_disconnect_clears_hint() {
    let mut f = setup("");
    let client = f.add_client();
    let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
    f.niri_complete_animations();
    set_time(f.niri(), Duration::ZERO);
    let name = format!("scale-test-{}-{}", std::process::id(), id.get());
    let server = IpcServer::start(&f.niri().event_loop, Some(OsStr::new(&name))).unwrap();
    let path = server.socket_path.clone().unwrap();
    f.niri().ipc_server = Some(server);
    f.niri_state().ipc_keyboard_layouts_changed();
    let mut stream = UnixStream::connect(path).unwrap();
    stream.set_nonblocking(true).unwrap();
    let request = serde_json::json!({"SetWindowAnimationTargets":{"targets":[{"id":id.get(),"output":"headless-1","rect":[8,240,40,40],"edge":"left"}]}});
    writeln!(stream, "{request}").unwrap();
    let mut reader = BufReader::new(stream);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut line = String::new();
    loop {
        f.state.server.dispatch();
        if reader.read_line(&mut line).is_ok() && line.ends_with('\n') {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    let reply: Reply = serde_json::from_str(&line).unwrap();
    assert!(reply.is_ok());
    let output = f.niri_output(1);
    assert!(f.niri_state().minimize_window(Some(id)));
    set_time(f.niri(), Duration::from_millis(500));
    f.niri().advance_animations();
    let midpoint = elements(&mut f, &output)[0];
    assert!((midpoint.loc.x - 4.).abs() < 1.);
    assert!((midpoint.loc.y - 120.).abs() < 1.);
    f.niri_complete_animations();
    assert!(f.niri_state().restore_window(Some(id), Some(&output), true));
    f.niri_complete_animations();
    set_time(f.niri(), Duration::ZERO);
    drop(reader);
    // Socket readiness and the cleanup idle callback can take more than one
    // dispatch under a parallel test load. Observe the public endpoint result.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        f.state.server.dispatch();
        assert!(f.niri_state().minimize_window(Some(id)));
        set_time(f.niri(), Duration::from_millis(500));
        f.niri().advance_animations();
        let midpoint = elements(&mut f, &output)[0];
        if midpoint.loc.y > 280. {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "publisher hint survived disconnect: {midpoint:?}"
        );
        f.niri_complete_animations();
        assert!(f.niri_state().restore_window(Some(id), Some(&output), true));
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn tile_rect(f: &mut Fixture, id: MappedId) -> Rectangle<f64, Logical> {
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .tiles_with_render_positions()
        .find(|(tile, _, _)| tile.window().id() == id)
        .map(|(tile, pos, _)| Rectangle::new(pos, tile.animated_tile_size()))
        .unwrap()
}

#[test]
fn egl_tiled_gap_closes_and_restore_inserts_at_new_position() {
    let mut f = setup("");
    let client = f.add_client();
    let (_, first) = window(&mut f, client, [u32::MAX, 0, 0]);
    let (_, second) = window(&mut f, client, [0, 0, u32::MAX]);
    f.niri_complete_animations();
    let first_before = tile_rect(&mut f, first);
    let second_before = tile_rect(&mut f, second);
    assert!(first_before.loc.x < second_before.loc.x);
    f.niri_state().minimize_window(Some(first));
    f.niri_complete_animations();
    // Niri may keep the focused column stationary by adjusting the view offset.
    // Its logical position must still move into the removed column, with no placeholder.
    let position = f
        .niri()
        .layout
        .active_workspace()
        .unwrap()
        .tiles_with_ipc_layouts()
        .find(|(tile, _)| tile.window().id() == second)
        .unwrap()
        .1
        .pos_in_scrolling_layout;
    assert_eq!(position, Some((1, 1)));
    assert!(f.niri_state().restore_window(Some(first), None, true));
    f.niri_complete_animations();
    let restored = tile_rect(&mut f, first);
    assert!(restored.loc.x > tile_rect(&mut f, second).loc.x);
    assert_ne!(restored.loc, first_before.loc);
    let output = f.niri_output(1);
    assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
}

#[test]
fn egl_floating_restore_and_output_change_reveal_live_window() {
    let mut f = setup("window-rule { open-floating true; }");
    let client = f.add_client();
    let (surface, id) = window(&mut f, client, [u32::MAX, 0, 0]);
    f.niri_complete_animations();
    let before = tile_rect(&mut f, id);
    let output = f.niri_output(1);
    f.niri_state().minimize_window(Some(id));
    f.niri_complete_animations();
    assert!(f.niri_state().restore_window(Some(id), None, true));
    assert_eq!(elements(&mut f, &output).len(), 1);
    output.change_current_state(None, Some(Transform::_180), None, None);
    f.niri().advance_animations();
    assert!(elements(&mut f, &output).is_empty());
    assert_eq!(tile_rect(&mut f, id), before);
    assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
    f.niri_state().minimize_window(Some(id));
    assert_eq!(elements(&mut f, &output).len(), 1);
    // Destroy the actual client toplevel during a transition.
    f.client(client).window(&surface).xdg_toplevel.destroy();
    f.double_roundtrip(client);
    assert!(elements(&mut f, &output).is_empty());
    assert!(f.niri().find_window_by_id(id).is_none());
}

#[test]
fn target_validation_and_surface_lifetime() {
    use smithay::desktop::layer_map_for_output;
    use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
    use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;

    use super::client::{LayerConfigureProps, LayerMargin};
    use crate::layout::minimize_animation::target_rect;

    let mut f = setup("");
    let client = f.add_client();
    let layer = f.client(client).create_layer(None, Layer::Top, "test-dock");
    let surface = layer.surface.clone();
    layer.set_configure_props(LayerConfigureProps {
        anchor: Some(Anchor::Left | Anchor::Bottom),
        size: Some((200, 60)),
        margin: Some(LayerMargin {
            left: 40,
            bottom: 20,
            ..Default::default()
        }),
        ..Default::default()
    });
    layer.commit();
    f.roundtrip(client);
    let layer = f.client(client).layer(&surface);
    layer.attach_new_buffer();
    layer.set_size(200, 60);
    layer.ack_last_and_commit();
    f.double_roundtrip(client);
    let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
    f.niri_complete_animations();
    set_time(f.niri(), Duration::ZERO);
    let output = f.niri_output(1);
    for rect in [
        [f64::NAN, 0., 1., 1.],
        [0., 0., 0., 1.],
        [0., 0., -1., 1.],
        [1e20, 0., 1., 1.],
    ] {
        assert!(target_rect(rect, &output, DockEdge::Left).is_none());
    }
    assert_eq!(
        target_rect([-50., 200., 40., 40.], &output, DockEdge::Left)
            .unwrap()
            .loc
            .x,
        0.
    );
    let layer = layer_map_for_output(&output)
        .layers()
        .find(|l| l.namespace() == "test-dock")
        .unwrap()
        .clone();
    let win = f.niri().find_window_by_id(id).unwrap();
    let owner = Rc::new(());
    f.niri().layout.set_window_animation_targets(
        &owner,
        vec![AnimationTarget {
            id: win,
            output: output.clone(),
            rect: Rectangle::new((8., 12.).into(), (40., 40.).into()),
            edge: DockEdge::Bottom,
            layer: Some(layer),
        }],
    );
    f.niri_state().minimize_window(Some(id));
    set_time(f.niri(), Duration::from_millis(500));
    f.niri().advance_animations();
    let midpoint = elements(&mut f, &output)[0];
    assert_eq!(midpoint.loc, (24., 266.).into());
    f.niri_complete_animations();
    // Unmapping the panel invalidates even a still-connected publisher's old hint.
    f.client(client).layer(&surface).surface.attach(None, 0, 0);
    f.client(client).layer(&surface).commit();
    f.double_roundtrip(client);
    assert!(f.niri_state().restore_window(Some(id), None, true));
    let fallback = elements(&mut f, &output)[0];
    assert!(
        fallback.loc.x > 350. && fallback.loc.y > 550.,
        "{fallback:?}"
    );
}

#[test]
fn egl_lock_cancels_snapshot_without_revealing_window() {
    for effect in ["scale", "genie"] {
        use crate::niri::LockState;
        let mut f = setup_with_effect("", effect);
        let client = f.add_client();
        let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
        f.niri_complete_animations();
        let output = f.niri_output(1);
        f.niri_state().minimize_window(Some(id));
        assert_eq!(elements(&mut f, &output).len(), 1);
        let c = f.client(client);
        let _lock = c
            .state
            .session_lock_manager
            .as_ref()
            .unwrap()
            .lock(&c.qh, ());
        f.double_roundtrip(client);
        if let LockState::WaitingForSurfaces {
            confirmation,
            deadline_token,
        } = std::mem::take(&mut f.niri().lock_state)
        {
            f.niri().event_loop.remove(deadline_token);
            f.niri().lock_state = LockState::Locking(confirmation);
        } else {
            panic!("expected a lock request");
        }
        f.niri().advance_animations();
        assert!(elements(&mut f, &output).is_empty());
        assert_eq!(red_pixels(&mut f, &output, RenderTarget::Output), 0);
        assert!(!f.niri_state().restore_window(Some(id), None, true));
    }
}

#[test]
fn egl_genie_deforms_all_edges_and_restores_in_reverse() {
    use crate::render_helpers::shaders::{ProgramType, Shaders};
    for edge in [
        DockEdge::Left,
        DockEdge::Right,
        DockEdge::Top,
        DockEdge::Bottom,
    ] {
        let mut f = setup_with_effect("window-rule { open-floating true; }", "genie");
        assert!(f
            .niri_state()
            .backend
            .with_primary_renderer(|r| Shaders::get(r).program(ProgramType::Genie).is_some())
            .unwrap());
        let client = f.add_client();
        let (surface, id) = window(&mut f, client, [u32::MAX, 0, 0]);
        stamp(&mut f, client, &surface, 25, 25, [u32::MAX; 3]);
        stamp(&mut f, client, &surface, 175, 130, [0, 0, u32::MAX]);
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        let output = f.niri_output(1);
        let win = f.niri().find_window_by_id(id).unwrap();
        let target = match edge {
            DockEdge::Left => Rectangle::new((8., 440.).into(), (40., 40.).into()),
            DockEdge::Right => Rectangle::new((752., 120.).into(), (40., 40.).into()),
            DockEdge::Top => Rectangle::new((120., 8.).into(), (40., 40.).into()),
            DockEdge::Bottom => Rectangle::new((500., 552.).into(), (40., 40.).into()),
        };
        let owner = Rc::new(());
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win,
                output: output.clone(),
                rect: target,
                edge,
                layer: None,
            }],
        );
        let before = pixels(&mut f, &output, RenderTarget::Output);
        f.niri_state().minimize_window(Some(id));
        let initial = pixels(&mut f, &output, RenderTarget::Output);
        assert!(before
            .chunks_exact(4)
            .map(is_red)
            .eq(initial.chunks_exact(4).map(is_red)));
        save_frame(&format!("{edge:?}-0"), &initial);
        let mut middle = Vec::new();
        for ms in [150, 300, 500, 700, 850] {
            set_time(f.niri(), Duration::from_millis(ms));
            f.niri().advance_animations();
            let frame = pixels(&mut f, &output, RenderTarget::Output);
            assert!(frame.chunks_exact(4).any(is_red), "{edge:?} at {ms}");
            save_frame(&format!("{edge:?}-{ms}"), &frame);
            if ms == 500 {
                middle = frame;
            }
        }
        // A rigid rectangle cannot pass this: near-Dock cross-sections must be narrower.
        let mut widths = vec![0; 800];
        for (i, p) in middle.chunks_exact(4).enumerate() {
            if !is_red(p) {
                continue;
            }
            let x = i % 800;
            let y = i / 800;
            let row = match edge {
                DockEdge::Bottom => y,
                DockEdge::Top => 599 - y,
                DockEdge::Right => x,
                DockEdge::Left => 799 - x,
            };
            widths[row] += 1;
        }
        let widths: Vec<_> = widths.into_iter().filter(|w| *w > 0).collect();
        assert!(widths.len() > 40, "{edge:?}");
        let n = widths.len() / 4;
        let far: usize = widths[..n].iter().sum();
        let near: usize = widths[widths.len() - n..].iter().sum();
        assert!(far > near * 2, "{edge:?}: far={far} near={near}");
        f.niri_complete_animations();
        assert!(f.niri_state().restore_window(Some(id), None, true));
        set_time(f.niri(), Duration::from_millis(1350));
        f.niri().advance_animations();
        let restored = pixels(&mut f, &output, RenderTarget::Output);
        assert!(
            middle
                .chunks_exact(4)
                .map(is_red)
                .eq(restored.chunks_exact(4).map(is_red)),
            "reverse {edge:?}"
        );
        f.niri_complete_animations();
        assert!(elements(&mut f, &output).is_empty());
        assert!(red_pixels(&mut f, &output, RenderTarget::Output) > 10000);
    }
}

/// Optional deterministic exports, sampled every 10 ms: play numbered frames at 100 fps.
/// Quarter-time stills are separate and must not be inserted into the playback sequence.
#[test]
#[ignore = "set NIRI_TEST_ANIMATION_FRAMES to export preset comparisons"]
fn egl_export_genie_preset_sequence() {
    assert!(std::env::var_os("NIRI_TEST_ANIMATION_FRAMES").is_some());
    for (name, timing, duration) in [
        (
            "A",
            "window-minimize { duration-ms 280; curve \"ease-out-cubic\"; }",
            280_u64,
        ),
        (
            "B",
            "window-minimize { duration-ms 550; curve \"linear\"; }",
            550,
        ),
        ("default", "", 550),
    ] {
        let mut f = setup_with_timing("window-rule { open-floating true; }", "genie", timing);
        let client = f.add_client();
        let (surface, id) = window(&mut f, client, [u32::MAX, 0, 0]);
        stamp(&mut f, client, &surface, 25, 25, [u32::MAX; 3]);
        stamp(&mut f, client, &surface, 175, 130, [0, 0, u32::MAX]);
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        let output = f.niri_output(1);
        let owner = Rc::new(());
        let win = f.niri().find_window_by_id(id).unwrap();
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win,
                output: output.clone(),
                rect: Rectangle::new((500., 552.).into(), (40., 40.).into()),
                edge: DockEdge::Bottom,
                layer: None,
            }],
        );
        for phase in ["minimize", "restore"] {
            let base = if phase == "minimize" {
                f.niri_state().minimize_window(Some(id));
                0
            } else {
                assert!(f.niri_state().restore_window(Some(id), None, true));
                (duration + 100) * 1000
            };
            let mut times: Vec<_> = (0..=duration + 100)
                .step_by(10)
                .map(|ms| ms * 1000)
                .collect();
            times.extend([duration * 250, duration * 500, duration * 750]);
            times.sort_unstable();
            times.dedup();
            for us in times {
                set_time(f.niri(), Duration::from_micros(base + us));
                f.niri().advance_animations();
                let frame = pixels(&mut f, &output, RenderTarget::Output);
                if us % 10000 == 0 {
                    save_frame(&format!("{name}-{phase}-{:04}", us / 10000), &frame);
                }
                for quarter in 1..=3 {
                    if us == duration * 250 * quarter {
                        save_frame(&format!("{name}-{phase}-quarter{quarter}"), &frame);
                    }
                }
            }
            assert!(elements(&mut f, &output).is_empty());
        }
    }
}

#[test]
fn egl_effect_presets_use_elapsed_time_and_hand_back_to_live_window() {
    for (effect, duration) in [("scale", 280_u64), ("genie", 550)] {
        for scale in [1., 1.25] {
            for edge in [
                DockEdge::Left,
                DockEdge::Right,
                DockEdge::Top,
                DockEdge::Bottom,
            ] {
                let mut f = setup_with_timing(&format!(
                    "window-rule {{ open-floating true; }}\noutput \"headless-1\" {{ scale {scale}; }}"
                ), effect, ""); // Deliberately no window-minimize block.
                let client = f.add_client();
                let (surface, id) = window(&mut f, client, [u32::MAX, 0, 0]);
                f.niri_complete_animations();
                set_time(f.niri(), Duration::ZERO);
                let output = f.niri_output(1);
                let size = output_size(&output);
                let loc = match edge {
                    DockEdge::Left => (8., size.h - 100.),
                    DockEdge::Right => (size.w - 48., 100.),
                    DockEdge::Top => (100., 8.),
                    DockEdge::Bottom => (size.w - 150., size.h - 48.),
                };
                let owner = Rc::new(());
                let win = f.niri().find_window_by_id(id).unwrap();
                f.niri().layout.set_window_animation_targets(
                    &owner,
                    vec![AnimationTarget {
                        id: win.clone(),
                        output: output.clone(),
                        rect: Rectangle::new(loc.into(), (40., 40.).into()),
                        edge,
                        layer: None,
                    }],
                );
                assert!(f.niri_state().minimize_window(Some(id)));
                let mut previous = pixels(&mut f, &output, RenderTarget::Output);
                for quarter in 1..=3 {
                    set_time(f.niri(), Duration::from_micros(duration * 250 * quarter));
                    f.niri().advance_animations();
                    assert_eq!(elements(&mut f, &output).len(), 1);
                    let frame = pixels(&mut f, &output, RenderTarget::Output);
                    assert_ne!(frame, previous, "{effect} {edge:?} at {quarter}/4 elapsed");
                    previous = frame;
                }
                set_time(f.niri(), Duration::from_millis(duration - 1));
                f.niri().advance_animations();
                assert_eq!(elements(&mut f, &output).len(), 1);
                set_time(f.niri(), Duration::from_millis(duration + 1));
                f.niri().advance_animations();
                assert!(elements(&mut f, &output).is_empty());
                assert_eq!(red_pixels(&mut f, &output, RenderTarget::Output), 0);
                assert!(f.niri_state().restore_window(Some(id), None, true));
                // Change the real client after the restore snapshot: it must stay hidden
                // until handoff, then the new buffer must replace the old snapshot.
                let w = f.client(client).window(&surface);
                let green = w
                    .spbm
                    .create_u32_rgba_buffer(0, u32::MAX, 0, u32::MAX, &w.qh, ());
                w.surface.attach(Some(&green), 0, 0);
                w.commit();
                f.double_roundtrip(client);
                let green_pixels = |frame: &[u8]| {
                    frame
                        .chunks_exact(4)
                        .filter(|p| p[1] > 180 && p[0] < 80 && p[2] < 80)
                        .count()
                };
                for quarter in 1..=3 {
                    set_time(
                        f.niri(),
                        Duration::from_micros((duration + 1) * 1000 + duration * 250 * quarter),
                    );
                    f.niri().advance_animations();
                    assert_eq!(elements(&mut f, &output).len(), 1);
                    assert_eq!(
                        green_pixels(&pixels(&mut f, &output, RenderTarget::Output)),
                        0
                    );
                }
                set_time(f.niri(), Duration::from_millis(2 * duration + 2));
                f.niri().advance_animations();
                assert!(elements(&mut f, &output).is_empty());
                assert!(green_pixels(&pixels(&mut f, &output, RenderTarget::Output)) > 10000);
                assert_eq!(f.niri().find_window_by_id(id).unwrap(), win);
                // Rapid reversal and output reconfiguration must reveal the live tile.
                assert!(f.niri_state().minimize_window(Some(id)));
                assert!(f.niri_state().restore_window(Some(id), None, true));
                assert_eq!(elements(&mut f, &output).len(), 1);
                output.change_current_state(None, Some(Transform::_180), None, None);
                f.niri().advance_animations();
                assert!(elements(&mut f, &output).is_empty());
                assert!(green_pixels(&pixels(&mut f, &output, RenderTarget::Output)) > 10000);
            }
        }
    }
}

#[test]
fn genie_preset_keeps_global_slowdown_and_off() {
    use crate::animation::{Animation, Clock};
    let config =
        Config::parse_mem("animations { window-minimize-effect \"genie\"; slowdown 2.0; }\n")
            .unwrap();
    let mut clock = Clock::with_time(Duration::ZERO);
    clock.set_rate(1. / config.animations.slowdown);
    let anim = Animation::new(
        clock.clone(),
        0.,
        1.,
        0.,
        config.animations.window_minimize().0,
    );
    for (ms, expected) in [(275, 0.25), (550, 0.5), (825, 0.75), (1100, 1.)] {
        clock.set_unadjusted(Duration::from_millis(ms));
        assert!((anim.clamped_value() - expected).abs() < 1e-6);
    }
    assert!(anim.is_done());
    clock.set_complete_instantly(true);
    let anim = Animation::new(
        clock.clone(),
        0.,
        1.,
        0.,
        config.animations.window_minimize().0,
    );
    assert!(anim.is_done());
}

#[test]
fn egl_genie_fallback_keeps_scale_opacity() {
    let capture = |effect| {
        let mut f = setup_with_timing(
            "window-rule { open-floating true; }",
            effect,
            "window-minimize { duration-ms 550; curve \"linear\"; }",
        );
        let client = f.add_client();
        let (_, id) = window(&mut f, client, [u32::MAX, 0, 0]);
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        let output = f.niri_output(1);
        let owner = Rc::new(());
        let win = f.niri().find_window_by_id(id).unwrap();
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win,
                output: output.clone(),
                // Behind the floating window for a bottom-edge hint: Genie falls back.
                rect: Rectangle::new((500., 8.).into(), (40., 40.).into()),
                edge: DockEdge::Bottom,
                layer: None,
            }],
        );
        let mut frames = Vec::new();
        for base in [0, 551] {
            if base == 0 {
                f.niri_state().minimize_window(Some(id));
            } else {
                assert!(f.niri_state().restore_window(Some(id), None, true));
            }
            for ms in [0, 10, 50, 275, 510, 549, 551] {
                set_time(f.niri(), Duration::from_millis(base + ms));
                f.niri().advance_animations();
                frames.push(pixels(&mut f, &output, RenderTarget::Output));
            }
        }
        frames
    };
    assert!(capture("genie") == capture("scale"));
}

#[test]
fn egl_genie_feeds_content_through_a_stationary_neck() {
    for edge in [
        DockEdge::Bottom,
        DockEdge::Top,
        DockEdge::Right,
        DockEdge::Left,
    ] {
        let mut f = setup_with_timing("window-rule { open-floating true; }", "genie", "");
        let client = f.add_client();
        let (surface, id) = window(&mut f, client, [u32::MAX, 0, 0]);
        let (far, near, target, mouth, section) = match edge {
            DockEdge::Bottom => ((110, 20), (110, 140), (500., 552.), 572., 532),
            DockEdge::Top => ((110, 140), (110, 20), (120., 8.), -28., -68),
            DockEdge::Right => ((20, 80), (200, 80), (752., 120.), 772., 732),
            DockEdge::Left => ((200, 80), (20, 80), (8., 440.), -28., -68),
        };
        stamp(&mut f, client, &surface, far.0, far.1, [u32::MAX; 3]);
        stamp(&mut f, client, &surface, near.0, near.1, [0, 0, u32::MAX]);
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        let output = f.niri_output(1);
        let owner = Rc::new(());
        let win = f.niri().find_window_by_id(id).unwrap();
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win,
                output: output.clone(),
                rect: Rectangle::new(target.into(), (40., 40.).into()),
                edge,
                layer: None,
            }],
        );
        f.niri_state().minimize_window(Some(id));
        let axis = |i: usize| match edge {
            DockEdge::Bottom => (i / 800) as i32,
            DockEdge::Top => -(i as i32 / 800),
            DockEdge::Right => (i % 800) as i32,
            DockEdge::Left => -(i as i32 % 800),
        };
        let mut neck = None;
        for ms in [330, 385, 440] {
            // 60%, 70%, 80% of the real default duration.
            set_time(f.niri(), Duration::from_millis(ms));
            f.niri().advance_animations();
            let frame = pixels(&mut f, &output, RenderTarget::Output);
            let colored = |p: &[u8]| p[0] > 180 || p[2] > 180;
            assert!(frame
                .chunks_exact(4)
                .enumerate()
                .all(|(i, p)| !colored(p) || f64::from(axis(i)) <= mouth + 1.));
            if ms < 440 {
                let slice: Vec<_> = frame
                    .chunks_exact(4)
                    .enumerate()
                    .filter(|(i, _)| axis(*i) == section)
                    .map(|(_, p)| colored(p))
                    .collect();
                assert!(slice.iter().any(|v| *v));
                let bounds = (
                    slice.iter().position(|v| *v),
                    slice.iter().rposition(|v| *v),
                );
                if let Some(previous) = neck {
                    assert_eq!(bounds, previous, "moving neck: {edge:?}");
                } else {
                    neck = Some(bounds);
                }
            } else {
                assert!(
                    frame
                        .chunks_exact(4)
                        .any(|p| p[0] > 220 && p[1] > 220 && p[2] > 220),
                    "trailing content lost: {edge:?}"
                );
                assert!(
                    !frame
                        .chunks_exact(4)
                        .any(|p| p[2] > 180 && p[0] < 80 && p[1] < 80),
                    "leading content not absorbed: {edge:?}"
                );
            }
        }
    }
}

/// Larger, patterned real client buffers make the funnel and content flow visible.
#[test]
#[ignore = "set NIRI_TEST_ANIMATION_FRAMES to export a large-window sequence"]
fn egl_export_genie_funnel_sequence() {
    assert!(std::env::var_os("NIRI_TEST_ANIMATION_FRAMES").is_some());
    let mut f = setup_with_timing("window-rule { open-floating true; }", "genie", "");
    let client = f.add_client();
    let (surface, id) = window(&mut f, client, [0x20202020, 0x70707070, 0xd0d0d0d0]);
    let w = f.client(client).window(&surface);
    w.set_size(560, 380);
    w.ack_last_and_commit();
    f.double_roundtrip(client);
    for y in (20..360).step_by(40) {
        for x in (20..540).step_by(40) {
            stamp(
                &mut f,
                client,
                &surface,
                x,
                y,
                if y < 60 {
                    [u32::MAX; 3]
                } else if x < 100 {
                    [0x99999999, 0xcccccccc, u32::MAX]
                } else {
                    [0xdddddddd, 0xeeeeeeee, u32::MAX]
                },
            );
        }
    }
    f.niri_complete_animations();
    set_time(f.niri(), Duration::ZERO);
    let output = f.niri_output(1);
    let owner = Rc::new(());
    let win = f.niri().find_window_by_id(id).unwrap();
    f.niri().layout.move_floating_window(
        Some(&win),
        niri_ipc::PositionChange::SetFixed(120.),
        niri_ipc::PositionChange::SetFixed(100.),
        false,
    );
    assert_eq!(
        tile_rect(&mut f, id),
        Rectangle::new((120., 100.).into(), (560., 380.).into())
    );
    f.niri().layout.set_window_animation_targets(
        &owner,
        vec![AnimationTarget {
            id: win,
            output: output.clone(),
            rect: Rectangle::new((380., 552.).into(), (40., 40.).into()),
            edge: DockEdge::Bottom,
            layer: None,
        }],
    );
    for phase in ["minimize", "restore"] {
        let base = if phase == "minimize" {
            f.niri_state().minimize_window(Some(id));
            0
        } else {
            assert!(f.niri_state().restore_window(Some(id), None, true));
            650
        };
        for ms in (0..=650).step_by(10) {
            set_time(f.niri(), Duration::from_millis(base + ms));
            f.niri().advance_animations();
            save_frame(
                &format!("large-{phase}-{:04}", ms / 10),
                &pixels(&mut f, &output, RenderTarget::Output),
            );
        }
        assert!(elements(&mut f, &output).is_empty());
    }
}

#[test]
fn egl_genie_first_frame_preserves_texture_padding_when_overlapping_dock() {
    for edge in [
        DockEdge::Bottom,
        DockEdge::Top,
        DockEdge::Right,
        DockEdge::Left,
    ] {
        let mut f = setup_with_timing("window-rule { open-floating true; }", "genie", "");
        let client = f.add_client();
        let (surface, id) = window(&mut f, client, [u32::MAX, 0, 0]);
        let w = f.client(client).window(&surface);
        w.set_size(760, 560);
        w.xdg_surface.set_window_geometry(0, 0, 760, 560);
        w.ack_last_and_commit();
        f.double_roundtrip(client);
        // Subsurfaces extend beyond the main geometry, just as snapshot padding can.
        stamp(&mut f, client, &surface, -12, -12, [u32::MAX; 3]);
        stamp(&mut f, client, &surface, 752, 552, [u32::MAX; 3]);
        let win = f.niri().find_window_by_id(id).unwrap();
        f.niri().layout.move_floating_window(
            Some(&win),
            niri_ipc::PositionChange::SetFixed(20.),
            niri_ipc::PositionChange::SetFixed(20.),
            false,
        );
        f.niri_complete_animations();
        set_time(f.niri(), Duration::ZERO);
        let output = f.niri_output(1);
        let target = match edge {
            DockEdge::Bottom => (380., 552.),
            DockEdge::Top => (380., 8.),
            DockEdge::Right => (752., 240.),
            DockEdge::Left => (8., 240.),
        };
        let owner = Rc::new(());
        f.niri().layout.set_window_animation_targets(
            &owner,
            vec![AnimationTarget {
                id: win,
                output: output.clone(),
                rect: Rectangle::new(target.into(), (40., 40.).into()),
                edge,
                layer: None,
            }],
        );
        let before = pixels(&mut f, &output, RenderTarget::Output);
        let white = |p: &[u8]| p[0] > 220 && p[1] > 220 && p[2] > 220;
        assert!(white(&before[(10 * 800 + 10) * 4..][..4]));
        assert!(white(&before[(590 * 800 + 790) * 4..][..4]));
        f.niri_state().minimize_window(Some(id));
        let first = pixels(&mut f, &output, RenderTarget::Output);
        save_frame(&format!("overlap-{edge:?}-before"), &before);
        save_frame(&format!("overlap-{edge:?}-first"), &first);
        assert!(
            before
                .chunks_exact(4)
                .map(|p| (is_red(p), white(p)))
                .eq(first.chunks_exact(4).map(|p| (is_red(p), white(p)))),
            "{edge:?}"
        );
    }
}

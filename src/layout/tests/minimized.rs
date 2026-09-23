use super::*;

fn add(id: usize, floating: bool) -> Op {
    let mut params = TestWindowParams::new(id);
    params.is_floating = floating;
    Op::AddWindow { params }
}

fn settle(layout: &mut Layout<TestWindow>) {
    layout.refresh(true);
    let windows: Vec<_> = layout
        .managed_windows()
        .map(|(_, window)| window.clone())
        .collect();
    for window in windows {
        if window.communicate() {
            layout.update_window(window.id(), None);
        }
    }
    layout.clock.set_complete_instantly(true);
    layout.advance_animations();
    layout.clock.set_complete_instantly(false);
    layout.verify_invariants();
}

fn floating_geometry(layout: &Layout<TestWindow>, id: usize) -> Rectangle<f64, Logical> {
    let mut geometry = None;
    layout.with_windows(|window, _, _, layout| {
        if *window.id() == id {
            geometry = Some(Rectangle::new(
                layout.tile_pos_in_workspace_view.unwrap().into(),
                layout.tile_size.into(),
            ));
        }
    });
    geometry.unwrap()
}

#[test]
fn minimizes_single_window_without_unmapping_or_recreating_it() {
    let mut layout = check_ops([Op::AddOutput(1), add(1, false)]);
    settle(&mut layout);
    let window = layout.focus().unwrap().clone();
    assert!(layout.minimize_window(&1));
    assert!(!layout.has_window(&1));
    assert!(layout.has_managed_window(&1));
    assert_eq!(layout.windows().count(), 0);
    assert!(layout.focus().is_none());
    layout.with_managed_windows(|window, output, workspace, geometry| {
        assert_eq!(*window.id(), 1);
        assert!(output.is_none());
        assert!(workspace.is_none());
        assert!(geometry.pos_in_scrolling_layout.is_none());
        assert!(geometry.tile_pos_in_workspace_view.is_none());
        assert!(geometry.tile_size.0.is_finite());
        assert!(geometry.tile_size.1.is_finite());
    });
    assert_eq!(
        layout.restore_window(None, None, ActivateWindow::Yes),
        Some(1)
    );
    assert!(Rc::ptr_eq(&window.0, &layout.focus().unwrap().0));
    assert_eq!(layout.managed_windows().count(), 1);
    settle(&mut layout);
}

#[test]
fn minimization_order_is_stable_and_close_removes_queue_entries() {
    let mut layout = check_ops([Op::AddOutput(1), add(1, false), add(2, false), add(3, true)]);
    assert!(layout.minimize_window(&1));
    assert!(layout.minimize_window(&2));
    assert!(!layout.minimize_window(&1));
    assert!(!layout.minimize_window(&99));
    assert_eq!(
        layout.restore_window(None, None, ActivateWindow::No),
        Some(2)
    );
    assert_eq!(
        layout.restore_window(Some(&2), None, ActivateWindow::Yes),
        None
    );
    assert!(layout.minimize_window(&3));
    assert!(layout.remove_window(&3, Transaction::new()).is_some());
    assert_eq!(
        layout.restore_window(Some(&3), None, ActivateWindow::Yes),
        None
    );
    assert_eq!(
        layout.restore_window(None, None, ActivateWindow::Yes),
        Some(1)
    );
    assert_eq!(layout.restore_window(None, None, ActivateWindow::Yes), None);
    assert_eq!(layout.managed_windows().count(), 2);
    settle(&mut layout);
}

#[test]
fn tiled_restore_uses_current_workspace_and_new_column() {
    let mut layout = check_ops([
        Op::AddOutput(1),
        add(1, false),
        add(2, false),
        add(3, false),
    ]);
    settle(&mut layout);
    let original_workspace = layout.active_workspace().unwrap().id();
    layout.activate_window(&2);
    assert!(layout.minimize_window(&1));
    assert_eq!(layout.focus().map(|window| *window.id()), Some(2));
    layout.switch_workspace_down();
    let target = layout.active_workspace().unwrap().id();
    assert_ne!(target, original_workspace);
    assert_eq!(
        layout.restore_window(Some(&1), None, ActivateWindow::Yes),
        Some(1)
    );
    assert!(layout.active_workspace().unwrap().has_window(&1));
    let mut ids = Vec::new();
    layout.with_windows(|window, _, workspace, _| {
        if *window.id() == 1 {
            ids.push(workspace);
        }
    });
    assert_eq!(ids, vec![Some(target)]);
    settle(&mut layout);
}

#[test]
fn multiwindow_and_tabbed_columns_do_not_keep_placeholders() {
    for tabbed in [false, true] {
        let mut layout = check_ops([
            Op::AddOutput(1),
            add(1, false),
            add(2, false),
            Op::FocusColumnLeft,
            Op::ConsumeWindowIntoColumn,
        ]);
        if tabbed {
            layout.set_column_display(ColumnDisplay::Tabbed);
        }
        let initial_columns = layout
            .active_workspace()
            .unwrap()
            .scrolling()
            .columns()
            .count();
        assert_eq!(initial_columns, 1);
        assert!(layout.minimize_window(&1));
        assert_eq!(
            layout
                .active_workspace()
                .unwrap()
                .scrolling()
                .columns()
                .count(),
            1
        );
        assert!(layout.minimize_window(&2));
        assert_eq!(
            layout
                .active_workspace()
                .unwrap()
                .scrolling()
                .columns()
                .count(),
            0
        );
        assert_eq!(
            layout.restore_window(None, None, ActivateWindow::Yes),
            Some(2)
        );
        assert_eq!(
            layout.restore_window(None, None, ActivateWindow::Yes),
            Some(1)
        );
        assert_eq!(
            layout
                .active_workspace()
                .unwrap()
                .scrolling()
                .columns()
                .count(),
            2
        );
        settle(&mut layout);
    }
}

#[test]
fn minimized_last_window_does_not_keep_an_old_workspace_alive() {
    let mut layout = check_ops([Op::AddOutput(1), add(1, false)]);
    let old = layout.active_workspace().unwrap().id();
    layout.switch_workspace_down();
    assert!(layout.minimize_window(&1));
    settle(&mut layout);
    assert!(layout.find_workspace_by_id(old).is_none());
    assert_eq!(
        layout.restore_window(None, None, ActivateWindow::Yes),
        Some(1)
    );
    assert_ne!(layout.active_workspace().unwrap().id(), old);
    settle(&mut layout);
}

#[test]
fn missing_output_and_no_outputs_preserve_minimized_windows() {
    let mut layout = check_ops([
        Op::AddOutput(1),
        Op::AddOutput(2),
        add(1, false),
        add(2, true),
    ]);
    let output = layout
        .outputs()
        .find(|output| output.name() == "output2")
        .unwrap()
        .clone();
    assert!(layout.minimize_window(&1));
    assert!(layout.minimize_window(&2));
    layout.remove_output(&output);
    assert_eq!(
        layout.restore_window(None, Some(&output), ActivateWindow::Yes),
        None
    );
    Op::RemoveOutput(1).apply(&mut layout);
    assert_eq!(layout.restore_window(None, None, ActivateWindow::Yes), None);
    assert_eq!(layout.managed_windows().count(), 2);
    assert!(layout.is_minimized(&1));
    assert!(layout.is_minimized(&2));
    layout.remove_window(&1, Transaction::new());
    assert!(!layout.has_managed_window(&1));
    Op::AddOutput(3).apply(&mut layout);
    assert_eq!(
        layout.restore_window(None, None, ActivateWindow::Yes),
        Some(2)
    );
    settle(&mut layout);
}

#[test]
fn floating_same_working_area_restores_size_and_position() {
    let mut layout = check_ops([
        Op::AddScaledOutput {
            id: 1,
            scale: 1.25,
            layout_config: None,
        },
        add(1, true),
    ]);
    settle(&mut layout);
    layout.move_floating_window(
        Some(&1),
        PositionChange::SetFixed(113.),
        PositionChange::SetFixed(79.),
        false,
    );
    settle(&mut layout);
    let before = floating_geometry(&layout, 1);
    assert!(layout.minimize_window(&1));
    layout.switch_workspace_down();
    assert_eq!(
        layout.restore_window(None, None, ActivateWindow::Yes),
        Some(1)
    );
    settle(&mut layout);
    let after = floating_geometry(&layout, 1);
    assert_eq!(after, before);
    assert!(layout.active_workspace().unwrap().is_floating(&1));
}

#[test]
fn floating_cross_output_restores_normalized_center_with_logical_size() {
    let mut layout = check_ops([
        Op::AddOutput(1),
        Op::AddScaledOutput {
            id: 2,
            scale: 1.25,
            layout_config: None,
        },
        add(1, true),
    ]);
    settle(&mut layout);
    let area = layout.active_workspace().unwrap().working_area();
    let before = floating_geometry(&layout, 1);
    let center = before.loc + before.size.to_point().downscale(2.);
    let normalized = (
        (center.x - area.loc.x) / area.size.w,
        (center.y - area.loc.y) / area.size.h,
    );
    assert!(layout.minimize_window(&1));
    let output = layout
        .outputs()
        .find(|output| output.name() == "output2")
        .unwrap()
        .clone();
    assert_eq!(
        layout.restore_window(None, Some(&output), ActivateWindow::Yes),
        Some(1)
    );
    settle(&mut layout);
    let area = layout.active_workspace().unwrap().working_area();
    let after = floating_geometry(&layout, 1);
    assert_eq!(after.size, before.size);
    let center = after.loc + after.size.to_point().downscale(2.);
    approx::assert_abs_diff_eq!(
        (center.x - area.loc.x) / area.size.w,
        normalized.0,
        epsilon = 0.001
    );
    approx::assert_abs_diff_eq!(
        (center.y - area.loc.y) / area.size.h,
        normalized.1,
        epsilon = 0.001
    );
}

#[test]
fn floating_smaller_target_honors_minimum_and_keeps_top_left_reachable() {
    let mut params = TestWindowParams::new(1);
    params.is_floating = true;
    params.bbox = Rectangle::from_size((1100, 650).into());
    params.min_max_size.0 = (900, 600).into();
    let mut layout = check_ops([
        Op::AddOutput(1),
        Op::AddScaledOutput {
            id: 2,
            scale: 2.,
            layout_config: None,
        },
        Op::AddWindow { params },
    ]);
    settle(&mut layout);
    assert!(layout.minimize_window(&1));
    let output = layout
        .outputs()
        .find(|output| output.name() == "output2")
        .unwrap()
        .clone();
    layout.restore_window(None, Some(&output), ActivateWindow::Yes);
    settle(&mut layout);
    let geometry = floating_geometry(&layout, 1);
    let area = layout.active_workspace().unwrap().working_area();
    assert!(geometry.size.w >= 900.);
    assert!(geometry.size.h >= 600.);
    approx::assert_abs_diff_eq!(geometry.loc.x, area.loc.x, epsilon = 1.);
    approx::assert_abs_diff_eq!(geometry.loc.y, area.loc.y, epsilon = 1.);
}

#[test]
fn fullscreen_maximized_floating_window_preserves_normal_state() {
    let mut layout = check_ops([Op::AddOutput(1), add(1, true)]);
    settle(&mut layout);
    let before = floating_geometry(&layout, 1);
    layout.set_maximized(&1, true);
    layout.set_fullscreen(&1, true);
    settle(&mut layout);
    assert!(layout.minimize_window(&1));
    assert_eq!(
        layout.restore_window(None, None, ActivateWindow::Yes),
        Some(1)
    );
    settle(&mut layout);
    assert!(layout
        .focus()
        .unwrap()
        .pending_sizing_mode()
        .is_fullscreen());
    layout.set_fullscreen(&1, false);
    assert!(layout.focus().unwrap().pending_sizing_mode().is_maximized());
    layout.set_maximized(&1, false);
    settle(&mut layout);
    assert!(layout.active_workspace().unwrap().is_floating(&1));
    assert_eq!(floating_geometry(&layout, 1), before);
}

#[test]
fn hidden_commits_and_sizing_requests_survive_restore() {
    let mut layout = check_ops([Op::AddOutput(1), add(1, false)]);
    assert!(layout.minimize_window(&1));
    layout.set_fullscreen(&1, true);
    settle(&mut layout);
    assert!(layout
        .managed_windows()
        .next()
        .unwrap()
        .1
        .pending_sizing_mode()
        .is_fullscreen());
    layout.set_maximized(&1, true);
    layout.set_fullscreen(&1, false);
    settle(&mut layout);
    assert_eq!(
        layout.restore_window(None, None, ActivateWindow::Yes),
        Some(1)
    );
    settle(&mut layout);
    assert!(layout.focus().unwrap().pending_sizing_mode().is_maximized());
}

fn floating_workspace(
    area: Rectangle<f64, Logical>,
    scale: f64,
    transform: Transform,
    clock: Clock,
) -> Workspace<TestWindow> {
    let mut workspace = Workspace::new_no_outputs(clock, Rc::new(Options::default()));
    workspace.set_view_size(
        smithay::output::Scale::Fractional(scale),
        transform,
        (1280., 720.).into(),
        area,
    );
    workspace
}

fn add_floating_tile(workspace: &mut Workspace<TestWindow>, tile: Tile<TestWindow>) {
    workspace.add_tile(
        tile,
        WorkspaceAddWindowTarget::Auto,
        ActivateWindow::Yes,
        ColumnWidth::Fixed(100.),
        false,
        true,
        None,
    );
    let window = workspace.windows().next().unwrap().clone();
    if window.communicate() {
        workspace.update_window(window.id(), None);
    }
    workspace.verify_invariants(None);
}

fn workspace_floating_geometry(workspace: &Workspace<TestWindow>) -> Rectangle<f64, Logical> {
    let (tile, pos) = workspace.floating().tiles_with_offsets().next().unwrap();
    Rectangle::new(pos, tile.tile_size())
}

#[test]
fn floating_restore_respects_working_area_offsets_and_output_transform() {
    let source_area = Rectangle::new((40., 32.).into(), (944., 512.).into());
    let mut source = floating_workspace(
        source_area,
        1.25,
        Transform::Normal,
        Clock::with_time(Duration::ZERO),
    );
    let tile = source.make_tile(TestWindow::new(TestWindowParams::new(1)));
    add_floating_tile(&mut source, tile);
    let before = workspace_floating_geometry(&source);
    let center = source.minimized_floating_center(&1);
    let mut removed = source.remove_tile(&1, Transaction::new());
    source.verify_invariants(None);

    let target_area = Rectangle::new((24., 48.).into(), (528., 928.).into());
    let mut target = floating_workspace(target_area, 1.25, Transform::_90, source.clock.clone());
    target.prepare_minimized_tile(&mut removed.tile, center);
    add_floating_tile(&mut target, removed.tile);
    let after = workspace_floating_geometry(&target);
    assert_eq!(before.size, after.size);
    let after_center = after.loc + after.size.to_point().downscale(2.);
    let center = center.unwrap();
    approx::assert_abs_diff_eq!(
        (after_center.x - target_area.loc.x) / target_area.size.w,
        center.x,
        epsilon = 0.002
    );
    approx::assert_abs_diff_eq!(
        (after_center.y - target_area.loc.y) / target_area.size.h,
        center.y,
        epsilon = 0.002
    );
}

#[test]
fn floating_restore_into_empty_working_area_has_finite_geometry() {
    let mut source = floating_workspace(
        Rectangle::from_size((1280., 720.).into()),
        1.,
        Transform::Normal,
        Clock::with_time(Duration::ZERO),
    );
    let tile = source.make_tile(TestWindow::new(TestWindowParams::new(1)));
    add_floating_tile(&mut source, tile);
    let center = source.minimized_floating_center(&1);
    let mut removed = source.remove_tile(&1, Transaction::new());
    let mut target = floating_workspace(
        Rectangle::new((100., 100.).into(), (0., 0.).into()),
        1.,
        Transform::Normal,
        source.clock.clone(),
    );
    target.prepare_minimized_tile(&mut removed.tile, center);
    add_floating_tile(&mut target, removed.tile);
    let geometry = workspace_floating_geometry(&target);
    assert!(geometry.loc.x.is_finite() && geometry.loc.y.is_finite());
    assert!(geometry.size.w.is_finite() && geometry.size.h.is_finite());
    assert!(geometry.size.w > 0. && geometry.size.h > 0.);
}

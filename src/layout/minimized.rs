use super::*;

/// An owned tile with no workspace or output affiliation.
#[derive(Debug)]
pub(super) struct MinimizedWindow<W: LayoutElement> {
    pub(super) removed: RemovedTile<W>,
    /// Normal floating window center within the source working area.
    floating_center: Option<Point<f64, SizeFrac>>,
    /// Maximization can remain set underneath fullscreen in the scrolling layout.
    maximized: bool,
}

impl<W: LayoutElement> MinimizedWindow<W> {
    pub(super) fn id(&self) -> &W::Id {
        self.removed.tile.window().id()
    }
}

impl<W: LayoutElement> Layout<W> {
    pub fn is_minimized(&self, id: &W::Id) -> bool {
        self.minimized_windows.iter().any(|entry| entry.id() == id)
    }

    /// Removes a live window from ordinary layout membership without destroying it.
    pub fn minimize_window(&mut self, id: &W::Id) -> bool {
        if self.is_minimized(id) {
            return false;
        }

        // Finish a move first so its final floating geometry and output membership are known.
        self.interactive_move_end(id);
        let Some((_, _, workspace)) = self.workspaces().find(|(_, _, ws)| ws.has_window(id)) else {
            return false;
        };
        let floating_center = workspace.minimized_floating_center(id);
        let maximized = workspace.is_pending_maximized(id);
        let tile = workspace
            .tiles()
            .find(|tile| tile.window().id() == id)
            .unwrap();
        let currently_floating = workspace.is_floating(id);
        let is_floating = currently_floating || tile.restore_to_floating;

        let Some(mut removed) = self.remove_window(id, Transaction::new()) else {
            return false;
        };
        removed.is_floating = is_floating;
        removed.tile.stop_move_animations();
        removed.tile.alpha_animation = None;
        removed.tile.interactive_move_offset = Point::default();
        let win = removed.tile.window_mut();
        win.cancel_interactive_resize();
        win.set_interactive_resize(None);
        win.set_activated(false);
        win.set_active_in_column(false);
        win.set_floating(currently_floating);
        win.set_minimized(true);
        win.send_pending_configure();
        self.minimized_windows.push(MinimizedWindow {
            removed,
            floating_center,
            maximized,
        });
        true
    }

    /// Restores into an output's current workspace using ordinary insertion rules.
    ///
    /// Resolve the destination before removing ownership from the queue: disconnected outputs
    /// and sessions without outputs must leave every minimized window intact.
    pub fn restore_window(
        &mut self,
        id: Option<&W::Id>,
        output: Option<&Output>,
        activate: ActivateWindow,
    ) -> Option<W::Id> {
        let MonitorSet::Normal {
            monitors,
            active_monitor_idx,
            ..
        } = &mut self.monitor_set
        else {
            return None;
        };
        let monitor_idx = match output {
            Some(output) => monitors
                .iter()
                .position(|monitor| monitor.output == *output)?,
            None => *active_monitor_idx,
        };
        let monitor = &mut monitors[monitor_idx];
        let workspace = monitor.workspaces.get_mut(monitor.active_workspace_idx)?;
        let idx = match id {
            Some(id) => self
                .minimized_windows
                .iter()
                .position(|entry| entry.id() == id)?,
            None => self.minimized_windows.len().checked_sub(1)?,
        };
        let mut entry = self.minimized_windows.remove(idx);
        let id = entry.id().clone();
        workspace.prepare_minimized_tile(&mut entry.removed.tile, entry.floating_center);
        entry.removed.tile.advance_animations();
        entry.removed.tile.window_mut().set_minimized(false);
        let RemovedTile {
            tile,
            width,
            is_full_width,
            is_floating,
        } = entry.removed;
        monitor.add_tile(
            tile,
            MonitorAddWindowTarget::Auto,
            activate,
            true,
            width,
            is_full_width,
            is_floating,
            None,
        );
        if entry.maximized {
            let workspace = monitor
                .workspaces
                .iter_mut()
                .find(|ws| ws.has_window(&id))
                .unwrap();
            workspace.set_maximized(&id, true);
        }
        if activate.map_smart(|| false) {
            *active_monitor_idx = monitor_idx;
        }
        Some(id)
    }

    /// All live managed windows, including windows with no layout membership.
    pub fn managed_windows(&self) -> impl Iterator<Item = (Option<&Monitor<W>>, &W)> {
        self.windows().chain(
            self.minimized_windows
                .iter()
                .map(|entry| (None, entry.removed.tile.window())),
        )
    }

    pub fn has_managed_window(&self, id: &W::Id) -> bool {
        self.managed_windows().any(|(_, window)| window.id() == id)
    }

    pub fn with_managed_windows(
        &self,
        mut f: impl FnMut(&W, Option<&Output>, Option<WorkspaceId>, WindowLayout),
    ) {
        self.with_windows(&mut f);
        for entry in &self.minimized_windows {
            f(
                entry.removed.tile.window(),
                None,
                None,
                entry.removed.tile.ipc_layout_template(),
            );
        }
    }

    pub fn with_managed_windows_mut(&mut self, mut f: impl FnMut(&mut W, Option<&Output>)) {
        self.with_windows_mut(&mut f);
        for entry in &mut self.minimized_windows {
            f(entry.removed.tile.window_mut(), None);
        }
    }

    pub fn find_managed_window_and_output(
        &self,
        surface: &WlSurface,
    ) -> Option<(&W, Option<&Output>)> {
        self.find_window_and_output(surface).or_else(|| {
            self.minimized_windows
                .iter()
                .find(|entry| entry.removed.tile.window().is_wl_surface(surface))
                .map(|entry| (entry.removed.tile.window(), None))
        })
    }

    pub fn find_managed_window_and_output_mut(
        &mut self,
        surface: &WlSurface,
    ) -> Option<(&mut W, Option<&Output>)> {
        if let Some(idx) = self
            .minimized_windows
            .iter()
            .position(|entry| entry.removed.tile.window().is_wl_surface(surface))
        {
            return Some((self.minimized_windows[idx].removed.tile.window_mut(), None));
        }
        self.find_window_and_output_mut(surface)
    }

    pub(super) fn set_minimized_fullscreen(&mut self, id: &W::Id, fullscreen: bool) -> bool {
        let Some(entry) = self
            .minimized_windows
            .iter_mut()
            .find(|entry| entry.id() == id)
        else {
            return false;
        };
        let window = entry.removed.tile.window();
        if !fullscreen && window.is_pending_windowed_fullscreen() {
            entry
                .removed
                .tile
                .window_mut()
                .request_windowed_fullscreen(false);
            return true;
        }
        let mode = if fullscreen {
            SizingMode::Fullscreen
        } else if entry.maximized {
            SizingMode::Maximized
        } else {
            SizingMode::Normal
        };
        Self::request_minimized_sizing_mode(entry, mode);
        true
    }

    pub(super) fn set_minimized_maximized(&mut self, id: &W::Id, maximized: bool) -> bool {
        let Some(entry) = self
            .minimized_windows
            .iter_mut()
            .find(|entry| entry.id() == id)
        else {
            return false;
        };
        entry.maximized = maximized;
        if !entry
            .removed
            .tile
            .window()
            .pending_sizing_mode()
            .is_fullscreen()
        {
            let mode = if maximized {
                SizingMode::Maximized
            } else {
                SizingMode::Normal
            };
            Self::request_minimized_sizing_mode(entry, mode);
        }
        true
    }

    fn request_minimized_sizing_mode(entry: &mut MinimizedWindow<W>, mode: SizingMode) {
        let tile = &mut entry.removed.tile;
        if tile.window().pending_sizing_mode() == mode {
            return;
        }
        // The target workspace recomputes output-dependent sizes on restore. While hidden, keep
        // configure handshakes moving without attaching the window to a departed output.
        let size = if mode.is_normal() {
            tile.floating_window_size
                .unwrap_or_else(|| tile.window().size())
        } else {
            tile.window().size()
        };
        tile.window_mut().request_size(size, mode, false, None);
    }
}

//! Output-local snapshot transitions. Window ownership stays in the regular layout/minimized set.
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use anyhow::Context;
use niri_config::animations::MinimizeEffect;
use niri_config::BlockOutFrom;
use niri_ipc::DockEdge;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture, Uniform};
use smithay::desktop::{layer_map_for_output, LayerSurface};
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Scale, Size, Transform};

use super::tile::TileRenderSnapshot;
use super::{Layout, LayoutElement};
use crate::animation::Animation;
use crate::niri_render_elements;
use crate::render_helpers::shader_element::ShaderRenderElement;
use crate::render_helpers::shaders::{ProgramType, Shaders};

niri_render_elements! {
    MinimizeAnimationRenderElement => {
        Texture = PrimaryGpuTextureRenderElement,
        Shader = ShaderRenderElement,
    }
}
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::render_helpers::{render_to_encompassing_texture, RenderTarget};
use crate::utils::{is_mapped, output_size};

#[derive(Debug)]
pub(super) struct TargetHint<I> {
    owner: Weak<()>,
    target: AnimationTarget<I>,
}

#[derive(Debug)]
pub struct AnimationTarget<I> {
    pub id: I,
    pub output: Output,
    pub rect: Rectangle<f64, Logical>,
    pub edge: DockEdge,
    pub layer: Option<LayerSurface>,
}

/// Validate once at the IPC boundary. Never convert logical coordinates to physical twice.
pub fn target_rect(
    rect: [f64; 4],
    output: &Output,
    edge: DockEdge,
) -> Option<Rectangle<f64, Logical>> {
    let [x, y, w, h] = rect;
    if !rect.iter().all(|v| v.is_finite() && v.abs() <= 32768.) || w <= 0. || h <= 0. {
        return None;
    }
    let bounds = Rectangle::from_size(output_size(output));
    let rect = Rectangle::new((x, y).into(), (w, h).into());
    // Auto-hidden panels may advertise a rectangle just beyond the output. Use its edge
    // rather than flying across output coordinate spaces or retaining stale geometry.
    Some(
        rect.intersection(bounds)
            .unwrap_or_else(|| edge_target(bounds.size, edge, rect.loc)),
    )
}

fn edge_target(
    size: Size<f64, Logical>,
    edge: DockEdge,
    near: Point<f64, Logical>,
) -> Rectangle<f64, Logical> {
    let w = 32_f64.min(size.w).max(1.);
    let h = 32_f64.min(size.h).max(1.);
    let x = near.x.clamp(0., (size.w - w).max(0.));
    let y = near.y.clamp(0., (size.h - h).max(0.));
    let loc = match edge {
        DockEdge::Left => (0., y),
        DockEdge::Right => ((size.w - w).max(0.), y),
        DockEdge::Top => (x, 0.),
        DockEdge::Bottom => (x, (size.h - h).max(0.)),
    };
    Rectangle::new(loc.into(), (w, h).into())
}

pub(super) fn interpolate(
    a: Rectangle<f64, Logical>,
    b: Rectangle<f64, Logical>,
    progress: f64,
) -> Rectangle<f64, Logical> {
    let p = progress.clamp(0., 1.);
    Rectangle::new(
        a.loc + (b.loc - a.loc).upscale(p),
        (
            a.size.w + (b.size.w - a.size.w) * p,
            a.size.h + (b.size.h - a.size.h) * p,
        )
            .into(),
    )
}

#[derive(Debug)]
struct Image {
    buffer: TextureBuffer<GlesTexture>,
    offset: Point<f64, Logical>,
}

#[derive(Debug)]
pub struct MinimizeAnimation<I> {
    pub(super) id: I,
    pub(super) output: Output,
    pub(super) restoring: bool,
    window_rect: Rectangle<f64, Logical>,
    /// Fixed for this operation, unaffected by subsequent Dock motion or disconnection.
    dock_rect: Rectangle<f64, Logical>,
    effect: MinimizeEffect,
    edge: DockEdge,
    source_size: Size<f64, Logical>,
    output_size: Size<f64, Logical>,
    output_scale: f64,
    output_transform: Transform,
    normal: Image,
    blocked: Image,
    blocked_background: Option<Image>,
    block_out_from: Option<BlockOutFrom>,
    anim: Animation,
}

impl<I> MinimizeAnimation<I> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        renderer: &mut GlesRenderer,
        id: I,
        output: Output,
        snapshot: TileRenderSnapshot,
        window_rect: Rectangle<f64, Logical>,
        dock_rect: Rectangle<f64, Logical>,
        restoring: bool,
        anim: Animation,
        effect: MinimizeEffect,
        edge: DockEdge,
    ) -> anyhow::Result<Self> {
        let scale = Scale::from(output.current_scale().fractional_scale());
        let mut bake = |elements: Vec<_>| -> anyhow::Result<Image> {
            let (texture, _, geo) = render_to_encompassing_texture(
                renderer,
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                &elements,
            )
            .context("rendering window transition snapshot")?;
            Ok(Image {
                buffer: TextureBuffer::from_texture(
                    renderer,
                    texture,
                    scale,
                    Transform::Normal,
                    Vec::new(),
                ),
                offset: geo.loc.to_f64().to_logical(scale),
            })
        };
        let normal = bake(snapshot.contents)?;
        let blocked = bake(snapshot.blocked_out_contents)?;
        let blocked_background = snapshot
            .contents_with_blocked_out_bg
            .map(bake)
            .transpose()?;
        Ok(Self {
            id,
            output_size: output_size(&output),
            output_scale: output.current_scale().fractional_scale(),
            output_transform: output.current_transform(),
            output,
            restoring,
            effect,
            edge,
            window_rect,
            dock_rect,
            source_size: snapshot.size,
            normal,
            blocked,
            blocked_background,
            block_out_from: snapshot.block_out_from,
            anim,
        })
    }

    pub fn rect(&self) -> Rectangle<f64, Logical> {
        let (from, to) = if self.restoring {
            (self.dock_rect, self.window_rect)
        } else {
            (self.window_rect, self.dock_rect)
        };
        interpolate(from, to, self.anim.clamped_value())
    }

    pub fn render(&self, target: RenderTarget) -> MinimizeAnimationRenderElement {
        let image = if target.should_block_out(self.block_out_from) {
            &self.blocked
        } else if target != RenderTarget::Output {
            self.blocked_background.as_ref().unwrap_or(&self.normal)
        } else {
            &self.normal
        };
        let rect = self.rect();
        let scale = Scale::from((
            rect.size.w / self.source_size.w,
            rect.size.h / self.source_size.h,
        ));
        let location = rect.loc + image.offset.to_physical(scale).to_logical(1.);
        let size = image.buffer.logical_size();
        let size = Size::from((size.w * scale.x, size.h * scale.y));
        let p = self.anim.clamped_value().clamp(0., 1.);
        let opacity = if self.restoring {
            (p / 0.15).min(1.)
        } else {
            ((1. - p) / 0.15).min(1.)
        };
        if self.effect == MinimizeEffect::Genie {
            // Keep the sheet opaque until it reaches the Dock. On restore, a very
            // short fade avoids popping an opaque icon-sized snapshot into view.
            let t = if self.restoring {
                p / 0.04
            } else {
                (1. - p) / 0.06
            }
            .clamp(0., 1.);
            let genie_opacity = t * t * (3. - 2. * t);
            if let Some(element) = self.render_genie(image, genie_opacity as f32, p) {
                return element.into();
            }
        }
        PrimaryGpuTextureRenderElement(TextureRenderElement::from_texture_buffer(
            image.buffer.clone(),
            location,
            opacity as f32,
            Some(Rectangle::from_size(image.buffer.logical_size())),
            Some(size),
            Kind::Unspecified,
        ))
        .into()
    }

    fn render_genie(
        &self,
        image: &Image,
        opacity: f32,
        progress: f64,
    ) -> Option<ShaderRenderElement> {
        let window = self.window_rect;
        let target = self.dock_rect;
        let far = |rect: Rectangle<f64, Logical>| match self.edge {
            DockEdge::Bottom => rect.loc.y,
            DockEdge::Top => -rect.loc.y - rect.size.h,
            DockEdge::Right => rect.loc.x,
            DockEdge::Left => -rect.loc.x - rect.size.w,
        };
        // An unusual hint behind the window can fold the sheet over itself.
        // Keep ordinary scale as the well-defined fallback for that geometry.
        if far(target) < far(window) {
            return None;
        }
        let texture_size = image.buffer.logical_size();
        let mut area = window.merge(target);
        let padding = Point::from((
            image
                .offset
                .x
                .abs()
                .max((image.offset.x + texture_size.w - self.source_size.w).abs())
                / self.source_size.w
                * area.size.w,
            image
                .offset
                .y
                .abs()
                .max((image.offset.y + texture_size.h - self.source_size.h).abs())
                / self.source_size.h
                * area.size.h,
        ));
        area.loc -= padding;
        area.size.w += 2. * padding.x;
        area.size.h += 2. * padding.y;
        area = area.intersection(Rectangle::from_size(self.output_size))?;
        let uniform_rect = |r: Rectangle<f64, Logical>| {
            [
                r.loc.x as f32,
                r.loc.y as f32,
                r.size.w as f32,
                r.size.h as f32,
            ]
        };
        Some(
            ShaderRenderElement::new(
                ProgramType::Genie,
                area.size,
                None,
                self.output_scale as f32,
                opacity,
                Rc::new([
                    Uniform::new("window_rect", uniform_rect(window)),
                    Uniform::new("target_rect", uniform_rect(target)),
                    Uniform::new(
                        "texture_rect",
                        [
                            (image.offset.x / self.source_size.w) as f32,
                            (image.offset.y / self.source_size.h) as f32,
                            (texture_size.w / self.source_size.w) as f32,
                            (texture_size.h / self.source_size.h) as f32,
                        ],
                    ),
                    Uniform::new("area_origin", [area.loc.x as f32, area.loc.y as f32]),
                    Uniform::new(
                        "edge",
                        match self.edge {
                            DockEdge::Bottom => 0_f32,
                            DockEdge::Left => 1.,
                            DockEdge::Right => 2.,
                            DockEdge::Top => 3.,
                        },
                    ),
                    Uniform::new(
                        "morph",
                        if self.restoring {
                            1. - progress as f32
                        } else {
                            progress as f32
                        },
                    ),
                ]),
                HashMap::from([(String::from("niri_tex"), image.buffer.texture().clone())]),
                Kind::Unspecified,
            )
            .with_location(area.loc),
        )
    }
}

impl<W: LayoutElement> Layout<W> {
    pub fn forget_window_animation_targets(&mut self, id: &W::Id) {
        self.minimize_targets.retain(|hint| &hint.target.id != id);
    }

    pub(super) fn remove_animation_output(&mut self, output: &Output) {
        self.minimize_targets
            .retain(|hint| hint.target.output != *output);
        let ids: Vec<_> = self
            .minimize_animations
            .iter()
            .filter(|animation| animation.output == *output)
            .map(|animation| animation.id.clone())
            .collect();
        for id in ids {
            self.cancel_window_animation(&id);
        }
    }

    pub fn clear_window_animation_targets(&mut self, owner: &Rc<()>) {
        let owner = Rc::downgrade(owner);
        self.minimize_targets
            .retain(|hint| !hint.owner.ptr_eq(&owner));
    }

    pub fn set_window_animation_targets(
        &mut self,
        owner: &Rc<()>,
        targets: Vec<AnimationTarget<W::Id>>,
    ) {
        self.clear_window_animation_targets(owner);
        self.minimize_targets
            .extend(targets.into_iter().map(|target| TargetHint {
                owner: Rc::downgrade(owner),
                target,
            }));
    }

    pub fn cancel_window_animation(&mut self, id: &W::Id) {
        self.minimize_animations
            .retain(|animation| &animation.id != id);
        for ws in self.workspaces_mut() {
            for tile in ws.tiles_mut() {
                if tile.window().id() == id {
                    tile.minimize_animation_hidden = false;
                }
            }
        }
    }

    pub fn cancel_minimize_animations(&mut self) {
        self.minimize_animations.clear();
        for ws in self.workspaces_mut() {
            for tile in ws.tiles_mut() {
                tile.minimize_animation_hidden = false;
            }
        }
    }

    fn animation_geometry(&self, id: &W::Id) -> Option<(Output, Rectangle<f64, Logical>)> {
        if self.overview_progress.is_some() {
            return None;
        }
        for mon in self.monitors() {
            for (ws, geo) in mon.workspaces_with_render_geo() {
                for (tile, pos, visible) in ws.tiles_with_render_positions() {
                    if tile.window().id() == id && visible {
                        return Some((
                            mon.output.clone(),
                            Rectangle::new(geo.loc + pos, tile.animated_tile_size()),
                        ));
                    }
                }
            }
        }
        None
    }

    /// Consume the same privacy-aware snapshot machinery used for unmapping, without unmapping.
    pub fn prepare_minimize_animation(
        &mut self,
        renderer: &mut GlesRenderer,
        id: &W::Id,
        restoring: bool,
    ) -> Option<MinimizeAnimation<W::Id>> {
        let geometry = self.animation_geometry(id);
        let snapshot = self.workspaces_mut().find_map(|ws| {
            ws.tiles_mut()
                .find(|tile| tile.window().id() == id)
                .and_then(|tile| tile.take_unmap_snapshot())
        })?;
        let (output, window_rect) = geometry?;
        let anim = Animation::new(
            self.clock.clone(),
            0.,
            1.,
            0.,
            self.options.animations.window_minimize().0,
        );
        if anim.is_done() || snapshot.size.w <= 0. || snapshot.size.h <= 0. {
            return None;
        }
        let size = output_size(&output);
        let (dock_rect, edge) = self
            .minimize_targets
            .iter()
            .rev()
            .filter(|hint| hint.owner.strong_count() > 0)
            .map(|hint| &hint.target)
            .find(|hint| hint.id == *id && hint.output == output)
            .and_then(|hint| {
                let mut rect = hint.rect;
                if let Some(layer) = &hint.layer {
                    // Resolve the surface's current output-local origin at operation time.
                    // Keeping its identity also invalidates hints when the surface disappears.
                    let layers = layer_map_for_output(&output);
                    if !is_mapped(layer.layer_surface().wl_surface()) {
                        return None;
                    }
                    rect.loc += layers.layer_geometry(layer)?.loc.to_f64();
                }
                target_rect(
                    [rect.loc.x, rect.loc.y, rect.size.w, rect.size.h],
                    &output,
                    hint.edge,
                )
                .map(|rect| (rect, hint.edge))
            })
            .unwrap_or_else(|| {
                (
                    edge_target(size, DockEdge::Bottom, (size.w / 2. - 16., size.h).into()),
                    DockEdge::Bottom,
                )
            });
        let effect = if Shaders::get(renderer).program(ProgramType::Genie).is_some() {
            self.options.animations.window_minimize_effect
        } else {
            MinimizeEffect::Scale
        };
        match MinimizeAnimation::new(
            renderer,
            id.clone(),
            output,
            snapshot,
            window_rect,
            dock_rect,
            restoring,
            anim,
            effect,
            edge,
        ) {
            Ok(animation) => Some(animation),
            Err(err) => {
                warn!("error preparing minimize animation: {err:?}");
                None
            }
        }
    }

    pub fn start_minimize_animation(&mut self, animation: MinimizeAnimation<W::Id>) {
        self.cancel_window_animation(&animation.id);
        if animation.restoring {
            for ws in self.workspaces_mut() {
                for tile in ws.tiles_mut() {
                    if tile.window().id() == &animation.id {
                        tile.minimize_animation_hidden = true;
                    }
                }
            }
        }
        self.minimize_animations.push(animation);
    }

    pub(super) fn advance_minimize_animations(&mut self) {
        if self.minimize_animations.is_empty() {
            return;
        }
        let animations = std::mem::take(&mut self.minimize_animations);
        for mut animation in animations {
            let output_alive = self.monitors().any(|mon| mon.output == animation.output);
            if animation.anim.is_done()
                || !output_alive
                || output_size(&animation.output) != animation.output_size
                || animation.output.current_scale().fractional_scale() != animation.output_scale
                || animation.output.current_transform() != animation.output_transform
                || !self.has_managed_window(&animation.id)
                || self.overview_progress.is_some()
            {
                continue;
            }
            if animation.restoring {
                let Some((output, rect)) = self.animation_geometry(&animation.id) else {
                    continue;
                };
                if output != animation.output {
                    continue;
                }
                // Follow the actual inserted tile while its transaction/scroll movement settles.
                // The Dock endpoint stays fixed. Completion always reveals the live tile, even
                // when a client never acknowledges a new size; no unbounded frame wait exists.
                animation.window_rect = rect;
            }
            self.minimize_animations.push(animation);
        }
        let hidden: Vec<_> = self
            .minimize_animations
            .iter()
            .filter(|a| a.restoring)
            .map(|a| a.id.clone())
            .collect();
        for ws in self.workspaces_mut() {
            for tile in ws.tiles_mut() {
                tile.minimize_animation_hidden = hidden.contains(tile.window().id());
            }
        }
    }

    pub fn render_minimize_animations(
        &self,
        output: &Output,
        target: RenderTarget,
        push: &mut dyn FnMut(MinimizeAnimationRenderElement),
    ) {
        for animation in self.minimize_animations.iter().rev() {
            if animation.output == *output {
                push(animation.render(target));
            }
        }
    }
}

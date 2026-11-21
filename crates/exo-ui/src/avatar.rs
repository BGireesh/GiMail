//! An account's avatar: its profile photo in a ring of its color, or a circle in a color with a
//! letter on it, standing in for the photo (the account switchers, contact lists).

use std::f32::consts::TAU;

use egui::{
    Color32, ColorImage, Mesh, Painter, Rect, Shape, Stroke, TextureHandle, TextureOptions, Vec2,
    epaint::Vertex, pos2, vec2,
};

use crate::TextStyle;

/// Points on the photo's edge: smooth past 56px, which is the largest avatar.
const EDGE_POINTS: usize = 64;
/// A photo's halvings stop before they get smaller than this, in texels.
const SMALLEST_LEVEL: usize = 8;

/// Paints the circle filling `rect` in `fill`, with `initial` centered on it in `style` and
/// `ink`. `dy` is the SVG text's `dy`, which is how far its baseline sits below the circle's
/// center.
pub fn paint_initial_avatar(
    painter: &Painter,
    rect: Rect,
    fill: Color32,
    initial: &str,
    style: &TextStyle,
    ink: Color32,
    dy: f32,
) {
    let center = rect.center();
    painter.circle_filled(center, rect.width() / 2.0, fill);
    let ctx = painter.ctx();
    let width = style.width(ctx, initial);
    // `text-anchor: middle` at the circle's center, the baseline `dy` below it.
    style.paint_line(
        painter,
        pos2(center.x - width / 2.0, center.y + dy - style.baseline(ctx)),
        initial,
        ink,
        None,
    );
}

/// Paints the avatar of someone without a photo: a disc filling `rect`, with a head and shoulders
/// cut to it. In a 56px avatar the head is a circle of 8.5px at (28, 16.5) and the shoulders an
/// ellipse of 21.5 by 18.5px at (28, 48.5). The disc is `#bfc8d2` with the figure in white at .6
/// (Carbon `#363a40`, .07), or a `small` one's `#c9d0d9` at .45 (Carbon `#606774`, .2).
pub fn paint_default_avatar(painter: &Painter, rect: Rect, small: bool) {
    const POINTS: usize = 64;
    let carbon = crate::palette(painter.ctx()).theme == crate::Theme::Carbon;
    let (background, figure) = match (small, carbon) {
        (false, false) => (Color32::from_rgb(0xbf, 0xc8, 0xd2), 0.6),
        (false, true) => (Color32::from_rgb(0x36, 0x3a, 0x40), 0.07),
        (true, false) => (Color32::from_rgb(0xc9, 0xd0, 0xd9), 0.45),
        (true, true) => (Color32::from_rgb(0x60, 0x67, 0x74), 0.2),
    };
    let figure = crate::motion::mix(background, Color32::WHITE, figure);
    let unit = rect.width() / 56.0;
    let at = |x: f32, y: f32| rect.min + vec2(x, y) * unit;
    let (center, radius) = (rect.center(), rect.width() / 2.0);
    painter.circle_filled(center, radius, background);
    painter.circle_filled(at(28.0, 16.5), 8.5 * unit, figure);
    // The shoulders where they're on the disc: both are convex, and so is what they share.
    let (shoulders, half) = (at(28.0, 48.5), vec2(21.5, 18.5) * unit);
    let around = |index: usize| {
        let angle = TAU * index as f32 / POINTS as f32;
        vec2(angle.cos(), angle.sin())
    };
    let mut points: Vec<egui::Pos2> = (0..POINTS)
        .map(|index| shoulders + around(index) * half)
        .filter(|point| point.distance(center) <= radius)
        .chain(
            (0..POINTS)
                .map(|index| center + around(index) * radius)
                .filter(|point| ((*point - shoulders) / half).length() <= 1.0),
        )
        .collect();
    let inside = at(28.0, 46.0);
    points.sort_by(|a, b| {
        let angle = |point: &egui::Pos2| (point.y - inside.y).atan2(point.x - inside.x);
        angle(a).total_cmp(&angle(b))
    });
    painter.add(Shape::convex_polygon(points, figure, Stroke::NONE));
}

/// A profile photo, ready to draw at an avatar's size the way Chromium draws an image scaled
/// down. It's drawn from the smallest of its halvings (mip levels, averaged 2×2 as Skia makes
/// them) that's still at least the size drawn (`GpuImageDecodeCache`'s upload level), sampled
/// bilinearly. Cheap to clone.
#[derive(Clone)]
pub struct AvatarPhoto {
    /// The photo, then each half the size of the one before.
    levels: Vec<TextureHandle>,
}

/// Shows its textures' ids in place of the textures.
impl std::fmt::Debug for AvatarPhoto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.levels.iter().map(TextureHandle::id))
            .finish()
    }
}

impl AvatarPhoto {
    /// `image` as textures of `ctx`, named `name`.
    pub fn new(ctx: &egui::Context, name: &str, mut image: ColorImage) -> Self {
        let mut levels = Vec::new();
        loop {
            let half =
                (image.size.iter().all(|&side| side / 2 >= SMALLEST_LEVEL)).then(|| halve(&image));
            levels.push(ctx.load_texture(name, image, TextureOptions::LINEAR));
            match half {
                Some(half) => image = half,
                None => return Self { levels },
            }
        }
    }

    /// The level to draw `pixels` (device pixels) large from.
    fn level(&self, pixels: Vec2) -> &TextureHandle {
        let covers = |level: &&TextureHandle| {
            level.size_vec2().x >= pixels.x && level.size_vec2().y >= pixels.y
        };
        self.levels
            .iter()
            .take_while(covers)
            .last()
            .unwrap_or(&self.levels[0])
    }

    /// Every texture it draws from, for tests that look for it in a frame.
    pub fn textures(&self) -> impl Iterator<Item = egui::TextureId> + '_ {
        self.levels.iter().map(TextureHandle::id)
    }
}

/// `image` half as large, each pixel the average of the 2×2 under it.
fn halve(image: &ColorImage) -> ColorImage {
    let [width, height] = image.size.map(|side| side / 2);
    let pixel = |x: usize, y: usize| image.pixels[y * image.size[0] + x].to_array();
    let pixels = (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| {
            let quad = [
                pixel(2 * x, 2 * y),
                pixel(2 * x + 1, 2 * y),
                pixel(2 * x, 2 * y + 1),
                pixel(2 * x + 1, 2 * y + 1),
            ];
            let channel =
                |c: usize| ((quad.iter().map(|p| u16::from(p[c])).sum::<u16>() + 2) / 4) as u8;
            Color32::from_rgba_premultiplied(channel(0), channel(1), channel(2), channel(3))
        })
        .collect();
    ColorImage::new([width, height], pixels)
}

/// Paints `photo` as account avatars show one. That's an element filling `rect` with
/// `border-radius: 50%` and a `ring`-wide solid border in `color` (`box-sizing: border-box`), with
/// the photo as its `background-size: cover` image centered in the padding box, all at `opacity`.
///
/// Chromium paints the background under the border. Here the photo stops half a device pixel
/// past the ring's inner edge, so the ring's antialiasing blends into the photo while the two
/// barely overlap, which only shows when `opacity` is under 1 (the group is one layer there).
pub fn paint_photo_avatar(
    painter: &Painter,
    rect: Rect,
    photo: &AvatarPhoto,
    ring: f32,
    color: Color32,
    opacity: f32,
) {
    let center = rect.center();
    let padding = rect.shrink(ring);
    let reach = rect.width() / 2.0 - ring + 0.5 / painter.pixels_per_point();
    let tint = Color32::WHITE.gamma_multiply(opacity);
    painter.add(Shape::mesh(disc(
        painter, padding, photo, reach, None, tint,
    )));
    // egui strokes a circle outside its radius.
    painter.circle_stroke(
        center,
        rect.width() / 2.0 - ring,
        Stroke::new(ring, color.gamma_multiply(opacity)),
    );
}

/// Paints `photo` as a contact's avatar shows one: an element filling `rect` with
/// `border-radius: 50%` and the photo as its `background-size: cover` image, centered. Its edge
/// is antialiased over a device pixel, as Chromium clips a rounded background.
pub fn paint_photo(painter: &Painter, rect: Rect, photo: &AvatarPhoto) {
    let half = 0.5 / painter.pixels_per_point();
    let mesh = disc(
        painter,
        rect,
        photo,
        rect.width() / 2.0 - half,
        Some(2.0 * half),
        Color32::WHITE,
    );
    painter.add(Shape::mesh(mesh));
}

/// A disc of `photo` covering `rect` (`background-size: cover`, centered) out to `reach` from its
/// center, tinted `tint`, and with `feather` a ring that far out again fading to nothing.
fn disc(
    painter: &Painter,
    rect: Rect,
    photo: &AvatarPhoto,
    reach: f32,
    feather: Option<f32>,
    tint: Color32,
) -> Mesh {
    let center = rect.center();
    let texture = photo.level(rect.size() * painter.pixels_per_point());
    let size = texture.size_vec2();
    let scale = (rect.width() / size.x).max(rect.height() / size.y);
    // With `no-repeat`, the image paints only where it lands.
    let image = Rect::from_center_size(center, size * scale);
    let vertex = |pos: egui::Pos2, color: Color32| Vertex {
        pos,
        uv: ((pos - image.min) / image.size()).to_pos2(),
        color,
    };
    let edge = |index: usize, reach: f32| {
        let angle = TAU * index as f32 / EDGE_POINTS as f32;
        (center + reach * vec2(angle.cos(), angle.sin())).clamp(image.min, image.max)
    };
    let mut mesh = Mesh::with_texture(texture.id());
    mesh.vertices.push(vertex(center, tint));
    for index in 0..EDGE_POINTS {
        mesh.vertices.push(vertex(edge(index, reach), tint));
        let next = (index + 1) % EDGE_POINTS;
        mesh.add_triangle(0, index as u32 + 1, next as u32 + 1);
    }
    if let Some(feather) = feather {
        let outer = mesh.vertices.len() as u32;
        for index in 0..EDGE_POINTS {
            mesh.vertices
                .push(vertex(edge(index, reach + feather), Color32::TRANSPARENT));
        }
        for index in 0..EDGE_POINTS as u32 {
            let next = (index + 1) % EDGE_POINTS as u32;
            mesh.add_triangle(index + 1, next + 1, outer + index);
            mesh.add_triangle(next + 1, outer + next, outer + index);
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_photo_is_drawn_from_the_smallest_level_still_as_large_as_the_avatar() {
        let ctx = egui::Context::default();
        let photo = AvatarPhoto::new(
            &ctx,
            "photo",
            ColorImage::new([100, 100], vec![Color32::WHITE; 100 * 100]),
        );
        let sizes: Vec<[usize; 2]> = photo.levels.iter().map(TextureHandle::size).collect();
        assert_eq!(sizes, [[100, 100], [50, 50], [25, 25], [12, 12]]);
        let drawn = |pixels: f32| photo.level(Vec2::splat(pixels)).size()[0];
        // The folder drawer's 26px photo at 2× (Chromium's level 0), the palette's 20px (level 1).
        assert_eq!(
            (
                drawn(52.0),
                drawn(40.0),
                drawn(50.0),
                drawn(20.0),
                drawn(200.0)
            ),
            (100, 50, 50, 25, 100)
        );
    }

    #[test]
    fn halving_averages_each_square_of_four() {
        let pixels = [0, 10, 20, 30].map(|v| Color32::from_rgb(v, v, v)).to_vec();
        let half = halve(&ColorImage::new([2, 2], pixels));
        assert_eq!(half.pixels, [Color32::from_rgb(15, 15, 15)]);
    }
}

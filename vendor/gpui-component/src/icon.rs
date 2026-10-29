// Krill modification: preserve original full-color brand images and their theme variants.
// Based on gpui-component (https://github.com/longbridge/gpui-kit), Apache-2.0.
use std::sync::Arc;

use crate::{ActiveTheme, Sizable, Size};
use gpui::{
    AnyElement, App, AppContext, Asset, Context, DevicePixels, Entity, Hsla, Image,
    ImageCacheError, ImageFormat, ImageSource, Img, IntoElement, Length, ObjectFit, Pixels,
    Radians, Render, RenderImage, RenderOnce, SharedString, StyleRefinement, Styled,
    StyledImage as _, Svg, SvgSize, Transformation, Window, img, prelude::FluentBuilder as _, svg,
};
pub use gpui_kit_assets::IconNamed;

// Preserve the original enum (including exhaustive matches and inherent view)
// while the complete, shared catalog is owned by gpui-kit-assets.
macro_rules! component_icon_names {
    ($($name:ident => $path:literal,)*) => {
        /// Default component icon names, retained for source compatibility.
        /// For the complete Lucide catalog, use `gpui_kit_assets::IconName`.
        #[derive(Clone, IntoElement)]
        pub enum IconName {
            $($name,)*
        }

        impl From<IconName> for gpui_kit_assets::IconName {
            fn from(name: IconName) -> Self {
                match name {
                    $(IconName::$name => Self::$name,)*
                }
            }
        }

        impl IconNamed for IconName {
            fn path(self) -> SharedString {
                match self { $(Self::$name => $path,)* }.into()
            }
        }
    };
}

gpui_kit_assets::__component_icon_names!(component_icon_names);

impl IconName {
    /// Return the icon as an Entity<Icon>.
    pub fn view(self, cx: &mut App) -> Entity<Icon> {
        Icon::build(self).view(cx)
    }
}

impl From<IconName> for AnyElement {
    fn from(name: IconName) -> Self {
        Icon::build(name).into_any_element()
    }
}

impl RenderOnce for IconName {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        Icon::build(self)
    }
}

impl<T: IconNamed> From<T> for Icon {
    fn from(value: T) -> Self {
        Icon::build(value)
    }
}

/// Component view construction for the shared `gpui_kit_assets::IconName`.
/// The legacy component `IconName` retains its inherent `view` method.
pub trait IconNameExt {
    fn view(self, cx: &mut App) -> Entity<Icon>;
}

impl IconNameExt for gpui_kit_assets::IconName {
    fn view(self, cx: &mut App) -> Entity<Icon> {
        Icon::build(self).view(cx)
    }
}

#[derive(Clone)]
pub(crate) enum IconSource {
    Path(SharedString),
    Data(Arc<[u8]>),
    Image { light: Arc<Image>, dark: Arc<Image> },
}

// SVG images otherwise rasterize at their intrinsic dimensions. Cache small,
// color-preserving rasters keyed by source and requested device-pixel width.
struct IconImageRaster;

impl Asset for IconImageRaster {
    type Source = (Arc<Image>, DevicePixels);
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        async move { raster_icon_image(&source.0, source.1, &renderer) }
    }
}

pub(crate) fn raster_icon_image(
    image: &Image,
    width: DevicePixels,
    renderer: &gpui::SvgRenderer,
) -> Result<Arc<RenderImage>, ImageCacheError> {
    let parsed = renderer.parse_svg(image.bytes())?;
    renderer
        .render_parsed(&parsed, SvgSize::Size(gpui::size(width, width)))
        .map_err(Into::into)
}

#[derive(Clone, IntoElement)]
pub struct Icon {
    style: StyleRefinement,
    source: IconSource,
    text_color: Option<Hsla>,
    size: Option<Size>,
    transformation: Option<Transformation>,
}

impl Default for Icon {
    fn default() -> Self {
        Self {
            style: StyleRefinement::default(),
            source: IconSource::Path("".into()),
            text_color: None,
            size: None,
            transformation: None,
        }
    }
}

impl Icon {
    pub fn new(icon: impl Into<Icon>) -> Self {
        icon.into()
    }

    fn build(name: impl IconNamed) -> Self {
        Self::default().path(name.path())
    }

    /// Set the icon path of the Assets bundle
    ///
    /// For example: `icons/foo.svg`
    /// Replaces any previously set path or SVG data.
    pub fn path(mut self, path: impl Into<SharedString>) -> Self {
        self.source = IconSource::Path(path.into());
        self
    }

    /// Set raw SVG bytes without registering an asset path.
    ///
    /// Copies the bytes into shared storage; the input need not be static.
    /// Cloning the icon shares those bytes. Replaces any previously set path or data.
    /// Parsing and rendering follow GPUI's SVG behavior.
    ///
    /// ```
    /// use gpui_component::Icon;
    ///
    /// let bytes = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">
    ///     <path d="M4 12h16" stroke="currentColor"/>
    /// </svg>"#;
    /// let icon = Icon::default().data(bytes);
    /// ```
    pub fn data(mut self, data: &[u8]) -> Self {
        self.source = IconSource::Data(Arc::from(data));
        self
    }

    /// Use original image bytes, selecting a variant for the current theme.
    ///
    /// Unlike SVG mask icons, these images retain their own colors. Text color
    /// styling does not recolor them. Clones share the original images.
    pub fn image_variants(mut self, light: Arc<Image>, dark: Arc<Image>) -> Self {
        self.source = IconSource::Image { light, dark };
        self
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub(crate) fn select_image_variant(&mut self, is_dark: bool) {
        if let IconSource::Image { light, dark } = &self.source {
            let selected = if is_dark { dark } else { light }.clone();
            self.source = IconSource::Image {
                light: selected.clone(),
                dark: selected,
            };
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows", test))]
    pub(crate) fn source_ref(&self) -> &IconSource {
        &self.source
    }

    /// Whether `other` takes up exactly the same space as this icon. The
    /// source, color and transformation change only what it paints.
    pub(crate) fn same_layout(&self, other: &Self) -> bool {
        self.style == other.style && self.size == other.size
    }

    /// Create a new view for the icon
    pub fn view(self, cx: &mut App) -> Entity<Icon> {
        cx.new(|_| self)
    }

    /// Set the SVG transformation, replacing any previous transformation or rotation.
    pub fn transform(mut self, transformation: gpui::Transformation) -> Self {
        self.transformation = Some(transformation);
        self
    }

    pub fn empty() -> Self {
        Self::default()
    }

    /// Rotate the icon by the given angle
    ///
    /// Replaces any previous transformation or rotation.
    pub fn rotate(mut self, radians: impl Into<Radians>) -> Self {
        self.transformation = Some(Transformation::rotate(radians));
        self
    }

    fn into_svg(self, text_size: Pixels, fallback_color: Hsla) -> Svg {
        let text_color = self.text_color.unwrap_or(fallback_color);
        let has_base_size = self.style.size.width.is_some() || self.style.size.height.is_some();

        svg()
            .map(|mut this| {
                *this.style() = self.style;
                this
            })
            .flex_shrink_0()
            .text_color(text_color)
            .when(!has_base_size, |this| this.size(text_size))
            .when_some(self.size, |this, size| match size {
                Size::Size(px) => this.size(px),
                Size::XSmall => this.size_3(),
                Size::Small => this.size_3p5(),
                Size::Medium => this.size_4(),
                Size::Large => this.size_6(),
            })
            .when_some(self.transformation, |this, transformation| {
                this.with_transformation(transformation)
            })
    }

    fn into_image(
        self,
        image: Arc<Image>,
        text_size: Pixels,
        rem_size: Pixels,
        scale_factor: f32,
    ) -> Img {
        let has_base_size = self.style.size.width.is_some() || self.style.size.height.is_some();
        let width = match self.size {
            Some(Size::Size(width)) => width,
            Some(Size::XSmall) => rem_size * 0.75,
            Some(Size::Small) => rem_size * 0.875,
            Some(Size::Medium) => rem_size,
            Some(Size::Large) => rem_size * 1.5,
            None => match self.style.size.width {
                Some(Length::Definite(width)) => width.to_pixels(text_size.into(), rem_size),
                _ => text_size,
            },
        };
        let source = if image.format == ImageFormat::Svg {
            // Two samples per device pixel keep small marks sharp, without
            // allocating textures at the large intrinsic press-kit dimensions.
            let device_width = DevicePixels(
                (f32::from(width) * scale_factor * 2.)
                    .ceil()
                    .clamp(1., 4096.) as i32,
            );
            ImageSource::from(move |window: &mut Window, cx: &mut App| {
                window.use_asset::<IconImageRaster>(&(image.clone(), device_width), cx)
            })
        } else {
            ImageSource::from(image)
        };
        img(source)
            .map(|mut this| {
                *this.style() = self.style;
                this
            })
            .object_fit(ObjectFit::Contain)
            .flex_shrink_0()
            .when(!has_base_size, |this| this.size(text_size))
            .when_some(self.size, |this, size| match size {
                Size::Size(px) => this.size(px),
                Size::XSmall => this.size_3(),
                Size::Small => this.size_3p5(),
                Size::Medium => this.size_4(),
                Size::Large => this.size_6(),
            })
    }

    fn into_icon_element(
        self,
        text_size: Pixels,
        fallback_color: Hsla,
        is_dark: bool,
        rem_size: Pixels,
        scale_factor: f32,
    ) -> AnyElement {
        match self.source.clone() {
            IconSource::Path(path) => self
                .into_svg(text_size, fallback_color)
                .path(path)
                .into_any_element(),
            IconSource::Data(data) => self
                .into_svg(text_size, fallback_color)
                .data(&data)
                .into_any_element(),
            IconSource::Image { light, dark } => self
                .into_image(
                    if is_dark { dark } else { light },
                    text_size,
                    rem_size,
                    scale_factor,
                )
                .into_any_element(),
        }
    }
}

impl Styled for Icon {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }

    fn text_color(mut self, color: impl Into<Hsla>) -> Self {
        self.text_color = Some(color.into());
        self
    }
}

impl Sizable for Icon {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = Some(size.into());
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let text_size = window.text_style().font_size.to_pixels(window.rem_size());
        self.into_icon_element(
            text_size,
            window.text_style().color,
            cx.theme().is_dark(),
            window.rem_size(),
            window.scale_factor(),
        )
    }
}

impl From<Icon> for AnyElement {
    fn from(val: Icon) -> Self {
        val.into_any_element()
    }
}

impl Render for Icon {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let text_size = window.text_style().font_size.to_pixels(window.rem_size());
        self.clone().into_icon_element(
            text_size,
            cx.theme().foreground,
            cx.theme().is_dark(),
            window.rem_size(),
            window.scale_factor(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size};

    const SVG: &[u8] = include_bytes!("../../assets/assets/icons/arrow-up.svg");

    #[test]
    fn test_icon_builder_preserves_owned_data_and_transform_on_clone() {
        let transformation = Transformation::scale(size(0.5, 0.5))
            .with_rotation(gpui::radians(std::f32::consts::FRAC_PI_2));
        let icon = {
            let bytes = SVG.to_vec();
            Icon::default()
                .data(&bytes)
                .large()
                .text_color(gpui::red())
                .transform(transformation)
        };
        let cloned = icon.clone();
        let (IconSource::Data(original), IconSource::Data(copy)) = (&icon.source, &cloned.source)
        else {
            panic!("cloning must preserve the data source");
        };
        assert_eq!(copy.as_ref(), SVG);
        assert!(Arc::ptr_eq(original, copy));
        assert_eq!(cloned.transformation, Some(transformation));
        assert_eq!(cloned.size, icon.size);
        assert_eq!(cloned.text_color, icon.text_color);

        let mut svg = cloned.into_svg(px(12.), gpui::blue());
        assert_eq!(svg.style().text.color, Some(gpui::red()));
        assert_eq!(svg.style().size.width, Some(gpui::rems(1.5).into()));

        let rotated = icon.rotate(gpui::radians(std::f32::consts::PI)).clone();
        assert_eq!(
            rotated.transformation,
            Some(Transformation::rotate(gpui::radians(std::f32::consts::PI)))
        );
    }

    #[test]
    fn test_icon_source_builders_replace_previous_source() {
        let icon = Icon::new(IconName::Search).data(SVG);
        assert!(matches!(icon.source_ref(), IconSource::Data(bytes) if bytes.as_ref() == SVG));

        let icon = icon.path("icons/replacement.svg");
        assert!(
            matches!(icon.source_ref(), IconSource::Path(path) if path == "icons/replacement.svg")
        );

        let icon = icon.data(SVG).data(b"replacement");
        assert!(
            matches!(icon.source_ref(), IconSource::Data(bytes) if bytes.as_ref() == b"replacement")
        );

        let icon = icon.path("");
        assert!(matches!(icon.source_ref(), IconSource::Path(path) if path.is_empty()));
    }

    #[test]
    fn image_icons_share_original_bytes_and_keep_size_without_text_tint() {
        let light = Arc::new(Image::from_bytes(ImageFormat::Svg, SVG.to_vec()));
        let dark = Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            b"dark variant".to_vec(),
        ));
        let icon = Icon::default()
            .image_variants(light.clone(), dark.clone())
            .with_size(px(19.))
            .mt(px(3.))
            .text_color(gpui::red());
        let cloned = icon.clone();
        let IconSource::Image {
            light: copy_light,
            dark: copy_dark,
        } = cloned.source_ref()
        else {
            panic!("image variant source must survive cloning");
        };
        assert!(Arc::ptr_eq(copy_light, &light));
        assert!(Arc::ptr_eq(copy_dark, &dark));
        assert_eq!(copy_light.bytes(), SVG);
        assert!(icon.same_layout(&cloned));

        let mut plain = icon.clone().into_image(light.clone(), px(12.), px(16.), 2.);
        let mut tinted =
            icon.clone()
                .text_color(gpui::blue())
                .into_image(light, px(12.), px(16.), 2.);
        assert!(plain.style() == tinted.style());
        assert_eq!(plain.style().size.width, Some(px(19.).into()));
        assert_eq!(plain.style().margin.top, Some(px(3.).into()));
        assert_eq!(plain.style().text.color, None);

        let replaced = icon.clone().data(SVG);
        assert!(matches!(replaced.source_ref(), IconSource::Data(_)));
        let replaced = icon.path("icons/replaced.svg");
        assert!(matches!(replaced.source_ref(), IconSource::Path(_)));
    }

    #[test]
    fn original_svg_raster_preserves_color_and_aspect_at_icon_dimensions() {
        let bytes = br##"<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="500" viewBox="0 0 1000 500"><path d="M0 0H500V500H0Z" fill="#ff0000"/><path d="M500 0H1000V500H500Z" fill="#00ff00"/></svg>"##;
        let image = Image::from_bytes(ImageFormat::Svg, bytes.to_vec());
        let renderer = gpui::SvgRenderer::new(Arc::new(()));
        let rendered = raster_icon_image(&image, DevicePixels(24), &renderer).unwrap();
        assert_eq!(
            rendered.size(0),
            gpui::size(DevicePixels(24), DevicePixels(12))
        );
        let pixels = rendered.as_bytes(0).unwrap();
        assert!(pixels.chunks_exact(4).any(|p| p == [0, 0, 255, 255]));
        assert!(pixels.chunks_exact(4).any(|p| p == [0, 255, 0, 255]));
        assert_eq!(image.bytes(), bytes);
    }
}

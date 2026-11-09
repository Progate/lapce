//! 画像ファイルを見るタブ。本物の Lapce には無い。
//!
//! ラスター画像（PNG・JPEG・GIF・WebP・BMP・ICO）はファイルを開くとこのタブになる。文字として編集できないため。
//! SVG は文字のまま編集することが多いので、ファイルはエディタで開き、`open_image_preview` でこのタブを出す。
//! SVG をエディタでも開いているあいだは、保存を待たずに編集中の文字を描く。
//!
//! ラスター画像の読み込みと復号はスレッドで行い、画面は待たない。表示は元の大きさで、入りきらないときだけ
//! 縦横比を保って縮める（VS Code の画像プレビューと同じ）。下の行に形式・寸法・ファイルの大きさを出す。

use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use floem::{
    IntoView, View,
    ext_event::create_ext_action,
    kurbo::Size,
    peniko::{self, Blob},
    reactive::{
        ReadSignal, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith,
        create_effect, create_rw_signal,
    },
    views::{
        Decorators, container, dyn_container, empty, img_dynamic, label, stack, svg,
        text,
    },
};

use crate::{
    config::{LapceConfig, color::LapceColor},
    doc::Doc,
    i18n,
};

/// 開くとエディタではなく画像のタブになる拡張子（文字として編集できないもの）
const RASTER_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

fn is_svg(path: &Path) -> bool {
    extension(path).as_deref() == Some("svg")
}

/// ファイルを開いたときに画像のタブにするか
pub fn opens_as_image(path: &Path) -> bool {
    extension(path).is_some_and(|ext| RASTER_EXTENSIONS.contains(&ext.as_str()))
}

/// 画像のタブで見られるか（SVG を含む）
pub fn can_preview(path: &Path) -> bool {
    opens_as_image(path) || is_svg(path)
}

#[derive(Clone)]
enum Picture {
    Raster(peniko::Image),
    Svg(String),
}

#[derive(Clone)]
struct Loaded {
    picture: Picture,
    /// 元の大きさ（論理ピクセル）
    size: Size,
    format: String,
    bytes: u64,
}

fn load_svg(text: String, bytes: u64) -> Result<Loaded, String> {
    let tree = resvg::usvg::Tree::from_str(&text, &resvg::usvg::Options::default())
        .map_err(|err| err.to_string())?;
    let size = tree.size();
    Ok(Loaded {
        picture: Picture::Svg(text),
        size: Size::new(size.width() as f64, size.height() as f64),
        format: "SVG".to_string(),
        bytes,
    })
}

fn load_file(path: &Path) -> Result<Loaded, String> {
    let data = std::fs::read(path).map_err(|err| err.to_string())?;
    let bytes = data.len() as u64;
    if is_svg(path) {
        let text = String::from_utf8(data).map_err(|err| err.to_string())?;
        return load_svg(text, bytes);
    }
    let image = image::load_from_memory(&data).map_err(|err| err.to_string())?;
    let (width, height) = (image.width(), image.height());
    let rgba = image.into_rgba8().into_vec();
    Ok(Loaded {
        picture: Picture::Raster(peniko::Image::new(
            Blob::new(Arc::new(rgba)),
            peniko::ImageFormat::Rgba8,
            width,
            height,
        )),
        size: Size::new(width as f64, height as f64),
        format: extension(path).unwrap_or_default().to_ascii_uppercase(),
        bytes,
    })
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

/// 入りきらないときだけ縦横比を保って縮めた大きさ
fn fit(natural: Size, available: Size) -> Size {
    if natural.width <= 0.0 || natural.height <= 0.0 {
        return Size::ZERO;
    }
    let scale = (available.width / natural.width)
        .min(available.height / natural.height)
        .clamp(0.0, 1.0);
    Size::new(natural.width * scale, natural.height * scale)
}

/// 画像のタブの中身
///
/// `docs` は開いている文書。SVG をエディタでも開いていれば、その文字を描く
pub fn image_preview_view(
    path: PathBuf,
    docs: RwSignal<im::HashMap<PathBuf, Rc<Doc>>>,
    config: ReadSignal<Arc<LapceConfig>>,
) -> impl View {
    let cx = Scope::current();
    let loaded: RwSignal<Option<Result<Loaded, String>>> = create_rw_signal(None);
    let available = create_rw_signal(Size::ZERO);

    if is_svg(&path) {
        // エディタで開いている SVG は、その文字を描く（打つたびに描き直す）。開いていなければファイルを読む
        let path = path.clone();
        create_effect(move |_| {
            let doc = docs.with(|docs| docs.get(&path).cloned());
            let text = doc.and_then(|doc| {
                doc.loaded
                    .get()
                    .then(|| doc.buffer.with(|buffer| buffer.to_string()))
            });
            let result = match text {
                Some(text) => {
                    let bytes = text.len() as u64;
                    load_svg(text, bytes)
                }
                None => load_file(&path),
            };
            loaded.set(Some(result));
        });
    } else {
        let send = create_ext_action(cx, move |result| loaded.set(Some(result)));
        std::thread::Builder::new()
            .name("ImagePreview".to_owned())
            .spawn(move || send(load_file(&path)))
            .ok();
    }

    // 絵が変わったときだけ作り直す（大きさの変化は style で追う）
    let picture = dyn_container(
        move || {
            loaded.with(|loaded| match loaded {
                Some(Ok(loaded)) => Ok(Some(loaded.picture.clone())),
                Some(Err(err)) => Err(err.clone()),
                None => Ok(None),
            })
        },
        move |picture| {
            let shown = move || {
                let natural = loaded.with(|loaded| {
                    loaded
                        .as_ref()
                        .and_then(|loaded| loaded.as_ref().ok())
                        .map(|loaded| loaded.size)
                        .unwrap_or_default()
                });
                fit(natural, available.get())
            };
            match picture {
                Ok(Some(Picture::Raster(image))) => {
                    img_dynamic(move || image.clone())
                        .style(move |s| {
                            let size = shown();
                            s.width(size.width).height(size.height)
                        })
                        .into_any()
                }
                Ok(Some(Picture::Svg(text))) => svg(move || text.clone())
                    .style(move |s| {
                        let size = shown();
                        s.width(size.width).height(size.height)
                    })
                    .into_any(),
                Ok(None) => empty().into_any(),
                Err(err) => text(i18n::tf("Could not open the image: {}", &[&err]))
                    .style(move |s| {
                        s.color(config.get().color(LapceColor::EDITOR_DIM))
                    })
                    .into_any(),
            }
        },
    );

    let footer = label(move || {
        loaded.with(|loaded| match loaded {
            Some(Ok(loaded)) => format!(
                "{}  ·  {} × {}  ·  {}",
                loaded.format,
                loaded.size.width.round(),
                loaded.size.height.round(),
                format_bytes(loaded.bytes)
            ),
            _ => String::new(),
        })
    })
    .style(move |s| {
        let config = config.get();
        s.padding_horiz(10.0)
            .padding_vert(4.0)
            .width_full()
            .color(config.color(LapceColor::EDITOR_DIM))
            .border_top(1.0)
            .border_color(config.color(LapceColor::LAPCE_BORDER))
    });

    stack((
        container(picture)
            .on_resize(move |rect| {
                // 周りに少し余白を残す
                let size = rect.size();
                available.set(Size::new(
                    (size.width - 32.0).max(0.0),
                    (size.height - 32.0).max(0.0),
                ));
            })
            .style(|s| {
                s.flex_grow(1.0f32)
                    .flex_basis(0.0)
                    .min_height(0.0)
                    .width_full()
                    .items_center()
                    .justify_center()
            }),
        footer,
    ))
    .style(move |s| {
        s.size_full()
            .flex_col()
            .background(config.get().color(LapceColor::EDITOR_BACKGROUND))
    })
    .debug_name("Image Preview")
}

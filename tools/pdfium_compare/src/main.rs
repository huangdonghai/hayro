use hayro::hayro_interpret::font::{FontData, FontQuery, StandardFont};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_interpret::hayro_cmap::CidFamily;
use hayro::vello_cpu::Pixels;
use hayro::vello_cpu::peniko::ImageAlphaType;
use hayro::{RenderSettings, PixmapSettings};
use hayro_syntax::Pdf;
use image::buffer::ConvertBuffer;
use image::{RgbImage, RgbaImage};
use rayon::prelude::*;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

fn read_font(path: &Path) -> Option<FontData> {
    Some(Arc::new(fs::read(path).ok()?))
}

fn assets_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/assets")
}

fn get_standard(font: &StandardFont) -> Option<FontData> {
    let assets = assets_dir();
    let name = match font {
        StandardFont::Helvetica => "LiberationSans-Regular.ttf",
        StandardFont::HelveticaBold => "LiberationSans-Bold.ttf",
        StandardFont::HelveticaOblique => "LiberationSans-Italic.ttf",
        StandardFont::HelveticaBoldOblique => "LiberationSans-BoldItalic.ttf",
        StandardFont::Courier => "LiberationMono-Regular.ttf",
        StandardFont::CourierBold => "LiberationMono-Bold.ttf",
        StandardFont::CourierOblique => "LiberationMono-Italic.ttf",
        StandardFont::CourierBoldOblique => "LiberationMono-BoldItalic.ttf",
        StandardFont::TimesRoman => "LiberationSerif-Regular.ttf",
        StandardFont::TimesBold => "LiberationSerif-Bold.ttf",
        StandardFont::TimesItalic => "LiberationSerif-Italic.ttf",
        StandardFont::TimesBoldItalic => "LiberationSerif-BoldItalic.ttf",
        StandardFont::ZapfDingBats => return read_font(&assets.join("../hayro-font/assets/FoxitDingbats.pfb")),
        StandardFont::Symbol => return read_font(&assets.join("../hayro-font/assets/FoxitSymbol.pfb")),
    };
    read_font(&assets.join(name))
}

fn get_noto_fallback(query: &hayro::hayro_interpret::font::FallbackFontQuery) -> Option<FontData> {
    let family = &query.character_collection.as_ref()?.family;
    let name = match family {
        CidFamily::AdobeGB1 | CidFamily::AdobeCNS1 => {
            if query.is_bold { "NotoSansCJKsc-Bold.otf" } else { "NotoSansCJKsc-Regular.otf" }
        }
        CidFamily::AdobeJapan1 => {
            if query.is_bold { "NotoSansCJKjp-Bold.otf" } else { "NotoSansCJKjp-Regular.otf" }
        }
        CidFamily::AdobeKorea1 => {
            if query.is_bold { "NotoSansCJKkr-Bold.otf" } else { "NotoSansCJKkr-Regular.otf" }
        }
        _ => return None,
    };
    read_font(&assets_dir().join(name))
}

fn interpreter_settings() -> InterpreterSettings {
    InterpreterSettings {
        font_resolver: Arc::new(|query| match query {
            FontQuery::Standard(font) => Some((get_standard(&font)?, 0)),
            FontQuery::Fallback(query) => {
                if let Some(data) = get_noto_fallback(&query) {
                    return Some((data, 0));
                }
                Some((get_standard(&query.pick_standard_font())?, 0))
            }
        }),
        ..Default::default()
    }
}

fn stable_id(path: &Path, input: &Path) -> String {
    let relative = path.strip_prefix(input).unwrap_or(path);
    relative
        .with_extension("")
        .to_string_lossy()
        .replace(['\\', '/'], "__")
}

fn render_file(path: PathBuf, input: PathBuf, output: PathBuf) -> Result<usize, String> {
    let id = stable_id(&path, &input);
    let data = fs::read(&path).map_err(|err| format!("read: {err}"))?;
    let pdf = Pdf::new(data).map_err(|err| format!("load: {err:?}"))?;
    let cache = hayro::RenderCache::new();
    let settings = interpreter_settings();
    let render_settings = RenderSettings::default();

    let mut count = 0;
    for (index, page) in pdf.pages().iter().enumerate() {
        let (width_pt, height_pt) = page.render_dimensions();
        let width = width_pt.ceil() as u32;
        let height = height_pt.ceil() as u32;
        let pixmap = hayro::render(
            page,
            &cache,
            &settings,
            &render_settings,
            &PixmapSettings {
                x_scale: width as f32 / width_pt,
                y_scale: height as f32 / height_pt,
                bg_color: hayro::vello_cpu::peniko::color::AlphaColor::from_rgba8(255, 255, 255, 255),
            },
        );
        let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        let image = match pixmap.try_take_rgb8(ImageAlphaType::Alpha) {
            Pixels::Rgb8(data) => RgbImage::from_raw(width, height, data)
                .ok_or("bad rgb data")?
                .convert(),
            Pixels::Rgba8(data) => RgbaImage::from_raw(width, height, data).ok_or("bad rgba data")?,
        };
        image
            .save(output.join(format!("{id}_p{index}.png")))
            .map_err(|err| format!("save: {err}"))?;
        count += 1;
    }
    Ok(count)
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <input-folder> <output-folder>", args[0]);
        std::process::exit(2);
    }
    let input = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);
    fs::create_dir_all(&output).unwrap();

    let paths: Vec<PathBuf> = WalkDir::new(&input)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path().to_path_buf())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
        })
        .collect();
    eprintln!("Found {} PDF files", paths.len());

    let failures = std::sync::Mutex::new(Vec::new());
    let pages = std::sync::atomic::AtomicUsize::new(0);
    paths.par_iter().for_each(|path| {
        match render_file(path.clone(), input.clone(), output.clone()) {
            Ok(count) => {
                pages.fetch_add(count, std::sync::atomic::Ordering::Relaxed);
            }
            Err(err) => failures
                .lock()
                .unwrap()
                .push((path.display().to_string(), err)),
        }
    });
    let failures = failures.into_inner().unwrap();
    for (path, err) in &failures {
        eprintln!("FAILED {path}: {err}");
    }
    eprintln!(
        "Rendered {} pages from {} files; {} failures",
        pages.load(std::sync::atomic::Ordering::Relaxed),
        paths.len(),
        failures.len()
    );
    if !failures.is_empty() {
        std::process::exit(1);
    }
}

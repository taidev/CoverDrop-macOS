use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::{codecs::jpeg::JpegEncoder, DynamicImage, ImageEncoder, ImageFormat};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};
use uuid::Uuid;
use walkdir::WalkDir;

const DEFAULT_PPI: f64 = 150.0;
const MM_PPI: f64 = 72.0;
const MAX_OUTPUT_DIMENSION: u32 = 10_000;
const MAX_OUTPUT_PIXELS: u64 = 40_000_000;
const MAX_RENDER_SECONDS: u64 = 90;
const MAX_RENDER_SIDE: u32 = 8_000;
const MAX_BATCH_FILES: usize = 10_000;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
struct Settings {
    format: String,
    unit: String,
    width: Option<f64>,
    height: Option<f64>,
    ppi: u32,
    resize_mode: String,
    crop_anchor: String,
    quality: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            format: "jpeg".into(),
            unit: "px".into(),
            width: None,
            height: None,
            ppi: 150,
            resize_mode: "contain".into(),
            crop_anchor: "center".into(),
            quality: "high".into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    source_mode: String,
    files: Vec<String>,
    folder: Option<String>,
    output_dir: String,
    settings: Settings,
}

#[derive(Serialize)]
struct ItemResult {
    source: String,
    output: Option<String>,
    error: Option<String>,
}
#[derive(Serialize)]
struct Summary {
    completed: Vec<ItemResult>,
    failed: Vec<ItemResult>,
}
#[derive(Clone, Serialize)]
struct Progress {
    current: usize,
    total: usize,
    file: String,
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}
#[tauri::command]
fn get_settings(app: AppHandle) -> Result<Settings, String> {
    let path = settings_path(&app)?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    fs::write(
        settings_path(&app)?,
        serde_json::to_vec_pretty(&settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|x| x.to_str())
        .map(|x| x.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}
fn validate_pdf_file(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err("The selected PDF no longer exists.".into());
    }
    if !is_pdf(path) {
        return Err("Only PDF files can be processed.".into());
    }
    Ok(())
}
fn gather_files(job: &Job) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    if !matches!(job.source_mode.as_str(), "files" | "folder") {
        return Err("Unsupported source mode.".into());
    }
    if job.source_mode == "files" {
        if job.files.len() > MAX_BATCH_FILES {
            return Err(
                "This batch contains more than 10,000 PDFs. Split it into smaller batches.".into(),
            );
        }
        return Ok(job
            .files
            .iter()
            .map(|f| {
                let p = PathBuf::from(f);
                (p, PathBuf::new())
            })
            .collect());
    }
    let root = PathBuf::from(job.folder.as_ref().ok_or("No source folder selected")?);
    if !root.is_dir() {
        return Err("The selected source folder no longer exists.".into());
    }
    Ok(WalkDir::new(&root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && is_pdf(e.path()))
        .map(|e| {
            let source = e.into_path();
            let relative = source
                .parent()
                .and_then(|p| p.strip_prefix(&root).ok())
                .unwrap_or(Path::new(""))
                .to_path_buf();
            (source, relative)
        })
        .take(MAX_BATCH_FILES + 1)
        .collect())
}
#[tauri::command]
fn list_pdfs_in_folder(folder: String) -> Result<Vec<String>, String> {
    let root = PathBuf::from(folder);
    if !root.is_dir() {
        return Err("The selected source folder no longer exists.".into());
    }
    Ok(WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file() && is_pdf(entry.path()))
        .map(|entry| entry.into_path().display().to_string())
        .take(MAX_BATCH_FILES + 1)
        .collect())
}
fn output_extension(format: &str) -> Result<&'static str, String> {
    match format {
        "jpeg" | "jpg" => Ok("jpg"),
        "png" => Ok("png"),
        "webp" => Ok("webp"),
        "gif" => Ok("gif"),
        _ => Err("Unsupported image format".into()),
    }
}
fn unique_output(dir: &Path, stem: &str, extension: &str) -> PathBuf {
    let mut n = 1;
    loop {
        let suffix = if n == 1 {
            "".to_string()
        } else {
            format!("-{n}")
        };
        let candidate = dir.join(format!("{stem}{suffix}.{extension}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}
fn renderer(app: &AppHandle) -> Result<PathBuf, String> {
    if cfg!(debug_assertions) {
        if let Ok(value) = std::env::var("PDFTOPPM_PATH") {
            return Ok(PathBuf::from(value));
        }
    }
    let name = if cfg!(target_os = "windows") {
        "pdftoppm.exe"
    } else {
        "pdftoppm"
    };
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    if let Ok(resource_dir) = app.path().resource_dir() {
        let platform_dir = resource_dir.join("binaries").join(platform);
        let bundled = if cfg!(target_os = "windows") {
            platform_dir.join("bin").join(name)
        } else {
            platform_dir.join(name)
        };
        if bundled.exists() {
            return Ok(bundled);
        }
    }
    if cfg!(debug_assertions) {
        return Ok(PathBuf::from(name));
    }
    Err(
        "CoverDrop's PDF renderer is missing from this installation. Please reinstall the app."
            .into(),
    )
}
struct RenderedPage {
    path: PathBuf,
    temp_dir: PathBuf,
}
impl Drop for RenderedPage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}
fn render_first_page(app: &AppHandle, source: &Path, ppi: f64) -> Result<RenderedPage, String> {
    let temp = std::env::temp_dir().join(format!("coverdrop-{}", Uuid::new_v4()));
    fs::create_dir_all(&temp).map_err(|e| e.to_string())?;
    let prefix = temp.join("page");
    let ppi = ppi.round().clamp(72.0, 300.0).to_string();
    let max_render_side = MAX_RENDER_SIDE.to_string();
    let mut child = Command::new(renderer(app)?)
        .args([
            "-f",
            "1",
            "-l",
            "1",
            "-singlefile",
            "-png",
            "-r",
            &ppi,
            "-scale-to",
            &max_render_side,
        ])
        .arg(source)
        .arg(&prefix)
        .spawn()
        .map_err(|e| format!("Could not start the PDF renderer: {e}"))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|e| format!("Could not monitor the PDF renderer: {e}"))?
        {
            break status;
        }
        if started.elapsed() >= Duration::from_secs(MAX_RENDER_SECONDS) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("The PDF renderer timed out after 90 seconds.".into());
        }
        thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        return Err("The PDF could not be rendered. It may be encrypted or damaged.".into());
    }
    let png = temp.join("page.png");
    if png.exists() {
        Ok(RenderedPage {
            path: png,
            temp_dir: temp,
        })
    } else {
        Err("The PDF has no renderable first page.".into())
    }
}
#[tauri::command]
fn get_pdf_preview(app: AppHandle, path: String) -> Result<String, String> {
    let source = Path::new(&path);
    validate_pdf_file(source)?;
    let rendered = render_first_page(&app, source, 100.0)?;
    let bytes = fs::read(&rendered.path).map_err(|e| e.to_string())?;
    Ok(format!("data:image/png;base64,{}", BASE64.encode(bytes)))
}
fn converted_dimension(value: Option<f64>, unit: &str) -> Option<u32> {
    value
        .map(|number| match unit {
            "in" => number * MM_PPI,
            "cm" => (number / 2.54) * MM_PPI,
            "mm" => (number / 25.4) * MM_PPI,
            "pt" => number,
            _ => number,
        })
        .filter(|number| number.is_finite() && *number > 0.0)
        .map(|number| number.round().clamp(1.0, MAX_OUTPUT_DIMENSION as f64) as u32)
}
fn validate_settings(settings: &Settings) -> Result<(), String> {
    output_extension(&settings.format)?;
    if !matches!(settings.unit.as_str(), "px" | "in" | "cm" | "mm" | "pt") {
        return Err("Unsupported size unit.".into());
    }
    let (width, height) = (
        converted_dimension(settings.width, &settings.unit),
        converted_dimension(settings.height, &settings.unit),
    );
    if width
        .zip(height)
        .is_some_and(|(w, h)| (w as u64) * (h as u64) > MAX_OUTPUT_PIXELS)
    {
        return Err(
            "The requested image is too large. Use dimensions below 40 million pixels.".into(),
        );
    }
    Ok(())
}
fn anchored_offset(extra: u32, anchor: &str, start: &str, end: &str) -> u32 {
    if anchor.contains(start) {
        0
    } else if anchor.contains(end) {
        extra
    } else {
        extra / 2
    }
}
fn crop_to_size(image: DynamicImage, width: u32, height: u32, anchor: &str) -> DynamicImage {
    let scale = (width as f64 / image.width() as f64).max(height as f64 / image.height() as f64);
    let scaled_width = ((image.width() as f64 * scale).ceil() as u32).max(width);
    let scaled_height = ((image.height() as f64 * scale).ceil() as u32).max(height);
    let resized = image.resize_exact(
        scaled_width,
        scaled_height,
        image::imageops::FilterType::Lanczos3,
    );
    let x = anchored_offset(scaled_width - width, anchor, "left", "right");
    let y = anchored_offset(scaled_height - height, anchor, "top", "bottom");
    DynamicImage::ImageRgba8(image::imageops::crop_imm(&resized, x, y, width, height).to_image())
}
fn resize_for_settings(image: DynamicImage, settings: &Settings) -> DynamicImage {
    let (w, h) = (
        converted_dimension(settings.width, &settings.unit),
        converted_dimension(settings.height, &settings.unit),
    );
    match settings.resize_mode.as_str() {
        "crop" if w.is_some() && h.is_some() => {
            crop_to_size(image, w.unwrap(), h.unwrap(), &settings.crop_anchor)
        }
        "width" if w.is_some() => {
            let width = w.unwrap();
            let height =
                ((image.height() as f64 / image.width() as f64) * width as f64).round() as u32;
            image.resize_exact(width, height.max(1), image::imageops::FilterType::Lanczos3)
        }
        "height" if h.is_some() => {
            let height = h.unwrap();
            let width =
                ((image.width() as f64 / image.height() as f64) * height as f64).round() as u32;
            image.resize_exact(width.max(1), height, image::imageops::FilterType::Lanczos3)
        }
        _ => match (w, h) {
            (Some(width), Some(height)) => image.thumbnail(width, height),
            (Some(width), None) => {
                let height =
                    ((image.height() as f64 / image.width() as f64) * width as f64).round() as u32;
                image.resize_exact(width, height.max(1), image::imageops::FilterType::Lanczos3)
            }
            (None, Some(height)) => {
                let width =
                    ((image.width() as f64 / image.height() as f64) * height as f64).round() as u32;
                image.resize_exact(width.max(1), height, image::imageops::FilterType::Lanczos3)
            }
            _ => image,
        },
    }
}
fn quality_value(quality: &str) -> u8 {
    match quality {
        "low" => 60,
        "medium" => 75,
        "very-high" => 92,
        "maximum" => 100,
        _ => 85,
    }
}
fn write_image(
    image: DynamicImage,
    output: &Path,
    format: &str,
    quality: &str,
) -> Result<(), String> {
    if matches!(format, "jpeg" | "jpg") {
        let mut bytes = Cursor::new(Vec::new());
        JpegEncoder::new_with_quality(&mut bytes, quality_value(quality))
            .write_image(
                image.as_bytes(),
                image.width(),
                image.height(),
                image.color().into(),
            )
            .map_err(|e| format!("Could not encode JPEG: {e}"))?;
        return fs::write(output, bytes.into_inner()).map_err(|e| e.to_string());
    }
    if format == "webp" {
        let rgba = image.to_rgba8();
        let encoded = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height())
            .encode(quality_value(quality) as f32);
        return fs::write(output, &encoded[..]).map_err(|e| e.to_string());
    }
    let image_format = match format {
        "jpeg" | "jpg" => ImageFormat::Jpeg,
        "png" => ImageFormat::Png,
        "webp" => ImageFormat::WebP,
        "gif" => ImageFormat::Gif,
        _ => return Err("Unsupported image format".into()),
    };
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image_format)
        .map_err(|e| format!("Could not encode image: {e}"))?;
    fs::write(output, bytes.into_inner()).map_err(|e| e.to_string())
}
#[tauri::command]
fn generate_covers(app: AppHandle, job: Job) -> Result<Summary, String> {
    validate_settings(&job.settings)?;
    let files = gather_files(&job)?;
    if files.is_empty() {
        return Err("No PDF files were found.".into());
    }
    if files.len() > MAX_BATCH_FILES {
        return Err(
            "This batch contains more than 10,000 PDFs. Split it into smaller batches.".into(),
        );
    }
    let output_root = PathBuf::from(&job.output_dir);
    fs::create_dir_all(&output_root).map_err(|e| e.to_string())?;
    let total = files.len();
    let mut completed = Vec::new();
    let mut failed = Vec::new();
    let extension = output_extension(&job.settings.format)?;
    for (index, (source, relative_dir)) in files.into_iter().enumerate() {
        let filename = source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("PDF")
            .to_string();
        let _ = app.emit(
            "conversion-progress",
            Progress {
                current: index + 1,
                total,
                file: filename,
            },
        );
        let result = (|| -> Result<PathBuf, String> {
            validate_pdf_file(&source)?;
            let target_dir = output_root.join(relative_dir);
            fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;
            let rendered = render_first_page(
                &app,
                &source,
                if job.settings.width.is_some() || job.settings.height.is_some() {
                    300.0
                } else {
                    DEFAULT_PPI
                },
            );
            let rendered = rendered?;
            let image = image::open(&rendered.path)
                .map_err(|e| format!("Could not read rendered page: {e}"))?;
            let resized = resize_for_settings(image, &job.settings);
            let stem = source
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("cover");
            let destination = unique_output(&target_dir, stem, extension);
            write_image(
                resized,
                &destination,
                &job.settings.format,
                &job.settings.quality,
            )?;
            Ok(destination)
        })();
        match result {
            Ok(output) => completed.push(ItemResult {
                source: source.display().to_string(),
                output: Some(output.display().to_string()),
                error: None,
            }),
            Err(error) => failed.push(ItemResult {
                source: source.display().to_string(),
                output: None,
                error: Some(error),
            }),
        }
    }
    Ok(Summary { completed, failed })
}
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            list_pdfs_in_folder,
            get_pdf_preview,
            generate_covers
        ])
        .run(tauri::generate_context!())
        .expect("error while running CoverDrop");
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;
    #[test]
    fn output_extensions_are_normalized() {
        assert_eq!(output_extension("jpeg").unwrap(), "jpg");
        assert_eq!(output_extension("webp").unwrap(), "webp");
    }
    #[test]
    fn physical_units_use_72_ppi() {
        assert_eq!(converted_dimension(Some(1.0), "in"), Some(72));
        assert_eq!(converted_dimension(Some(25.0), "mm"), Some(71));
        assert_eq!(converted_dimension(Some(3.0), "cm"), Some(85));
        assert_eq!(converted_dimension(Some(72.0), "pt"), Some(72));
        assert_eq!(converted_dimension(Some(8.5), "in"), Some(612));
    }
    #[test]
    fn mm_is_converted_and_fitted() {
        let settings = Settings {
            unit: "mm".into(),
            width: Some(25.0),
            height: None,
            ..Settings::default()
        };
        let result = resize_for_settings(DynamicImage::new_rgb8(1000, 500), &settings);
        assert_eq!(result.dimensions(), (71, 36));
    }
    #[test]
    fn crop_fills_the_requested_size() {
        let settings = Settings {
            width: Some(200.0),
            height: Some(200.0),
            resize_mode: "crop".into(),
            crop_anchor: "top".into(),
            ..Settings::default()
        };
        let result = resize_for_settings(DynamicImage::new_rgb8(800, 400), &settings);
        assert_eq!(result.dimensions(), (200, 200));
    }
    #[test]
    fn duplicate_names_increment() {
        let dir = std::env::temp_dir().join(Uuid::new_v4().to_string());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("book.jpg"), []).unwrap();
        assert_eq!(
            unique_output(&dir, "book", "jpg").file_name().unwrap(),
            "book-2.jpg"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}

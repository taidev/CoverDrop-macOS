use image::{codecs::jpeg::JpegEncoder, DynamicImage, ImageEncoder, ImageFormat};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;
use walkdir::WalkDir;

const DEFAULT_PPI: f64 = 150.0;
const MM_PPI: f64 = 72.0;
const MAX_OUTPUT_DIMENSION: u32 = 10_000;
const MAX_OUTPUT_PIXELS: u64 = 40_000_000;
const MAX_RENDER_SECONDS: u64 = 90;
const MAX_RENDER_SIDE: u32 = 8_000;
const PREVIEW_RENDER_SIDE: u32 = 1_400;
const PREVIEW_JPEG_QUALITY: u8 = 82;
const PREVIEW_CACHE_VERSION: &str = "v1";
const PREVIEW_CACHE_MAX_FILES: usize = 256;
const PREVIEW_CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const MAX_BATCH_FILES: usize = 10_000;
const MAX_EXPORT_WORKERS: usize = 2;
const MIN_CUSTOM_RENDER_SIDE: u32 = 256;

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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewResponse {
    path: String,
    cache_hit: bool,
}

#[derive(Clone)]
struct PreviewJob {
    id: u64,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct PreviewState {
    active: Mutex<Option<PreviewJob>>,
    cache_pruned: AtomicBool,
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
fn reserve_output(dir: &Path, stem: &str, extension: &str) -> Result<PathBuf, String> {
    let mut n = 1;
    loop {
        let suffix = if n == 1 {
            "".to_string()
        } else {
            format!("-{n}")
        };
        let candidate = dir.join(format!("{stem}{suffix}.{extension}"));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
            Err(error) => return Err(format!("Could not reserve the output file: {error}")),
        }
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
#[derive(Clone, Copy)]
enum RenderFormat {
    Png,
    Jpeg(u8),
}

fn render_first_page(
    app: &AppHandle,
    source: &Path,
    ppi: f64,
    render_side: Option<u32>,
    format: RenderFormat,
    cancelled: Option<&AtomicBool>,
) -> Result<RenderedPage, String> {
    let temp = std::env::temp_dir().join(format!("coverdrop-{}", Uuid::new_v4()));
    fs::create_dir_all(&temp).map_err(|e| e.to_string())?;
    let prefix = temp.join("page");
    let ppi = ppi.round().clamp(72.0, 300.0).to_string();
    let mut command = Command::new(renderer(app)?);
    command.args(["-f", "1", "-l", "1", "-singlefile"]);
    match format {
        RenderFormat::Png => {
            command.arg("-png");
        }
        RenderFormat::Jpeg(quality) => {
            command.args([
                "-jpeg",
                "-jpegopt",
                &format!("quality={quality},optimize=y"),
            ]);
        }
    }
    let render_side = render_side.map(|value| value.to_string());
    let result = (|| -> Result<RenderedPage, String> {
        command.args(["-r", &ppi]);
        if let Some(render_side) = render_side.as_deref() {
            command.args(["-scale-to", render_side]);
        }
        let mut child = command
            .arg(source)
            .arg(&prefix)
            .spawn()
            .map_err(|e| format!("Could not start the PDF renderer: {e}"))?;
        let started = Instant::now();
        let status = loop {
            if cancelled.is_some_and(|token| token.load(Ordering::Relaxed)) {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Preview cancelled.".into());
            }
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
            thread::sleep(Duration::from_millis(25));
        };
        if !status.success() {
            return Err("The PDF could not be rendered. It may be encrypted or damaged.".into());
        }
        let rendered = temp.join(match format {
            RenderFormat::Png => "page.png",
            RenderFormat::Jpeg(_) => "page.jpg",
        });
        if rendered.exists() {
            Ok(RenderedPage {
                path: rendered,
                temp_dir: temp.clone(),
            })
        } else {
            Err("The PDF has no renderable first page.".into())
        }
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temp);
    }
    result
}

fn preview_cache_path(app: &AppHandle, source: &Path) -> Result<PathBuf, String> {
    let canonical = source.canonicalize().map_err(|e| e.to_string())?;
    let metadata = canonical.metadata().map_err(|e| e.to_string())?;
    let modified = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(PREVIEW_CACHE_VERSION.as_bytes());
    hasher.update(canonical.to_string_lossy().as_bytes());
    hasher.update(metadata.len().to_le_bytes());
    hasher.update(modified.as_nanos().to_le_bytes());
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("previews");
    fs::create_dir_all(&cache_dir).map_err(|e| e.to_string())?;
    Ok(cache_dir.join(format!("{:x}.jpg", hasher.finalize())))
}

fn prune_preview_cache(app: &AppHandle) {
    let Ok(cache_dir) = app.path().app_cache_dir().map(|path| path.join("previews")) else {
        return;
    };
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return;
    };
    let now = SystemTime::now();
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if now.duration_since(modified).unwrap_or_default() > PREVIEW_CACHE_MAX_AGE {
                let _ = fs::remove_file(entry.path());
                return None;
            }
            Some((modified, entry.path()))
        })
        .collect();
    files.sort_by_key(|(modified, _)| *modified);
    let remove_count = files.len().saturating_sub(PREVIEW_CACHE_MAX_FILES);
    for (_, path) in files.into_iter().take(remove_count) {
        let _ = fs::remove_file(path);
    }
}

fn begin_preview_job(
    state: &PreviewState,
    request_id: u64,
    prefetch: bool,
) -> Result<Arc<AtomicBool>, String> {
    let mut active = state
        .active
        .lock()
        .map_err(|_| "Preview state is unavailable.")?;
    if prefetch && active.is_some() {
        return Err("Preview busy.".into());
    }
    if let Some(job) = active.take() {
        job.cancelled.store(true, Ordering::Relaxed);
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    *active = Some(PreviewJob {
        id: request_id,
        cancelled: cancelled.clone(),
    });
    Ok(cancelled)
}

fn finish_preview_job(state: &PreviewState, request_id: u64) {
    if let Ok(mut active) = state.active.lock() {
        if active.as_ref().is_some_and(|job| job.id == request_id) {
            *active = None;
        }
    }
}

#[tauri::command]
async fn get_pdf_preview(
    app: AppHandle,
    state: State<'_, PreviewState>,
    path: String,
    request_id: u64,
    prefetch: bool,
) -> Result<PreviewResponse, String> {
    let source = PathBuf::from(path);
    validate_pdf_file(&source)?;
    if !prefetch {
        if let Ok(mut active) = state.active.lock() {
            if let Some(job) = active.take() {
                job.cancelled.store(true, Ordering::Relaxed);
            }
        }
    }
    if !state.cache_pruned.swap(true, Ordering::Relaxed) {
        prune_preview_cache(&app);
    }
    let cache_path = preview_cache_path(&app, &source)?;
    if cache_path.is_file() {
        return Ok(PreviewResponse {
            path: cache_path.display().to_string(),
            cache_hit: true,
        });
    }
    let cancelled = begin_preview_job(&state, request_id, prefetch)?;
    let app_for_render = app.clone();
    let cache_for_render = cache_path.clone();
    let task = tauri::async_runtime::spawn_blocking(move || {
        let rendered = render_first_page(
            &app_for_render,
            &source,
            72.0,
            Some(PREVIEW_RENDER_SIDE),
            RenderFormat::Jpeg(PREVIEW_JPEG_QUALITY),
            Some(&cancelled),
        )?;
        if cancelled.load(Ordering::Relaxed) {
            return Err("Preview cancelled.".into());
        }
        if !cache_for_render.is_file() {
            fs::copy(&rendered.path, &cache_for_render).map_err(|e| e.to_string())?;
        }
        Ok(PreviewResponse {
            path: cache_for_render.display().to_string(),
            cache_hit: false,
        })
    })
    .await;
    finish_preview_job(&state, request_id);
    task.map_err(|e| format!("Preview task failed: {e}"))?
}

#[tauri::command]
fn cancel_pdf_preview(state: State<'_, PreviewState>, request_id: u64) {
    if let Ok(active) = state.active.lock() {
        if let Some(job) = active.as_ref().filter(|job| job.id == request_id) {
            job.cancelled.store(true, Ordering::Relaxed);
        }
    }
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

fn custom_render_side(settings: &Settings) -> Option<u32> {
    let (width, height) = (
        converted_dimension(settings.width, &settings.unit),
        converted_dimension(settings.height, &settings.unit),
    );
    let requested = width.into_iter().chain(height).max()?;
    let multiplier = if settings.resize_mode == "crop" && width.is_some() && height.is_some() {
        3.0
    } else if width.is_some() && height.is_some() {
        1.25
    } else {
        2.5
    };
    Some(
        ((requested as f64 * multiplier).ceil() as u32)
            .clamp(MIN_CUSTOM_RENDER_SIDE, MAX_RENDER_SIDE),
    )
}

fn direct_render_format(settings: &Settings) -> Option<RenderFormat> {
    if settings.width.is_some() || settings.height.is_some() {
        return None;
    }
    match settings.format.as_str() {
        "jpeg" | "jpg" => Some(RenderFormat::Jpeg(quality_value(&settings.quality))),
        "png" => Some(RenderFormat::Png),
        _ => None,
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

fn write_reserved_output(
    target_dir: &Path,
    stem: &str,
    extension: &str,
    write: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let destination = reserve_output(target_dir, stem, extension)?;
    if let Err(error) = write(&destination) {
        let _ = fs::remove_file(&destination);
        return Err(error);
    }
    Ok(destination)
}

fn generate_one_cover(
    app: &AppHandle,
    source: &Path,
    relative_dir: &Path,
    output_root: &Path,
    settings: &Settings,
    extension: &str,
) -> Result<PathBuf, String> {
    validate_pdf_file(source)?;
    let target_dir = output_root.join(relative_dir);
    fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;
    let direct_format = direct_render_format(settings);
    let render_side = custom_render_side(settings);
    let rendered = render_first_page(
        app,
        source,
        if render_side.is_none() {
            DEFAULT_PPI
        } else {
            72.0
        },
        render_side,
        direct_format.unwrap_or(RenderFormat::Png),
        None,
    )?;
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("cover");
    if direct_format.is_some() {
        return write_reserved_output(&target_dir, stem, extension, |destination| {
            fs::copy(&rendered.path, destination)
                .map(|_| ())
                .map_err(|e| format!("Could not save the cover image: {e}"))
        });
    }
    let image =
        image::open(&rendered.path).map_err(|e| format!("Could not read rendered page: {e}"))?;
    let resized = resize_for_settings(image, settings);
    write_reserved_output(&target_dir, stem, extension, |destination| {
        write_image(resized, destination, &settings.format, &settings.quality)
    })
}

fn generate_covers_blocking(app: AppHandle, job: Job) -> Result<Summary, String> {
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
    let extension = output_extension(&job.settings.format)?;
    let worker_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1)
        .min(MAX_EXPORT_WORKERS)
        .min(total)
        .max(1);
    let queue = Arc::new(Mutex::new(VecDeque::from_iter(
        files.into_iter().enumerate(),
    )));
    let (sender, receiver) = mpsc::channel();
    let mut results = Vec::with_capacity(total);
    thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = queue.clone();
            let sender = sender.clone();
            let app = app.clone();
            let output_root = &output_root;
            let settings = &job.settings;
            scope.spawn(move || loop {
                let task = queue.lock().ok().and_then(|mut queue| queue.pop_front());
                let Some((index, (source, relative_dir))) = task else {
                    break;
                };
                let result = generate_one_cover(
                    &app,
                    &source,
                    &relative_dir,
                    output_root,
                    settings,
                    extension,
                );
                if sender.send((index, source, result)).is_err() {
                    break;
                }
            });
        }
        drop(sender);
        for current in 1..=total {
            let Ok((index, source, result)) = receiver.recv() else {
                break;
            };
            let filename = source
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("PDF")
                .to_string();
            let _ = app.emit(
                "conversion-progress",
                Progress {
                    current,
                    total,
                    file: filename,
                },
            );
            let item = match result {
                Ok(output) => ItemResult {
                    source: source.display().to_string(),
                    output: Some(output.display().to_string()),
                    error: None,
                },
                Err(error) => ItemResult {
                    source: source.display().to_string(),
                    output: None,
                    error: Some(error),
                },
            };
            results.push((index, item));
        }
    });
    results.sort_by_key(|(index, _)| *index);
    let mut completed = Vec::new();
    let mut failed = Vec::new();
    for (_, result) in results {
        if result.error.is_some() {
            failed.push(result);
        } else {
            completed.push(result);
        }
    }
    Ok(Summary { completed, failed })
}

#[tauri::command]
async fn generate_covers(app: AppHandle, job: Job) -> Result<Summary, String> {
    tauri::async_runtime::spawn_blocking(move || generate_covers_blocking(app, job))
        .await
        .map_err(|error| format!("Cover generation task failed: {error}"))?
}
pub fn run() {
    tauri::Builder::default()
        .manage(PreviewState::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            list_pdfs_in_folder,
            get_pdf_preview,
            cancel_pdf_preview,
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
            reserve_output(&dir, "book", "jpg")
                .unwrap()
                .file_name()
                .unwrap(),
            "book-2.jpg"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn small_custom_outputs_use_a_small_render_budget() {
        let settings = Settings {
            width: Some(500.0),
            height: Some(700.0),
            ..Settings::default()
        };
        assert_eq!(custom_render_side(&settings), Some(875));
    }

    #[test]
    fn crop_outputs_keep_extra_pixels_for_quality() {
        let settings = Settings {
            width: Some(500.0),
            height: Some(500.0),
            resize_mode: "crop".into(),
            ..Settings::default()
        };
        assert_eq!(custom_render_side(&settings), Some(1_500));
    }

    #[test]
    fn unscaled_jpeg_and_png_can_skip_reencoding() {
        assert!(matches!(
            direct_render_format(&Settings::default()),
            Some(RenderFormat::Jpeg(85))
        ));
        let settings = Settings {
            format: "png".into(),
            ..Settings::default()
        };
        assert!(matches!(
            direct_render_format(&settings),
            Some(RenderFormat::Png)
        ));
    }
}

use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use gpui::RenderImage;
use image::{DynamicImage, ImageDecoder, Limits, codecs::bmp::BmpDecoder};
use thiserror::Error;

use crate::{
    domain::{
        capture::FormatKind,
        clipboard::{ImageDimensions, ImagePreviewSource},
    },
    services::storage::{RawBlob, RawBlobError, RawBlobStore},
};

#[derive(Clone, Debug)]
pub struct ImagePreviewService {
    blob_store: RawBlobStore,
}

#[derive(Debug, Error)]
pub enum ImagePreviewError {
    #[error("读取图片文件失败：{0}")]
    ReadFile(#[from] std::io::Error),
    #[error("读取图片 blob 失败：{0}")]
    Storage(#[from] RawBlobError),
    #[error("图片解码失败：{0}")]
    Decode(#[from] image::ImageError),
    #[cfg(target_os = "windows")]
    #[error("WIC 图片解码失败：{0}")]
    Wic(#[source] windows::core::Error),
    #[error("图片像素数据超过预览内存限制")]
    AllocationLimit,
    #[error("本地图片文件超过预览大小限制")]
    FileTooLarge,
    #[error("文件列表中没有可预览的图片")]
    NoImageFile,
    #[error("数据正在导入，请稍后重试")]
    DataSwitching,
}

const MAX_IMAGE_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FILE_FALLBACK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LOCAL_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, PartialEq)]
pub struct LoadedImagePreview {
    pub image: Arc<RenderImage>,
    pub dimensions: Option<ImageDimensions>,
    /// 图片原始数据大小；文件列表使用实际图片文件大小，不使用路径列表大小。
    pub byte_length: Option<u64>,
    pub source_path: Option<PathBuf>,
}

/// 只读取图片解码器的头部信息，不生成像素缓冲区。
pub fn probe_image_dimensions(bytes: &[u8], format_key: &str) -> Option<ImageDimensions> {
    let (width, height) = if is_dib_format(format_key) {
        let normalized_dib;
        let dib_bytes = if format_key.eq_ignore_ascii_case("CF_DIBV5") {
            normalized_dib = normalize_dib_v5(bytes);
            normalized_dib.as_deref().unwrap_or(bytes)
        } else {
            bytes
        };
        BmpDecoder::new_without_file_header(Cursor::new(dib_bytes))
            .ok()?
            .dimensions()
    } else {
        image::ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .into_decoder()
            .ok()?
            .dimensions()
    };

    (width > 0 && height > 0).then_some(ImageDimensions { width, height })
}

pub(crate) fn validate_remote_image(
    bytes: &[u8],
    format_key: &str,
) -> Result<(), ImagePreviewError> {
    decode_image_without_wic(bytes, format_key, (1, 1)).map(|_| ())
}

impl ImagePreviewService {
    pub fn new(blob_store: RawBlobStore) -> Self {
        Self { blob_store }
    }

    pub fn load(
        &self,
        source: &ImagePreviewSource,
        format_kind: FormatKind,
        format_key: &str,
        max_size: (u32, u32),
    ) -> Result<Arc<RenderImage>, ImagePreviewError> {
        let bytes = match format_kind {
            FormatKind::FileList => {
                return self.load_file_list(source, max_size);
            }
            _ => self.load_source_bytes(source)?,
        };
        decode_image(&bytes, format_key, max_size)
    }

    pub fn load_with_dimensions(
        &self,
        source: &ImagePreviewSource,
        format_kind: FormatKind,
        format_key: &str,
        max_size: (u32, u32),
    ) -> Result<LoadedImagePreview, ImagePreviewError> {
        self.load_with_dimensions_hint(source, format_kind, format_key, max_size, None)
    }

    pub fn load_with_dimensions_hint(
        &self,
        source: &ImagePreviewSource,
        format_kind: FormatKind,
        format_key: &str,
        max_size: (u32, u32),
        dimensions_hint: Option<ImageDimensions>,
    ) -> Result<LoadedImagePreview, ImagePreviewError> {
        if format_kind == FormatKind::FileList {
            return self.load_file_list_with_dimensions(source, max_size);
        }

        let bytes = self.load_source_bytes(source)?;
        let dimensions = dimensions_hint.or_else(|| probe_image_dimensions(&bytes, format_key));
        let image = decode_image(&bytes, format_key, max_size)?;
        Ok(LoadedImagePreview {
            image,
            dimensions,
            byte_length: Some(bytes.len() as u64),
            source_path: None,
        })
    }

    /// 读取剪贴板文件列表中的路径；真正的文件元数据和图标由列表视图按行高懒加载前几项。
    pub fn load_file_list_paths(
        &self,
        source: &ImagePreviewSource,
    ) -> Result<Vec<PathBuf>, ImagePreviewError> {
        let bytes = self.load_source_bytes(source)?;
        Ok(parse_file_list(&bytes))
    }

    /// 从 HTML 提供的本地路径加载图片，不访问网络或其它 URI 资源。
    pub fn load_local_path(
        &self,
        path: &Path,
        max_size: (u32, u32),
    ) -> Result<Arc<RenderImage>, ImagePreviewError> {
        let _operation = crate::services::data_runtime::begin_operation()
            .map_err(|_| ImagePreviewError::DataSwitching)?;
        let path = dunce::canonicalize(path)?;
        let metadata = fs::metadata(&path)?;
        if !metadata.is_file() {
            return Err(ImagePreviewError::NoImageFile);
        }
        if metadata.len() > MAX_LOCAL_IMAGE_BYTES {
            return Err(ImagePreviewError::FileTooLarge);
        }

        let mut bytes = fs::read(path)?;
        let truncated_jpeg = is_truncated_jpeg(&bytes);
        complete_truncated_jpeg(&mut bytes);

        if truncated_jpeg {
            return decode_image_without_wic(&bytes, "", max_size);
        }

        #[cfg(target_os = "windows")]
        match decode_bytes_with_wic(&bytes, max_size) {
            Ok(loaded) => return Ok(loaded.image),
            Err(error) => {
                tracing::debug!(%error, "WIC 按图片内容预览失败，尝试备用解码器");
            }
        }

        decode_image(&bytes, "", max_size)
    }

    /// 按需解码 GIF，每帧立即缩小，并在帧间检查取消状态。
    pub fn load_animated_gif(
        &self,
        path: &Path,
        max_size: (u32, u32),
        max_frame_bytes: usize,
        cancelled: &AtomicBool,
    ) -> Result<Option<Arc<RenderImage>>, ImagePreviewError> {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let _operation = crate::services::data_runtime::begin_operation()
            .map_err(|_| ImagePreviewError::DataSwitching)?;
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(ImagePreviewError::NoImageFile);
        }
        if metadata.len() > MAX_LOCAL_IMAGE_BYTES {
            return Err(ImagePreviewError::FileTooLarge);
        }
        let bytes = fs::read(path)?;
        decode_animated_gif(&bytes, max_size, max_frame_bytes, cancelled)
    }

    fn load_file_list(
        &self,
        source: &ImagePreviewSource,
        max_size: (u32, u32),
    ) -> Result<Arc<RenderImage>, ImagePreviewError> {
        self.load_file_list_with_dimensions(source, max_size)
            .map(|loaded| loaded.image)
    }

    fn load_file_list_with_dimensions(
        &self,
        source: &ImagePreviewSource,
        max_size: (u32, u32),
    ) -> Result<LoadedImagePreview, ImagePreviewError> {
        let bytes = self.load_source_bytes(source)?;
        let mut skipped_count = 0;
        for path in parse_file_list(&bytes) {
            if !path.is_file() {
                skipped_count += 1;
                tracing::debug!("文件列表图片文件不存在，已跳过");
                continue;
            }

            #[cfg(target_os = "windows")]
            match decode_file_with_wic(&path, max_size) {
                Ok(loaded) => return Ok(loaded),
                Err(error) => {
                    tracing::debug!(%error, "WIC 图片预览失败，尝试小文件回退");
                }
            }

            let file_size = match fs::metadata(&path) {
                Ok(metadata) => metadata.len(),
                Err(error) => {
                    skipped_count += 1;
                    tracing::debug!(%error, "读取文件列表图片元数据失败");
                    continue;
                }
            };
            if file_size > MAX_FILE_FALLBACK_BYTES {
                skipped_count += 1;
                tracing::debug!(file_size, "文件列表图片超过回退读取大小限制，已跳过");
                continue;
            }
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    skipped_count += 1;
                    tracing::debug!(%error, "读取文件列表图片失败");
                    continue;
                }
            };
            match decode_image(&bytes, "", max_size) {
                Ok(image) => {
                    return Ok(LoadedImagePreview {
                        image,
                        dimensions: probe_image_dimensions(&bytes, ""),
                        byte_length: Some(bytes.len() as u64),
                        source_path: Some(path),
                    });
                }
                Err(error) => {
                    skipped_count += 1;
                    tracing::debug!(%error, "解码文件列表图片失败");
                }
            }
        }
        if skipped_count > 0 {
            tracing::debug!(skipped_count, "文件列表图片预览跳过部分文件");
        }
        Err(ImagePreviewError::NoImageFile)
    }

    fn load_source_bytes(&self, source: &ImagePreviewSource) -> Result<Vec<u8>, ImagePreviewError> {
        match source {
            ImagePreviewSource::Inline { bytes } => Ok(bytes.to_vec()),
            ImagePreviewSource::External {
                sha256,
                byte_length,
                relative_path,
            } => Ok(self.blob_store.read_for_preview(&RawBlob::from_record(
                *sha256,
                *byte_length,
                relative_path,
            ))?),
        }
    }
}

fn decode_image(
    bytes: &[u8],
    format_key: &str,
    max_size: (u32, u32),
) -> Result<Arc<RenderImage>, ImagePreviewError> {
    #[cfg(target_os = "windows")]
    if !is_dib_format(format_key) {
        if let Ok(image) = decode_bytes_with_wic(bytes, max_size) {
            return Ok(image.image);
        }
    }

    #[cfg(target_os = "windows")]
    if is_dib_format(format_key)
        && let Some(bmp_bytes) = bmp_file_bytes_from_dib(bytes)
        && let Ok(image) = decode_bytes_with_wic(&bmp_bytes, max_size)
    {
        return Ok(image.image);
    }

    decode_image_without_wic(bytes, format_key, max_size)
}

fn decode_image_without_wic(
    bytes: &[u8],
    format_key: &str,
    max_size: (u32, u32),
) -> Result<Arc<RenderImage>, ImagePreviewError> {
    let image = if is_dib_format(format_key) {
        let normalized_dib;
        let dib_bytes = if format_key.eq_ignore_ascii_case("CF_DIBV5") {
            normalized_dib = normalize_dib_v5(bytes);
            normalized_dib.as_deref().unwrap_or(bytes)
        } else {
            bytes
        };
        let mut decoder = BmpDecoder::new_without_file_header(Cursor::new(dib_bytes))?;
        decoder.set_limits(image_decode_limits())?;
        if decoder.total_bytes() > MAX_IMAGE_DECODE_BYTES {
            return Err(ImagePreviewError::AllocationLimit);
        }
        DynamicImage::from_decoder(decoder)?
    } else {
        let mut reader = image::ImageReader::new(Cursor::new(bytes));
        reader = reader.with_guessed_format()?;
        reader.limits(image_decode_limits());
        reader.decode()?
    };
    let image = image.thumbnail(max_size.0.max(1), max_size.1.max(1));
    let mut rgba = image.into_rgba8();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(rgba)])))
}

fn image_decode_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_IMAGE_DECODE_BYTES);
    limits
}

fn decode_animated_gif(
    bytes: &[u8],
    max_size: (u32, u32),
    max_frame_bytes: usize,
    cancelled: &AtomicBool,
) -> Result<Option<Arc<RenderImage>>, ImagePreviewError> {
    use image::AnimationDecoder;

    if cancelled.load(Ordering::Relaxed) || !has_gif_signature(bytes) {
        return Ok(None);
    }
    let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))?;
    let mut limits = image_decode_limits();
    limits.max_alloc = Some(64 * 1024 * 1024);
    decoder.set_limits(limits)?;
    let mut frames = Vec::new();
    let mut frame_bytes = 0;
    let mut source_frames = decoder.into_frames();
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let Some(frame) = source_frames.next() else {
            break;
        };
        let frame = frame?;
        // 超限时保留静态缩略图，避免把截断动画当作完整循环播放。
        if frames.len() >= 600 {
            return Err(ImagePreviewError::AllocationLimit);
        }
        let delay =
            if std::time::Duration::from(frame.delay()) < std::time::Duration::from_millis(20) {
                image::Delay::from_numer_denom_ms(100, 1)
            } else {
                frame.delay()
            };
        let mut buffer = DynamicImage::ImageRgba8(frame.into_buffer())
            .thumbnail(max_size.0.max(1), max_size.1.max(1))
            .into_rgba8();
        frame_bytes += buffer.len();
        if frame_bytes > max_frame_bytes {
            return Err(ImagePreviewError::AllocationLimit);
        }
        for pixel in buffer.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        frames.push(image::Frame::from_parts(buffer, 0, 0, delay));
    }
    if frames.is_empty() {
        return Err(ImagePreviewError::NoImageFile);
    }
    Ok(Some(Arc::new(RenderImage::new(frames))))
}

fn has_gif_signature(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

fn is_truncated_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8]) && !bytes.ends_with(&[0xff, 0xd9])
}

fn complete_truncated_jpeg(bytes: &mut Vec<u8>) {
    if is_truncated_jpeg(bytes) {
        bytes.extend_from_slice(&[0xff, 0xd9]);
    }
}

#[cfg(target_os = "windows")]
fn decode_file_with_wic(
    path: &Path,
    max_size: (u32, u32),
) -> Result<LoadedImagePreview, ImagePreviewError> {
    use std::{iter::once, os::windows::ffi::OsStrExt};

    use windows::{
        Win32::{
            Foundation::GENERIC_READ,
            Graphics::Imaging::{
                CLSID_WICImagingFactory, IWICImagingFactory, WICDecodeMetadataCacheOnDemand,
            },
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
        },
        core::PCWSTR,
    };

    struct ComGuard;

    impl ComGuard {
        fn initialize() -> Result<Self, windows::core::Error> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
            Ok(Self)
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    let _com = ComGuard::initialize().map_err(ImagePreviewError::Wic)?;
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(ImagePreviewError::Wic)?;
    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
    let decoder = unsafe {
        factory.CreateDecoderFromFilename(
            PCWSTR::from_raw(wide_path.as_ptr()),
            None,
            GENERIC_READ,
            WICDecodeMetadataCacheOnDemand,
        )
    }
    .map_err(ImagePreviewError::Wic)?;
    let mut loaded = decode_wic_decoder(&factory, &decoder, max_size)?;
    loaded.byte_length = fs::metadata(path).ok().map(|metadata| metadata.len());
    loaded.source_path = Some(path.to_path_buf());
    Ok(loaded)
}

#[cfg(target_os = "windows")]
fn decode_bytes_with_wic(
    bytes: &[u8],
    max_size: (u32, u32),
) -> Result<LoadedImagePreview, ImagePreviewError> {
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, IWICImagingFactory, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize,
    };

    struct ComGuard;

    impl ComGuard {
        fn initialize() -> Result<Self, windows::core::Error> {
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
            Ok(Self)
        }
    }

    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    let _com = ComGuard::initialize().map_err(ImagePreviewError::Wic)?;
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(ImagePreviewError::Wic)?;
    let stream = unsafe { factory.CreateStream() }.map_err(ImagePreviewError::Wic)?;
    unsafe { stream.InitializeFromMemory(bytes) }.map_err(ImagePreviewError::Wic)?;
    let decoder = unsafe {
        factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
    }
    .map_err(ImagePreviewError::Wic)?;

    let mut loaded = decode_wic_decoder(&factory, &decoder, max_size)?;
    loaded.byte_length = Some(bytes.len() as u64);
    Ok(loaded)
}

#[cfg(target_os = "windows")]
fn decode_wic_decoder(
    factory: &windows::Win32::Graphics::Imaging::IWICImagingFactory,
    decoder: &windows::Win32::Graphics::Imaging::IWICBitmapDecoder,
    max_size: (u32, u32),
) -> Result<LoadedImagePreview, ImagePreviewError> {
    use windows::Win32::Graphics::Imaging::{
        GUID_WICPixelFormat32bppBGRA, WICBitmapInterpolationModeFant, WICConvertBitmapSource,
    };

    let frame = unsafe { decoder.GetFrame(0) }.map_err(ImagePreviewError::Wic)?;

    let mut width = 0;
    let mut height = 0;
    unsafe { frame.GetSize(&mut width, &mut height) }.map_err(ImagePreviewError::Wic)?;
    let (target_width, target_height) = thumbnail_dimensions(width, height, max_size);

    let scaler = unsafe { factory.CreateBitmapScaler() }.map_err(ImagePreviewError::Wic)?;
    unsafe {
        scaler.Initialize(
            &frame,
            target_width,
            target_height,
            WICBitmapInterpolationModeFant,
        )
    }
    .map_err(ImagePreviewError::Wic)?;
    let source = unsafe { WICConvertBitmapSource(&GUID_WICPixelFormat32bppBGRA, &scaler) }
        .map_err(ImagePreviewError::Wic)?;
    let stride = target_width
        .checked_mul(4)
        .ok_or(ImagePreviewError::AllocationLimit)?;
    let buffer_len = usize::try_from(stride)
        .ok()
        .and_then(|stride| stride.checked_mul(usize::try_from(target_height).ok()?))
        .ok_or(ImagePreviewError::AllocationLimit)?;
    let mut bgra = vec![0; buffer_len];
    unsafe { source.CopyPixels(std::ptr::null(), stride, &mut bgra) }
        .map_err(ImagePreviewError::Wic)?;
    // WIC 已输出 BGRA；RenderImage 也使用 BGRA，不能再交换红蓝通道。
    let bgra_image = image::RgbaImage::from_raw(target_width, target_height, bgra)
        .ok_or(ImagePreviewError::AllocationLimit)?;
    Ok(LoadedImagePreview {
        image: Arc::new(RenderImage::new(vec![image::Frame::new(bgra_image)])),
        dimensions: (width > 0 && height > 0).then_some(ImageDimensions { width, height }),
        byte_length: None,
        source_path: None,
    })
}

fn thumbnail_dimensions(width: u32, height: u32, max_size: (u32, u32)) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (1, 1);
    }
    let max_width = max_size.0.max(1);
    let max_height = max_size.1.max(1);
    let width_ratio = f64::from(max_width) / f64::from(width);
    let height_ratio = f64::from(max_height) / f64::from(height);
    let ratio = width_ratio.min(height_ratio).min(1.0);
    (
        (f64::from(width) * ratio).round().max(1.0) as u32,
        (f64::from(height) * ratio).round().max(1.0) as u32,
    )
}

fn is_dib_format(format_key: &str) -> bool {
    format_key.eq_ignore_ascii_case("CF_DIB") || format_key.eq_ignore_ascii_case("CF_DIBV5")
}

fn bmp_file_bytes_from_dib(bytes: &[u8]) -> Option<Vec<u8>> {
    let header_size =
        usize::try_from(u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?)).ok()?;
    if header_size < 12 || header_size > bytes.len() {
        return None;
    }

    let pixel_offset = if header_size == 12 {
        let bits_per_pixel = u16::from_le_bytes(bytes.get(10..12)?.try_into().ok()?);
        let palette_entries = if bits_per_pixel <= 8 {
            1_usize.checked_shl(u32::from(bits_per_pixel))?
        } else {
            0
        };
        header_size.checked_add(palette_entries.checked_mul(3)?)?
    } else {
        let bits_per_pixel = u16::from_le_bytes(bytes.get(14..16)?.try_into().ok()?);
        let compression = u32::from_le_bytes(bytes.get(16..20)?.try_into().ok()?);
        let colors_used = u32::from_le_bytes(bytes.get(32..36)?.try_into().ok()?);
        let palette_entries = if bits_per_pixel <= 8 {
            usize::try_from(if colors_used == 0 {
                1_u32.checked_shl(u32::from(bits_per_pixel))?
            } else {
                colors_used
            })
            .ok()?
        } else {
            0
        };
        let bitfield_bytes = if header_size == 40 {
            match compression {
                3 => 12,
                6 => 16,
                _ => 0,
            }
        } else {
            0
        };
        header_size
            .checked_add(bitfield_bytes)?
            .checked_add(palette_entries.checked_mul(4)?)?
    };
    if pixel_offset > bytes.len() {
        return None;
    }

    let file_size = 14_usize.checked_add(bytes.len())?;
    let file_pixel_offset = 14_usize.checked_add(pixel_offset)?;
    let file_size = u32::try_from(file_size).ok()?;
    let file_pixel_offset = u32::try_from(file_pixel_offset).ok()?;
    let mut result = Vec::with_capacity(usize::try_from(file_size).ok()?);
    result.extend_from_slice(b"BM");
    result.extend_from_slice(&file_size.to_le_bytes());
    result.extend_from_slice(&[0; 4]);
    result.extend_from_slice(&file_pixel_offset.to_le_bytes());
    result.extend_from_slice(bytes);
    Some(result)
}

fn normalize_dib_v5(bytes: &[u8]) -> Option<Vec<u8>> {
    const BITMAP_INFO_HEADER_SIZE: usize = 40;
    const BITMAP_V5_HEADER_SIZE: usize = 124;
    if bytes.len() < BITMAP_V5_HEADER_SIZE
        || u32::from_le_bytes(bytes[0..4].try_into().ok()?) != BITMAP_V5_HEADER_SIZE as u32
    {
        return None;
    }

    let compression = u32::from_le_bytes(bytes[16..20].try_into().ok()?);
    let mut normalized = Vec::with_capacity(
        BITMAP_INFO_HEADER_SIZE
            + if matches!(compression, 3 | 6) { 12 } else { 0 }
            + bytes.len().saturating_sub(BITMAP_V5_HEADER_SIZE),
    );
    normalized.extend_from_slice(&bytes[..BITMAP_INFO_HEADER_SIZE]);
    normalized[..4].copy_from_slice(&(BITMAP_INFO_HEADER_SIZE as u32).to_le_bytes());
    if matches!(compression, 3 | 6) {
        normalized[16..20].copy_from_slice(&3_u32.to_le_bytes());
        normalized.extend_from_slice(&bytes[40..52]);
    }
    normalized.extend_from_slice(&bytes[BITMAP_V5_HEADER_SIZE..]);
    Some(normalized)
}

pub(crate) fn parse_file_list(bytes: &[u8]) -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if bytes.len() >= 20 {
            let offset = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
            let wide = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) != 0;
            if offset < bytes.len() {
                let data = &bytes[offset..];
                if wide {
                    let units = data
                        .chunks_exact(2)
                        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
                    return split_null_terminated(units.map(|unit| unit as u32))
                        .into_iter()
                        .filter_map(|units| {
                            String::from_utf16(
                                &units
                                    .into_iter()
                                    .map(|unit| unit as u16)
                                    .collect::<Vec<_>>(),
                            )
                            .ok()
                        })
                        .map(PathBuf::from)
                        .collect();
                }
                return split_null_terminated(data.iter().map(|byte| *byte as u32))
                    .into_iter()
                    .filter_map(|bytes| {
                        String::from_utf8(bytes.into_iter().map(|byte| byte as u8).collect()).ok()
                    })
                    .map(PathBuf::from)
                    .collect();
            }
        }
    }

    split_null_terminated(bytes.iter().map(|byte| *byte as u32))
        .into_iter()
        .filter_map(|bytes| {
            String::from_utf8(bytes.into_iter().map(|byte| byte as u8).collect()).ok()
        })
        .map(PathBuf::from)
        .collect()
}

fn split_null_terminated<T>(values: impl IntoIterator<Item = T>) -> Vec<Vec<T>>
where
    T: Copy + Default + PartialEq,
{
    let mut result = Vec::new();
    let mut current = Vec::new();
    for value in values {
        if value == T::default() {
            if current.is_empty() {
                break;
            }
            result.push(std::mem::take(&mut current));
        } else {
            current.push(value);
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use std::os::windows::ffi::OsStrExt;

    use super::*;
    use image::{
        ExtendedColorType, ImageEncoder,
        codecs::{bmp::BmpEncoder, png::PngEncoder},
    };

    #[test]
    fn empty_file_list_produces_no_image_path() {
        assert!(parse_file_list(&[]).is_empty());
    }

    #[test]
    fn file_list_paths_are_split_on_null_delimiter() {
        let paths = parse_file_list(b"C:\\a.png\0C:\\b.txt\0\0");
        assert_eq!(
            paths,
            vec![PathBuf::from("C:\\a.png"), PathBuf::from("C:\\b.txt")]
        );
    }

    #[test]
    fn standard_image_bytes_decode_directly_to_render_image() {
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&[255, 0, 0, 255], 1, 1, ExtendedColorType::Rgba8)
            .expect("无法生成测试 PNG");

        let image = decode_image(&bytes, "registered:PNG", (16, 16)).expect("无法解码标准图片");
        assert_eq!(image.frame_count(), 1);
        assert!(
            image
                .as_bytes(0)
                .expect("缺少图片帧")
                .chunks_exact(4)
                .all(|pixel| pixel == [0, 0, 255, 255])
        );
    }

    #[test]
    fn retained_image_formats_decode_after_feature_trimming() {
        for format in [
            image::ImageFormat::Png,
            image::ImageFormat::Jpeg,
            image::ImageFormat::Gif,
            image::ImageFormat::WebP,
            image::ImageFormat::Bmp,
            image::ImageFormat::Tiff,
            image::ImageFormat::Ico,
        ] {
            let source = if format == image::ImageFormat::Jpeg {
                image::DynamicImage::new_rgb8(2, 1)
            } else {
                image::DynamicImage::new_rgba8(2, 1)
            };
            let mut bytes = Cursor::new(Vec::new());
            source
                .write_to(&mut bytes, format)
                .expect("无法生成测试图片");
            let name = format!("registered:{}", format.extensions_str()[0]);
            let decoded = decode_image_without_wic(bytes.get_ref(), &name, (2, 1))
                .expect("裁剪后无法解码保留的图片格式");
            assert_eq!(decoded.size(0).width.0, 2);
            assert_eq!(decoded.size(0).height.0, 1);
        }
    }

    #[test]
    fn probe_image_dimensions_reads_png_header_without_decoding_pixels() {
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&[255; 4 * 2 * 4], 4, 2, ExtendedColorType::Rgba8)
            .expect("无法生成测试 PNG");

        assert_eq!(
            probe_image_dimensions(&bytes, "registered:PNG"),
            Some(ImageDimensions {
                width: 4,
                height: 2,
            })
        );
    }

    #[test]
    fn local_image_path_decodes_a_real_file() {
        let directory = tempfile::tempdir().expect("无法创建本地图片测试目录");
        let path = directory.path().join("preview.png");
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(&[255, 0, 0, 255], 1, 1, ExtendedColorType::Rgba8)
            .expect("无法生成测试 PNG");
        fs::write(&path, bytes).expect("无法写入测试图片");

        let blob_store =
            crate::services::storage::RawBlobStore::new(directory.path().join("blobs"))
                .expect("无法创建测试 blob 存储");
        let service = ImagePreviewService::new(blob_store);
        let image = service
            .load_local_path(&path, (16, 16))
            .expect("无法从本地路径加载图片");
        assert_eq!(image.frame_count(), 1);
    }

    #[test]
    fn single_image_file_list_returns_original_dimensions() {
        let directory = tempfile::tempdir().expect("无法创建本地图片测试目录");
        let path = directory.path().join("preview.png");
        let mut image_bytes = Vec::new();
        PngEncoder::new(&mut image_bytes)
            .write_image(&[255; 4 * 4 * 2], 4, 2, ExtendedColorType::Rgba8)
            .expect("无法生成测试 PNG");
        fs::write(&path, &image_bytes).expect("无法写入测试图片");

        let blob_store =
            crate::services::storage::RawBlobStore::new(directory.path().join("blobs"))
                .expect("无法创建测试 blob 存储");
        let service = ImagePreviewService::new(blob_store);
        let source = ImagePreviewSource::Inline {
            bytes: file_list_bytes(&path).into(),
        };
        let loaded = service
            .load_with_dimensions(&source, FormatKind::FileList, "CF_HDROP", (2, 2))
            .expect("无法从单个图片文件加载预览");

        assert_eq!(
            loaded.dimensions,
            Some(ImageDimensions {
                width: 4,
                height: 2,
            })
        );
        assert_eq!(loaded.byte_length, Some(image_bytes.len() as u64));
        assert_eq!(loaded.source_path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn raw_image_size_uses_original_payload_for_inline_and_external_sources() {
        let directory = tempfile::tempdir().expect("无法创建原始图片测试目录");
        let store = crate::services::storage::RawBlobStore::new(directory.path().join("blobs"))
            .expect("无法创建测试 blob 存储");
        let mut bmp = Vec::new();
        BmpEncoder::new(&mut bmp)
            .write_image(&vec![128; 128 * 128 * 3], 128, 128, ExtendedColorType::Rgb8)
            .expect("无法生成测试 BMP");
        let bytes = &bmp[14..];
        let pending = store.write_pending(bytes).expect("无法暂存原始图片");
        let blob = store.commit_pending(&pending).expect("无法保存原始图片");
        let service = ImagePreviewService::new(store);
        for source in [
            ImagePreviewSource::Inline {
                bytes: bytes.into(),
            },
            ImagePreviewSource::External {
                sha256: *blob.sha256(),
                byte_length: blob.byte_length(),
                relative_path: blob.relative_path().to_string_lossy().into_owned(),
            },
        ] {
            let loaded = service
                .load_with_dimensions(&source, FormatKind::Image, "CF_DIB", (16, 16))
                .expect("无法加载原始图片预览");
            assert_eq!(loaded.byte_length, Some(bytes.len() as u64));
            assert!(loaded.source_path.is_none());
            assert_eq!(
                loaded.dimensions,
                Some(ImageDimensions {
                    width: 128,
                    height: 128
                })
            );
        }
    }

    #[cfg(target_os = "windows")]
    fn file_list_bytes(path: &Path) -> Vec<u8> {
        let mut bytes = vec![0_u8; 20];
        bytes[0..4].copy_from_slice(&20_u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&1_u32.to_le_bytes());
        bytes.extend(path.as_os_str().encode_wide().flat_map(u16::to_le_bytes));
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes
    }

    #[cfg(not(target_os = "windows"))]
    fn file_list_bytes(path: &Path) -> Vec<u8> {
        format!("{}\0\0", path.to_string_lossy()).into_bytes()
    }

    #[test]
    fn dib_bytes_decode_without_writing_back_to_file() {
        let mut bmp = Vec::new();
        BmpEncoder::new(&mut bmp)
            .write_image(&[255, 0, 0], 1, 1, ExtendedColorType::Rgb8)
            .expect("无法生成测试 BMP");

        let image = decode_image(&bmp[14..], "CF_DIB", (16, 16)).expect("无法解码 DIB");
        assert_eq!(image.frame_count(), 1);
        assert_eq!(
            image.as_bytes(0).expect("缺少 DIB 图片帧"),
            &[0, 0, 255, 255]
        );
    }

    #[test]
    fn dib_can_be_wrapped_as_bmp_without_changing_payload() {
        let mut bmp = Vec::new();
        BmpEncoder::new(&mut bmp)
            .write_image(&[255, 0, 0], 1, 1, ExtendedColorType::Rgb8)
            .expect("无法生成测试 BMP");

        let wrapped = bmp_file_bytes_from_dib(&bmp[14..]).expect("无法补回 BMP 文件头");
        assert_eq!(&wrapped[..2], b"BM");
        assert_eq!(
            u32::from_le_bytes(wrapped[2..6].try_into().unwrap()),
            wrapped.len() as u32
        );
        assert_eq!(u32::from_le_bytes(wrapped[10..14].try_into().unwrap()), 54);
        assert_eq!(&wrapped[14..], &bmp[14..]);
    }

    #[test]
    fn invalid_dib_cannot_be_wrapped_as_bmp() {
        assert!(bmp_file_bytes_from_dib(&[40, 0, 0]).is_none());
    }

    #[test]
    fn dibv5_bitfield_bytes_are_converted_and_decoded() {
        let mut dibv5 = vec![0_u8; 128];
        dibv5[0..4].copy_from_slice(&124_u32.to_le_bytes());
        dibv5[4..8].copy_from_slice(&1_i32.to_le_bytes());
        dibv5[8..12].copy_from_slice(&1_i32.to_le_bytes());
        dibv5[12..14].copy_from_slice(&1_u16.to_le_bytes());
        dibv5[14..16].copy_from_slice(&32_u16.to_le_bytes());
        dibv5[16..20].copy_from_slice(&3_u32.to_le_bytes());
        dibv5[20..24].copy_from_slice(&4_u32.to_le_bytes());
        dibv5[40..44].copy_from_slice(&0x00ff0000_u32.to_le_bytes());
        dibv5[44..48].copy_from_slice(&0x0000ff00_u32.to_le_bytes());
        dibv5[48..52].copy_from_slice(&0x000000ff_u32.to_le_bytes());
        dibv5[52..56].copy_from_slice(&0xff000000_u32.to_le_bytes());
        dibv5[124..128].copy_from_slice(&[0, 0, 255, 255]);

        let image = decode_image(&dibv5, "CF_DIBV5", (16, 16)).expect("无法解码 DIBV5");
        assert_eq!(image.frame_count(), 1);
    }

    #[test]
    fn thumbnail_preserves_aspect_ratio_without_upscaling() {
        assert_eq!(
            thumbnail_dimensions(15_200, 11_400, (4096, 120)),
            (160, 120)
        );
        assert_eq!(thumbnail_dimensions(100, 50, (4096, 120)), (100, 50));
    }

    fn animated_gif_bytes(frame_count: usize, delay_ms: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for _ in 0..frame_count {
                encoder
                    .encode_frame(image::Frame::from_parts(
                        image::RgbaImage::from_pixel(4, 2, image::Rgba([255, 0, 0, 255])),
                        0,
                        0,
                        image::Delay::from_numer_denom_ms(delay_ms, 1),
                    ))
                    .expect("无法生成测试 GIF");
            }
        }
        bytes
    }

    #[test]
    fn animated_gif_preserves_long_loops_and_frame_delays() {
        let bytes = animated_gif_bytes(80, 50);
        let image = decode_animated_gif(&bytes, (2, 2), 1024 * 1024, &AtomicBool::new(false))
            .expect("GIF 解码失败")
            .expect("缺少动画");
        assert_eq!(image.frame_count(), 80);
        assert_eq!(image.as_bytes(0).expect("缺少首帧").len(), 8);
        assert_eq!(
            std::time::Duration::from(image.delay(79)),
            std::time::Duration::from_millis(50)
        );
        assert_eq!(
            &image.as_bytes(0).expect("缺少首帧")[..4],
            &[0, 0, 255, 255]
        );
    }

    #[test]
    fn animated_gif_respects_budget_without_returning_partial_loops() {
        let bytes = animated_gif_bytes(3, 50);
        assert!(matches!(
            decode_animated_gif(&bytes, (4, 2), 64, &AtomicBool::new(false)),
            Err(ImagePreviewError::AllocationLimit)
        ));
    }

    #[test]
    fn animated_gif_cancellation_skips_decoding() {
        assert!(
            decode_animated_gif(b"GIF89a", (4, 2), 1024, &AtomicBool::new(true))
                .expect("取消解码不应报错")
                .is_none()
        );
    }

    #[test]
    fn animated_gif_normalizes_zero_frame_delay() {
        let bytes = animated_gif_bytes(2, 0);
        let image = decode_animated_gif(&bytes, (4, 2), 1024, &AtomicBool::new(false))
            .expect("GIF 解码失败")
            .expect("缺少动画");
        assert_eq!(
            std::time::Duration::from(image.delay(0)),
            std::time::Duration::from_millis(100)
        );
    }
}

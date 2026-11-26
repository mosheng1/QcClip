use std::{
    collections::BTreeMap,
    fs,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};

const GROUPS_FILE_NAME: &str = "groups.json";
const GROUP_ICONS_DIRECTORY_NAME: &str = ".groups_icons";
const MAX_GROUP_ICON_BYTES: u64 = 32 * 1024 * 1024;
const USAGE_FILE_NAME: &str = ".usage.json";
// 使用分数按半衰期衰减，避免很久以前常用的图片一直排在最前。
const USAGE_HALF_LIFE_SECS: f64 = 14.0 * 24.0 * 60.0 * 60.0;
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];
// Windows 原生拖放层把网络地址包装成此伪路径，再由图库导入阶段下载。
const REMOTE_IMAGE_PATH_PREFIX: &str = "qcclip-url:";
const REMOTE_IMAGE_REFERRER_SEPARATOR: &str = "\nqcclip-referrer:";
const MAX_REMOTE_IMAGE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ImageLibraryGroup {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default)]
    has_icon: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ImageLibraryImage {
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) is_gif: bool,
    pub(crate) file_type: String,
    pub(crate) modified_at: SystemTime,
    pub(crate) created_at: SystemTime,
    pub(crate) usage_score: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct ImageUsage {
    score: f64,
    last_used: u64,
}

impl ImageUsage {
    fn score_at(&self, now: u64) -> f64 {
        let elapsed = now.saturating_sub(self.last_used) as f64;
        self.score * 0.5_f64.powf(elapsed / USAGE_HALF_LIFE_SECS)
    }
}

// 组 ID -> 文件名 -> 使用记录。
type UsageMap = BTreeMap<String, BTreeMap<String, ImageUsage>>;

// 粘贴可能并发记录使用次数，串行化使用记录文件的读改写。
static USAGE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug)]
pub(crate) struct ImageLibraryService {
    root: PathBuf,
}

impl ImageLibraryService {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub(crate) fn load_groups(&self) -> Result<Vec<ImageLibraryGroup>, String> {
        let _operation =
            crate::services::data_runtime::begin_operation().map_err(|error| error.to_string())?;
        fs::create_dir_all(self.icon_directory())
            .map_err(|error| format!("创建图库图标目录失败：{error}"))?;
        let groups_path = self.root.join(GROUPS_FILE_NAME);
        let content = match fs::read_to_string(groups_path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("读取图库组信息失败：{error}")),
        };
        let mut groups: Vec<ImageLibraryGroup> = serde_json::from_str(&content)
            .map_err(|error| format!("解析图库组信息失败：{error}"))?;
        let mut seen_ids = Vec::new();
        let mut seen_names = Vec::new();
        groups.retain(|group| {
            if !valid_group_name(&group.name)
                || uuid::Uuid::parse_str(&group.id).is_err()
                || seen_ids.contains(&group.id)
                || seen_names
                    .iter()
                    .any(|name: &String| name.eq_ignore_ascii_case(&group.name))
            {
                return false;
            }
            seen_ids.push(group.id.clone());
            seen_names.push(group.name.clone());
            true
        });
        Ok(groups)
    }

    pub(crate) fn create_group(
        &self,
        name: &str,
        icon_path: Option<&Path>,
    ) -> Result<ImageLibraryGroup, String> {
        let _operation =
            crate::services::data_runtime::begin_operation().map_err(|error| error.to_string())?;
        let name = name.trim();
        if !valid_group_name(name) {
            return Err("组名称不能为空，且不能包含 /\\:*?\"<>| 等路径字符".to_owned());
        }
        let mut groups = self.load_groups()?;
        if groups
            .iter()
            .any(|group| group.name.eq_ignore_ascii_case(name))
        {
            return Err("已存在同名图库组".to_owned());
        }

        let id = uuid::Uuid::new_v4().to_string();
        let group = ImageLibraryGroup {
            id,
            name: name.to_owned(),
            has_icon: icon_path.is_some(),
        };
        fs::create_dir_all(self.group_directory(&group)?)
            .map_err(|error| format!("创建图库组目录失败：{error}"))?;
        if let Some(icon_path) = icon_path {
            if let Err(error) = self.save_group_icon(&group, icon_path) {
                let _ = fs::remove_dir_all(self.group_directory(&group)?);
                return Err(error);
            }
        }
        groups.push(group.clone());
        if let Err(error) = self.save_groups(&groups) {
            let _ = fs::remove_dir_all(self.group_directory(&group)?);
            if let Some(icon_path) = self.group_icon_path(&group) {
                let _ = fs::remove_file(icon_path);
            }
            return Err(error);
        }
        Ok(group)
    }

    pub(crate) fn update_group(
        &self,
        group: &ImageLibraryGroup,
        name: &str,
        icon_path: Option<&Path>,
    ) -> Result<ImageLibraryGroup, String> {
        let _operation =
            crate::services::data_runtime::begin_operation().map_err(|error| error.to_string())?;
        let name = name.trim();
        if !valid_group_name(name) {
            return Err("组名称不能为空，且不能包含 /\\:*?\"<>| 等路径字符".to_owned());
        }
        let mut groups = self.load_groups()?;
        let index = groups
            .iter()
            .position(|candidate| candidate.id == group.id)
            .ok_or_else(|| "图库组不存在".to_owned())?;
        if groups
            .iter()
            .enumerate()
            .any(|(candidate_index, candidate)| {
                candidate_index != index && candidate.name.eq_ignore_ascii_case(name)
            })
        {
            return Err("已存在同名图库组".to_owned());
        }

        let current = groups[index].clone();
        let updated = ImageLibraryGroup {
            id: current.id.clone(),
            name: name.to_owned(),
            has_icon: current.has_icon || icon_path.is_some(),
        };
        let icon_bytes = icon_path
            .map(|path| self.encode_group_icon(path))
            .transpose()?;
        let old_directory = self.group_directory(&current)?;
        let new_directory = self.group_directory(&updated)?;
        let renamed = current.name != updated.name;
        if renamed {
            if new_directory.exists() {
                return Err("目标组目录已存在".to_owned());
            }
            fs::rename(&old_directory, &new_directory)
                .map_err(|error| format!("重命名图库组目录失败：{error}"))?;
        }

        if let Some(icon_bytes) = icon_bytes {
            if let Err(error) = self.save_group_icon_bytes(&updated, &icon_bytes) {
                if renamed {
                    let _ = fs::rename(&new_directory, &old_directory);
                }
                return Err(error);
            }
        }

        groups[index] = updated.clone();
        if let Err(error) = self.save_groups(&groups) {
            if renamed {
                let _ = fs::rename(&new_directory, &old_directory);
            }
            return Err(error);
        }
        Ok(updated)
    }

    pub(crate) fn delete_group(&self, group: &ImageLibraryGroup) -> Result<(), String> {
        // 删除期间暂停其他数据操作，避免图片读取或导入重新创建组目录。
        crate::services::data_runtime::with_exclusive_operation(|| {
            let mut groups = self.load_groups()?;
            let index = groups
                .iter()
                .position(|candidate| candidate.id == group.id)
                .ok_or_else(|| "图库组不存在".to_owned())?;
            let current = groups.remove(index);
            let directory = self.group_directory(&current)?;
            let staging = self
                .root
                .join(format!(".deleted-group-{}", uuid::Uuid::new_v4()));
            let staged = match fs::symlink_metadata(&directory) {
                Ok(metadata) => {
                    if !metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err("图库组目录无效，无法删除".to_owned());
                    }
                    fs::rename(&directory, &staging)
                        .map_err(|error| format!("准备删除图库组目录失败：{error}"))?;
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(format!("读取图库组目录失败：{error}")),
            };

            // 先保存组列表，失败时恢复目录，避免丢失组内图片。
            if let Err(error) = self.save_groups(&groups) {
                if staged {
                    if let Err(restore_error) = fs::rename(&staging, &directory) {
                        return Err(format!(
                            "{error}；恢复图库组目录失败：{restore_error}，图片保留在 {}",
                            staging.display(),
                        ));
                    }
                }
                return Err(error);
            }
            if staged {
                if let Err(error) = fs::remove_dir_all(&staging) {
                    tracing::warn!(
                        "图库组已删除，清理图片目录失败（{}）：{error}",
                        staging.display()
                    );
                }
            }
            if let Some(path) = self.group_icon_path(&current) {
                if let Err(error) = fs::remove_file(&path) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        tracing::warn!(
                            "图库组已删除，清理组图标失败（{}）：{error}",
                            path.display()
                        );
                    }
                }
            }
            self.update_usage_logged(|usage| {
                usage.remove(&current.id);
            });
            Ok(())
        })
    }

    pub(crate) fn list_images(
        &self,
        group: &ImageLibraryGroup,
    ) -> Result<Vec<ImageLibraryImage>, String> {
        let _operation =
            crate::services::data_runtime::begin_operation().map_err(|error| error.to_string())?;
        let directory = self.group_directory(group)?;
        fs::create_dir_all(&directory).map_err(|error| format!("读取图库组目录失败：{error}"))?;
        let entries =
            fs::read_dir(directory).map_err(|error| format!("读取图库图片列表失败：{error}"))?;
        let usage = self.load_usage().unwrap_or_else(|error| {
            tracing::warn!("{error}");
            UsageMap::new()
        });
        let group_usage = usage.get(&group.id);
        let now = unix_now();
        let mut images = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                if !path.is_file() || !is_supported_image(&path) {
                    return None;
                }
                let name = path.file_name()?.to_string_lossy().into_owned();
                let is_gif = has_gif_extension(&path) || has_gif_signature(&path);
                let metadata = fs::metadata(&path).ok();
                let file_type = path
                    .extension()
                    .map(|extension| extension.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                let modified_at = metadata
                    .as_ref()
                    .and_then(|metadata| metadata.modified().ok())
                    .unwrap_or(UNIX_EPOCH);
                let created_at = metadata
                    .as_ref()
                    .and_then(|metadata| metadata.created().ok())
                    .unwrap_or(UNIX_EPOCH);
                let usage_score = group_usage
                    .and_then(|usage| usage.get(&name))
                    .map_or(0.0, |usage| usage.score_at(now));
                Some(ImageLibraryImage {
                    path,
                    name,
                    is_gif,
                    file_type,
                    modified_at,
                    created_at,
                    usage_score,
                })
            })
            .collect::<Vec<_>>();
        images.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.name.cmp(&right.name))
        });
        Ok(images)
    }

    pub(crate) fn rename_image(
        &self,
        group: &ImageLibraryGroup,
        source: &Path,
        name: &str,
    ) -> Result<PathBuf, String> {
        let _operation =
            crate::services::data_runtime::begin_operation().map_err(|error| error.to_string())?;
        let name = name.trim();
        if name.is_empty() || name == "." || name == ".." {
            return Err("图片名称不能为空".to_owned());
        }
        if name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
            return Err("图片名称不能包含 /\\:*?\"<>| 等路径字符".to_owned());
        }
        let directory = self.group_directory(group)?;
        let source = source
            .file_name()
            .map(|name| directory.join(name))
            .ok_or_else(|| "图片路径无效".to_owned())?;
        let destination_name = if Path::new(name).extension().is_none() {
            source
                .extension()
                .map(|extension| format!("{name}.{}", extension.to_string_lossy()))
                .unwrap_or_else(|| name.to_owned())
        } else {
            name.to_owned()
        };
        if !is_supported_image(Path::new(&destination_name)) {
            return Err("图片名称必须使用 png、jpg、jpeg、gif 或 webp 扩展名".to_owned());
        }
        let destination = directory.join(destination_name);
        if source == destination {
            return Ok(destination);
        }
        if destination.exists() {
            return Err("目标图片名称已存在".to_owned());
        }
        fs::rename(&source, &destination)
            .map_err(|error| format!("重命名图库图片失败：{error}"))?;
        if let (Some(old_name), Some(new_name)) = (source.file_name(), destination.file_name()) {
            let old_name = old_name.to_string_lossy().into_owned();
            let new_name = new_name.to_string_lossy().into_owned();
            self.update_usage_logged(|usage| {
                if let Some(group_usage) = usage.get_mut(&group.id)
                    && let Some(entry) = group_usage.remove(&old_name)
                {
                    group_usage.insert(new_name, entry);
                }
            });
        }
        Ok(destination)
    }

    pub(crate) fn move_image(
        &self,
        source_group: &ImageLibraryGroup,
        target_group: &ImageLibraryGroup,
        source: &Path,
    ) -> Result<(), String> {
        let _operation =
            crate::services::data_runtime::begin_operation().map_err(|error| error.to_string())?;
        if source_group.id == target_group.id {
            return Ok(());
        }
        let source_name = source
            .file_name()
            .ok_or_else(|| "图片路径无效".to_owned())?;
        let source_path = self.group_directory(source_group)?.join(source_name);
        let target_directory = self.group_directory(target_group)?;
        fs::create_dir_all(&target_directory)
            .map_err(|error| format!("创建目标图库组目录失败：{error}"))?;
        let target_path = target_directory.join(source_name);
        if target_path.exists() {
            return Err("目标图库组中已存在同名图片".to_owned());
        }
        fs::rename(&source_path, &target_path)
            .map_err(|error| format!("移动图库图片失败：{error}"))?;
        let name = source_name.to_string_lossy().into_owned();
        self.update_usage_logged(|usage| {
            if let Some(entry) = usage
                .get_mut(&source_group.id)
                .and_then(|group_usage| group_usage.remove(&name))
            {
                usage
                    .entry(target_group.id.clone())
                    .or_default()
                    .insert(name, entry);
            }
        });
        Ok(())
    }

    pub(crate) fn delete_image(
        &self,
        group: &ImageLibraryGroup,
        source: &Path,
    ) -> Result<(), String> {
        let _operation =
            crate::services::data_runtime::begin_operation().map_err(|error| error.to_string())?;
        let source_name = source
            .file_name()
            .ok_or_else(|| "图片路径无效".to_owned())?;
        let source_path = self.group_directory(group)?.join(source_name);
        fs::remove_file(&source_path).map_err(|error| format!("删除图库图片失败：{error}"))?;
        let name = source_name.to_string_lossy().into_owned();
        self.update_usage_logged(|usage| {
            if let Some(group_usage) = usage.get_mut(&group.id) {
                group_usage.remove(&name);
            }
        });
        Ok(())
    }

    /// 记录一次图片使用（粘贴或复制），供“使用频率”排序。
    pub(crate) fn record_image_use(&self, path: &Path) -> Result<(), String> {
        let _operation =
            crate::services::data_runtime::begin_operation().map_err(|error| error.to_string())?;
        let name = path
            .file_name()
            .ok_or_else(|| "图片路径无效".to_owned())?
            .to_string_lossy()
            .into_owned();
        let groups = self.load_groups()?;
        let group = groups
            .iter()
            .find(|group| {
                self.group_directory(group)
                    .is_ok_and(|directory| path.parent() == Some(directory.as_path()))
            })
            .ok_or_else(|| "图片不属于任何图库组".to_owned())?;
        let now = unix_now();
        self.update_usage(|usage| {
            // 顺带清理已不存在的组和图片，防止手动删除文件后残留记录。
            usage.retain(|group_id, group_usage| {
                let Some(directory) = groups
                    .iter()
                    .find(|group| &group.id == group_id)
                    .and_then(|group| self.group_directory(group).ok())
                else {
                    return false;
                };
                group_usage.retain(|name, _| directory.join(name).is_file());
                !group_usage.is_empty()
            });
            let entry = usage
                .entry(group.id.clone())
                .or_default()
                .entry(name)
                .or_default();
            entry.score = entry.score_at(now) + 1.0;
            entry.last_used = now;
        })
    }

    pub(crate) fn import_images_with_progress<F>(
        &self,
        group: &ImageLibraryGroup,
        paths: &[PathBuf],
        mut on_progress: F,
    ) -> Result<usize, String>
    where
        F: FnMut(usize, usize),
    {
        let _operation =
            crate::services::data_runtime::begin_operation().map_err(|error| error.to_string())?;
        let directory = self.group_directory(group)?;
        fs::create_dir_all(&directory).map_err(|error| format!("打开图库组目录失败：{error}"))?;
        let mut copied = 0;
        let total = paths.len();
        for (index, source) in paths.iter().enumerate() {
            let completed = index + 1;
            if let Some(url) = source
                .to_string_lossy()
                .strip_prefix(REMOTE_IMAGE_PATH_PREFIX)
            {
                let (url, referrer) = url
                    .split_once(REMOTE_IMAGE_REFERRER_SEPARATOR)
                    .map_or((url, None), |(url, referrer)| (url, Some(referrer)));
                let result = self.import_remote_image(&directory, url, referrer);
                if result.is_ok() {
                    copied += 1;
                }
                on_progress(completed, total);
                if let Err(error) = result {
                    return Err(error);
                }
                continue;
            }
            if !source.is_file() || !is_supported_image(source) {
                on_progress(completed, total);
                continue;
            }
            let Some(name) = source.file_name() else {
                on_progress(completed, total);
                continue;
            };
            let destination = directory.join(name);
            if destination.exists()
                || fs::canonicalize(source)
                    .ok()
                    .zip(fs::canonicalize(&destination).ok())
                    .is_some_and(|(source, destination)| source == destination)
            {
                on_progress(completed, total);
                continue;
            }
            if let Err(error) = fs::copy(source, destination) {
                on_progress(completed, total);
                return Err(format!("复制图库图片失败：{error}"));
            }
            copied += 1;
            on_progress(completed, total);
        }
        Ok(copied)
    }

    fn import_remote_image(
        &self,
        directory: &Path,
        url: &str,
        referrer: Option<&str>,
    ) -> Result<(), String> {
        self.import_remote_image_with_depth(directory, url, referrer, 0)
    }

    fn import_remote_image_with_depth(
        &self,
        directory: &Path,
        url: &str,
        referrer: Option<&str>,
        depth: u8,
    ) -> Result<(), String> {
        let normalized_url = url
            .replace("&amp;", "&")
            .replace("&#38;", "&")
            .replace("&#x26;", "&");
        let url = normalized_url.as_str();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("网络图片地址无效".to_owned());
        }
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .build();
        let mut request = agent
            .get(url)
            .set(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
                 AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/140.0.0.0 Safari/537.36",
            )
            .set(
                "Accept",
                "image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            )
            .set("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8");
        if let Some(referrer) = referrer
            .filter(|referrer| referrer.starts_with("https://") || referrer.starts_with("http://"))
        {
            request = request.set("Referer", referrer);
        }
        let response = request
            .call()
            .map_err(|error| format!("下载网络图片失败：{error}"))?;
        let content_type = response.header("Content-Type").unwrap_or("未知").to_owned();
        if let Some(length) = response.header("Content-Length")
            && length
                .parse::<usize>()
                .ok()
                .is_some_and(|length| length > MAX_REMOTE_IMAGE_BYTES)
        {
            return Err("网络图片超过 32 MB 限制".to_owned());
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take((MAX_REMOTE_IMAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("读取网络图片失败：{error}"))?;
        if bytes.len() > MAX_REMOTE_IMAGE_BYTES {
            return Err("网络图片超过 32 MB 限制".to_owned());
        }
        let format = match image::guess_format(&bytes) {
            Ok(format) => format,
            Err(error) => {
                let is_html = content_type.to_ascii_lowercase().contains("html")
                    || bytes.get(..128.min(bytes.len())).is_some_and(|prefix| {
                        let prefix = String::from_utf8_lossy(prefix).to_ascii_lowercase();
                        prefix.contains("<!doctype html")
                            || prefix.contains("<html")
                            || prefix.contains("<head")
                    });
                if depth == 0
                    && is_html
                    && let Some(image_url) = extract_html_image_url(&bytes)
                {
                    return self.import_remote_image_with_depth(directory, &image_url, referrer, 1);
                }
                return Err(format!(
                    "识别网络图片格式失败：{error}（Content-Type: {content_type}）"
                ));
            }
        };
        let extension = match format {
            image::ImageFormat::Png => "png",
            image::ImageFormat::Jpeg => "jpg",
            image::ImageFormat::Gif => "gif",
            image::ImageFormat::WebP => "webp",
            _ => return Err("网络资源不是支持的图片格式".to_owned()),
        };
        image::ImageReader::new(Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|error| format!("识别网络图片失败：{error}"))?
            .decode()
            .map_err(|error| format!("解析网络图片失败：{error}"))?;
        let name = remote_image_name(url, extension);
        let destination = unique_import_path(directory, &name);
        let mut file = fs::File::create(&destination)
            .map_err(|error| format!("创建网络图片文件失败：{error}"))?;
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.flush()) {
            let _ = fs::remove_file(&destination);
            return Err(format!("保存网络图片失败：{error}"));
        }
        Ok(())
    }

    pub(crate) fn group_icon_path(&self, group: &ImageLibraryGroup) -> Option<PathBuf> {
        group
            .has_icon
            .then(|| self.icon_directory().join(format!("{}.png", group.id)))
    }

    fn save_group_icon(&self, group: &ImageLibraryGroup, source: &Path) -> Result<(), String> {
        let bytes = self.encode_group_icon(source)?;
        self.save_group_icon_bytes(group, &bytes)
    }

    fn encode_group_icon(&self, source: &Path) -> Result<Vec<u8>, String> {
        let metadata =
            fs::metadata(source).map_err(|error| format!("读取组图标文件失败：{error}"))?;
        if !metadata.is_file() || metadata.len() > MAX_GROUP_ICON_BYTES {
            return Err("组图标文件无效或超过 32 MB".to_owned());
        }
        let mut reader = image::ImageReader::open(source)
            .map_err(|error| format!("打开组图标失败：{error}"))?
            .with_guessed_format()
            .map_err(|error| format!("识别组图标格式失败：{error}"))?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(128 * 1024 * 1024);
        reader.limits(limits);
        let thumbnail = reader
            .decode()
            .map_err(|error| format!("解码组图标失败：{error}"))?
            .thumbnail(64, 64);
        let mut bytes = Cursor::new(Vec::new());
        thumbnail
            .write_to(&mut bytes, image::ImageFormat::Png)
            .map_err(|error| format!("生成组图标缩略图失败：{error}"))?;
        Ok(bytes.into_inner())
    }

    fn save_group_icon_bytes(&self, group: &ImageLibraryGroup, bytes: &[u8]) -> Result<(), String> {
        let path = self.icon_directory().join(format!("{}.png", group.id));
        let mut file =
            AtomicWriteFile::open(path).map_err(|error| format!("打开组图标缓存失败：{error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("写入组图标缓存失败：{error}"))?;
        file.commit()
            .map_err(|error| format!("保存组图标缩略图失败：{error}"))
    }

    fn save_groups(&self, groups: &[ImageLibraryGroup]) -> Result<(), String> {
        fs::create_dir_all(&self.root).map_err(|error| format!("创建图库目录失败：{error}"))?;
        let mut content = serde_json::to_string_pretty(groups)
            .map_err(|error| format!("序列化图库组信息失败：{error}"))?;
        content.push('\n');
        let path = self.root.join(GROUPS_FILE_NAME);
        let mut file = AtomicWriteFile::open(path)
            .map_err(|error| format!("打开图库组信息文件失败：{error}"))?;
        file.write_all(content.as_bytes())
            .map_err(|error| format!("写入图库组信息失败：{error}"))?;
        file.commit()
            .map_err(|error| format!("保存图库组信息失败：{error}"))
    }

    fn load_usage(&self) -> Result<UsageMap, String> {
        match fs::read_to_string(self.root.join(USAGE_FILE_NAME)) {
            Ok(content) => serde_json::from_str(&content)
                .map_err(|error| format!("解析图库使用记录失败：{error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(UsageMap::new()),
            Err(error) => Err(format!("读取图库使用记录失败：{error}")),
        }
    }

    fn update_usage(&self, update: impl FnOnce(&mut UsageMap)) -> Result<(), String> {
        let _lock = USAGE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // 记录损坏时从空记录重新开始，不影响图库本身的使用。
        let mut usage = self.load_usage().unwrap_or_else(|error| {
            tracing::warn!("{error}");
            UsageMap::new()
        });
        update(&mut usage);
        fs::create_dir_all(&self.root).map_err(|error| format!("创建图库目录失败：{error}"))?;
        let content = serde_json::to_string(&usage)
            .map_err(|error| format!("序列化图库使用记录失败：{error}"))?;
        let mut file = AtomicWriteFile::open(self.root.join(USAGE_FILE_NAME))
            .map_err(|error| format!("打开图库使用记录文件失败：{error}"))?;
        file.write_all(content.as_bytes())
            .map_err(|error| format!("写入图库使用记录失败：{error}"))?;
        file.commit()
            .map_err(|error| format!("保存图库使用记录失败：{error}"))
    }

    // 使用记录只影响排序，同步失败时不让图片操作本身失败。
    fn update_usage_logged(&self, update: impl FnOnce(&mut UsageMap)) {
        if let Err(error) = self.update_usage(update) {
            tracing::warn!("同步图库使用记录失败：{error}");
        }
    }

    fn group_directory(&self, group: &ImageLibraryGroup) -> Result<PathBuf, String> {
        if uuid::Uuid::parse_str(&group.id).is_err() || !valid_group_name(&group.name) {
            return Err("图库组信息无效".to_owned());
        }
        Ok(self.root.join(&group.name))
    }

    fn icon_directory(&self) -> PathBuf {
        self.root.join(GROUP_ICONS_DIRECTORY_NAME)
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn valid_group_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && !name.eq_ignore_ascii_case(GROUP_ICONS_DIRECTORY_NAME)
        && !name.eq_ignore_ascii_case(USAGE_FILE_NAME)
        && name.len() <= 160
        && !name.ends_with('.')
        && !name.ends_with(' ')
        && !name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        })
}

fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            IMAGE_EXTENSIONS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(extension))
        })
}

fn remote_image_name(url: &str, extension: &str) -> String {
    let candidate = url
        .split(['?', '#'])
        .next()
        .and_then(|url| url.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("网络图片");
    let candidate = candidate
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    let stem = Path::new(&candidate)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("网络图片");
    format!("{stem}.{extension}")
}

fn extract_html_image_url(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let lower = text.to_ascii_lowercase();
    for marker in ["src=\"", "src='", "src="] {
        let mut offset = 0;
        while let Some(relative) = lower[offset..].find(marker) {
            let start = offset + relative + marker.len();
            let value = &text[start..];
            let end = value
                .bytes()
                .position(|character| {
                    character.is_ascii_whitespace() || matches!(character, b'"' | b'\'' | b'>')
                })
                .unwrap_or(value.len());
            let value = value[..end].trim();
            if value.starts_with("https://") || value.starts_with("http://") {
                return Some(
                    value
                        .replace("&amp;", "&")
                        .replace("&#38;", "&")
                        .replace("&#x26;", "&"),
                );
            }
            offset = start;
        }
    }
    None
}

fn unique_import_path(directory: &Path, name: &str) -> PathBuf {
    let destination = directory.join(name);
    if !destination.exists() {
        return destination;
    }
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("网络图片");
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("png");
    for index in 2.. {
        let candidate = directory.join(format!("{stem} ({index}).{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("图片文件名序号溢出")
}

fn has_gif_signature(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut signature = [0; 6];
    if file.read_exact(&mut signature).is_err() {
        return false;
    }
    &signature == b"GIF87a" || &signature == b"GIF89a"
}

fn has_gif_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gif"))
}

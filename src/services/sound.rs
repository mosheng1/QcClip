use std::f32::consts::TAU;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, mpsc};
use std::thread;
use std::time::Duration;

use raw_input::{Event, Key, Listen};
use rodio::buffer::SamplesBuffer;
use rodio::{Decoder, OutputStream, Sink, Source};

use crate::config::{SOUND_VOLUME_MAX, SoundConfig, SoundPreset};

const SAMPLE_RATE: u32 = 44_100;
const MAX_PENDING_SOUND_REQUESTS: usize = 1;
const AUDIO_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
pub(crate) enum SoundEvent {
    Paste,
    GlobalPasteShortcut,
    Copy,
    QuickPasteScroll,
}

#[derive(Clone)]
struct SoundRequest {
    preset: SoundPreset,
    volume: f32,
    directory: Option<PathBuf>,
}

impl SoundRequest {
    fn new(
        config: &SoundConfig,
        preset: SoundPreset,
        volume: u8,
        directory: Option<PathBuf>,
    ) -> Option<Self> {
        if !config.enabled || preset == SoundPreset::Disabled || volume == 0 {
            return None;
        }
        Some(Self {
            preset,
            volume: f32::from(volume.min(SOUND_VOLUME_MAX)) / f32::from(SOUND_VOLUME_MAX),
            directory,
        })
    }
}

struct SoundPlayer {
    sender: mpsc::SyncSender<SoundRequest>,
    disconnected_reported: AtomicBool,
}

static SOUND_CONFIG: OnceLock<RwLock<SoundConfig>> = OnceLock::new();
static SOUND_DIRECTORY: OnceLock<RwLock<PathBuf>> = OnceLock::new();
static SOUND_PLAYER: OnceLock<SoundPlayer> = OnceLock::new();
static SOUND_SINK: RwLock<Option<Arc<Sink>>> = RwLock::new(None);
static GLOBAL_PASTE_KEYS: OnceLock<Mutex<std::collections::HashSet<Key>>> = OnceLock::new();

pub(crate) fn install(config: SoundConfig, sound_directory: PathBuf) {
    if let Err(error) = fs::create_dir_all(&sound_directory) {
        tracing::error!("创建外部音效目录失败：{error}");
    }
    if SOUND_DIRECTORY
        .set(RwLock::new(sound_directory.clone()))
        .is_err()
    {
        set_data_directory(sound_directory);
    }
    if SOUND_CONFIG.set(RwLock::new(config.clone())).is_err() {
        set_config(config);
    }
    let pressed_keys = GLOBAL_PASTE_KEYS.get_or_init(Default::default);
    Listen::subscribe(move |event| {
        let (key, is_down) = match event {
            Event::KeyDown { key, .. } => (key, true),
            Event::KeyUp { key, .. } => (key, false),
            _ => return,
        };
        let should_play = {
            let mut pressed_keys = pressed_keys
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !is_down {
                pressed_keys.remove(&key);
                false
            } else {
                let newly_pressed = pressed_keys.insert(key);
                newly_pressed && is_global_paste_shortcut(key, &pressed_keys)
            }
        };
        if should_play {
            play(SoundEvent::GlobalPasteShortcut);
        }
    });
}

pub(crate) fn set_data_directory(directory: PathBuf) {
    if let Err(error) = fs::create_dir_all(&directory) {
        tracing::error!(%error, "创建外部音效目录失败");
        return;
    }
    if let Some(current) = SOUND_DIRECTORY.get() {
        if let Ok(mut current) = current.write() {
            *current = directory;
        }
    }
}

pub(crate) fn set_config(config: SoundConfig) {
    let Some(settings) = SOUND_CONFIG.get() else {
        return;
    };
    match settings.write() {
        Ok(mut current) => {
            *current = config;
            if !current.enabled {
                if let Ok(sink) = SOUND_SINK.read()
                    && let Some(sink) = sink.as_ref()
                {
                    sink.stop();
                }
            }
        }
        Err(_) => tracing::error!("更新音效配置失败：配置锁状态异常"),
    }
}

pub(crate) fn external_presets() -> Vec<SoundPreset> {
    let Ok(_operation) = crate::services::data_runtime::begin_operation() else {
        return Vec::new();
    };
    let Some(directory) = SOUND_DIRECTORY.get() else {
        return Vec::new();
    };
    let Ok(directory) = directory.read() else {
        return Vec::new();
    };
    scan_external_presets(&directory)
}

pub(crate) fn open_sound_directory() {
    let Some(directory) = SOUND_DIRECTORY.get() else {
        tracing::error!("打开外部音效目录失败：音效目录未初始化");
        return;
    };
    let Ok(directory) = directory.read() else {
        tracing::error!("打开外部音效目录失败：音效目录锁状态异常");
        return;
    };
    let directory = directory.clone();
    if let Err(error) = fs::create_dir_all(&directory) {
        tracing::error!("创建外部音效目录失败：{error}");
        return;
    }

    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer")
        .arg(directory)
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(directory).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(directory)
        .spawn();

    if let Err(error) = result {
        tracing::error!("打开外部音效目录失败：{error}");
    }
}

pub(crate) fn play(event: SoundEvent) {
    let Some(settings) = SOUND_CONFIG.get() else {
        return;
    };
    let config = match settings.read() {
        Ok(config) => config,
        Err(_) => {
            tracing::error!("读取音效配置失败：配置锁状态异常");
            return;
        }
    };
    let effect = match event {
        SoundEvent::GlobalPasteShortcut if !config.global_paste => return,
        SoundEvent::Paste | SoundEvent::GlobalPasteShortcut => config.paste.clone(),
        SoundEvent::Copy => config.copy.clone(),
        SoundEvent::QuickPasteScroll => config.quick_paste_scroll.clone(),
    };
    drop(config);
    enqueue(effect.preset, effect.volume);
}

fn is_global_paste_shortcut(key: Key, pressed_keys: &std::collections::HashSet<Key>) -> bool {
    let control =
        pressed_keys.contains(&Key::ControlLeft) || pressed_keys.contains(&Key::ControlRight);
    let shift = pressed_keys.contains(&Key::ShiftLeft) || pressed_keys.contains(&Key::ShiftRight);
    let alt = pressed_keys.contains(&Key::AltLeft) || pressed_keys.contains(&Key::AltRight);
    let meta = pressed_keys.contains(&Key::MetaLeft) || pressed_keys.contains(&Key::MetaRight);
    !alt && !meta
        && match key {
            Key::KeyV => control && !shift,
            Key::Insert => shift && !control,
            _ => false,
        }
}

pub(crate) fn preview(preset: SoundPreset, volume: u8) {
    enqueue(preset, volume);
}

fn enqueue(preset: SoundPreset, volume: u8) {
    let Some(settings) = SOUND_CONFIG.get() else {
        return;
    };
    let Ok(config) = settings.read() else {
        tracing::error!("读取音效配置失败：配置锁状态异常");
        return;
    };
    let directory = SOUND_DIRECTORY
        .get()
        .and_then(|directory| directory.read().ok().map(|directory| directory.clone()));
    let Some(request) = SoundRequest::new(&config, preset, volume, directory) else {
        return;
    };
    let player = SOUND_PLAYER.get_or_init(SoundPlayer::new);
    if let Err(error) = player.sender.try_send(request) {
        if matches!(error, mpsc::TrySendError::Disconnected(_))
            && !player.disconnected_reported.swap(true, Ordering::Relaxed)
        {
            tracing::warn!("音效播放请求未发送，音效线程已退出");
        }
    }
}

impl SoundPlayer {
    fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(MAX_PENDING_SOUND_REQUESTS);
        let spawn_result = thread::Builder::new()
            .name("qcclip-sound-player".to_owned())
            .spawn(move || audio_worker(receiver));
        if let Err(error) = spawn_result {
            tracing::error!("启动音效播放线程失败：{error}");
        }
        Self {
            sender,
            disconnected_reported: AtomicBool::new(false),
        }
    }
}

fn audio_worker(receiver: mpsc::Receiver<SoundRequest>) {
    let Some(settings) = SOUND_CONFIG.get() else {
        return;
    };
    let mut output: Option<(OutputStream, Arc<Sink>)> = None;
    loop {
        let request = match receive_sound_request(&receiver, output.is_some()) {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if output.as_ref().is_some_and(|(_, sink)| sink.empty()) {
                    if let Ok(mut sink) = SOUND_SINK.write() {
                        *sink = None;
                    }
                    // 释放输出流才能停止 WASAPI 的持续静音回调。
                    output = None;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if output.is_none() {
            let (stream, stream_handle) = match OutputStream::try_default() {
                Ok(output) => output,
                Err(error) => {
                    tracing::error!(%error, "初始化音效输出设备失败");
                    continue;
                }
            };
            let sink = match Sink::try_new(&stream_handle) {
                Ok(sink) => Arc::new(sink),
                Err(error) => {
                    tracing::error!(%error, "创建音效播放通道失败");
                    continue;
                }
            };
            if let Ok(mut current) = SOUND_SINK.write() {
                *current = Some(sink.clone());
            }
            output = Some((stream, sink));
        }
        let Some((_, sink)) = output.as_ref() else {
            continue;
        };
        // 播放期间丢弃新请求，避免滚动等高频操作把过期提示排队播放。
        if !sink.empty() {
            continue;
        }

        match request.preset {
            SoundPreset::Disabled => {}
            SoundPreset::CrispChime | SoundPreset::BrightPing | SoundPreset::QuickPasteScroll => {
                let source = SamplesBuffer::new(1, SAMPLE_RATE, generate_samples(request.preset))
                    .amplify(request.volume);
                append_if_enabled(settings, sink, source);
            }
            SoundPreset::External(file_name) => {
                let Some(directory) = request.directory.as_deref() else {
                    tracing::error!("无法播放外部音效：音效目录未初始化");
                    continue;
                };
                let Some(source) = load_external_source(directory, &file_name) else {
                    continue;
                };
                append_if_enabled(
                    settings,
                    sink,
                    source.amplify(request.volume).convert_samples(),
                );
            }
        }
    }
    if let Ok(mut sink) = SOUND_SINK.write() {
        *sink = None;
    }
    tracing::debug!("音效播放线程已退出");
}

fn receive_sound_request(
    receiver: &mpsc::Receiver<SoundRequest>,
    output_active: bool,
) -> Result<SoundRequest, mpsc::RecvTimeoutError> {
    if output_active {
        receiver.recv_timeout(AUDIO_IDLE_TIMEOUT)
    } else {
        receiver
            .recv()
            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
    }
}

fn append_if_enabled(
    settings: &RwLock<SoundConfig>,
    sink: &Sink,
    source: impl Source<Item = f32> + Send + 'static,
) {
    let Ok(config) = settings.read() else {
        tracing::error!("读取音效配置失败：配置锁状态异常");
        return;
    };
    // 与关闭开关时的写锁互斥，防止已入队或正在解码的音效在关闭后开始播放。
    if config.enabled {
        sink.append(source);
    }
}

fn scan_external_presets(directory: &Path) -> Vec<SoundPreset> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::error!("读取外部音效目录失败：{error}");
            return Vec::new();
        }
    };
    let mut presets = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_file() {
                return None;
            }
            let file_name = entry.file_name().into_string().ok()?;
            SoundPreset::is_valid_external_file_name(&file_name)
                .then(|| SoundPreset::External(file_name))
        })
        .collect::<Vec<_>>();
    presets.sort_by_cached_key(|preset| {
        preset
            .external_file_name()
            .unwrap_or_default()
            .to_ascii_lowercase()
    });
    presets
}

fn load_external_source(directory: &Path, file_name: &str) -> Option<Decoder<Cursor<Vec<u8>>>> {
    let Ok(_operation) = crate::services::data_runtime::begin_operation() else {
        return None;
    };
    if !SoundPreset::is_valid_external_file_name(file_name) {
        tracing::error!("拒绝播放非法的外部音效文件名：{file_name}");
        return None;
    }
    let path = directory.join(file_name);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "打开外部音效文件失败");
            return None;
        }
    };
    match Decoder::new(Cursor::new(bytes)) {
        Ok(source) => Some(source),
        Err(error) => {
            tracing::error!(%error, "解码外部音效文件失败");
            None
        }
    }
}

fn generate_samples(preset: SoundPreset) -> Vec<f32> {
    match preset {
        SoundPreset::Disabled => Vec::new(),
        // 轻快上扬：与示例中的粘贴音效一致。
        SoundPreset::CrispChime => generate_sweep(660.0, 980.0, 0.06, 0.10, 0.15),
        // 双音确认：两个短促的上扬音，中间保留很短的间隔。
        SoundPreset::BrightPing => {
            let mut samples = generate_sweep(880.0, 1_200.0, 0.045, 0.07, 0.16);
            let gap_count = (0.025 * SAMPLE_RATE as f32).ceil() as usize;
            samples.extend(std::iter::repeat_n(0.0, gap_count));
            samples.extend(generate_sweep(1_040.0, 1_440.0, 0.045, 0.07, 0.13));
            samples
        }
        SoundPreset::QuickPasteScroll => generate_quick_paste_scroll_click(),
        SoundPreset::External(_) => Vec::new(),
    }
}

fn generate_quick_paste_scroll_click() -> Vec<f32> {
    const NOISE_DURATION: f32 = 0.009;
    const OSCILLATOR_DURATION: f32 = 0.012;
    const RAMP_DURATION: f32 = 0.001;
    const MASTER_VOLUME: f32 = 0.75;

    let sample_count = (OSCILLATOR_DURATION * SAMPLE_RATE as f32).ceil() as usize;
    let noise_sample_count = (NOISE_DURATION * SAMPLE_RATE as f32).ceil() as usize;
    let high_pass_alpha = {
        let time_constant = 1.0 / (std::f32::consts::TAU * 3_200.0);
        time_constant / (time_constant + 1.0 / SAMPLE_RATE as f32)
    };
    let mut random_state = 0x6d2b_79f5_u32;
    let mut previous_noise = 0.0;
    let mut previous_filtered_noise = 0.0;
    let oscillator_frequency = 1_800.0;

    (0..sample_count)
        .map(|index| {
            let time = index as f32 / SAMPLE_RATE as f32;
            let mut sample = 0.0;

            if index < noise_sample_count {
                random_state ^= random_state << 13;
                random_state ^= random_state >> 17;
                random_state ^= random_state << 5;
                let noise = (random_state as f32 / u32::MAX as f32) * 2.0 - 1.0;
                let filtered_noise =
                    high_pass_alpha * (previous_filtered_noise + noise - previous_noise);
                previous_noise = noise;
                previous_filtered_noise = filtered_noise;
                let envelope = 0.22 * (0.0001_f32 / 0.22).powf(time / NOISE_DURATION);
                sample += filtered_noise * envelope;
            }

            let oscillator_envelope = if time < RAMP_DURATION {
                0.0001 * (0.07_f32 / 0.0001).powf(time / RAMP_DURATION)
            } else {
                0.07 * (0.0001_f32 / 0.07)
                    .powf((time - RAMP_DURATION) / (OSCILLATOR_DURATION - RAMP_DURATION))
            };
            let phase = std::f32::consts::TAU * oscillator_frequency * time;
            let square_wave = if phase.sin() >= 0.0 { 1.0 } else { -1.0 };
            sample += square_wave * oscillator_envelope;

            sample * MASTER_VOLUME
        })
        .collect()
}

fn generate_sweep(
    start_frequency: f32,
    end_frequency: f32,
    ramp_duration: f32,
    duration: f32,
    amplitude: f32,
) -> Vec<f32> {
    let sample_count = (duration * SAMPLE_RATE as f32).ceil() as usize;
    let frequency_ratio = end_frequency / start_frequency;
    let gain_ratio = 0.001 / amplitude;

    (0..sample_count)
        .map(|index| {
            let time = index as f32 / SAMPLE_RATE as f32;
            let phase_cycles = sweep_phase_cycles(
                time,
                start_frequency,
                end_frequency,
                ramp_duration,
                frequency_ratio,
            );
            let gain = amplitude * gain_ratio.powf((time / duration).min(1.0));
            (TAU * phase_cycles).sin() * gain
        })
        .collect()
}

fn sweep_phase_cycles(
    time: f32,
    start_frequency: f32,
    end_frequency: f32,
    ramp_duration: f32,
    frequency_ratio: f32,
) -> f32 {
    let ramp_time = time.min(ramp_duration);
    let ratio_log = frequency_ratio.ln();
    let ramp_cycles = if ratio_log.abs() < f32::EPSILON {
        start_frequency * ramp_time
    } else {
        start_frequency * ramp_duration * (frequency_ratio.powf(ramp_time / ramp_duration) - 1.0)
            / ratio_log
    };
    if time <= ramp_duration {
        ramp_cycles
    } else {
        ramp_cycles + end_frequency * (time - ramp_duration)
            - if ratio_log.abs() < f32::EPSILON {
                start_frequency * ramp_duration
            } else {
                start_frequency * ramp_duration * (frequency_ratio - 1.0) / ratio_log
            }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SoundRequest, append_if_enabled, generate_samples, is_global_paste_shortcut,
        scan_external_presets,
    };
    use crate::config::{SoundConfig, SoundPreset};
    use raw_input::Key;
    use rodio::{Sink, buffer::SamplesBuffer};
    use std::collections::HashSet;
    use std::fs;
    use std::sync::RwLock;

    #[test]
    fn global_paste_shortcut_matching_excludes_copy_and_modified_paste_keys() {
        let control_c = HashSet::from([Key::ControlLeft, Key::KeyC]);
        let control_v = HashSet::from([Key::ControlLeft, Key::KeyV]);
        let shift_insert = HashSet::from([Key::ShiftLeft, Key::Insert]);
        let control_shift_v = HashSet::from([Key::ControlLeft, Key::ShiftLeft, Key::KeyV]);

        assert!(!is_global_paste_shortcut(Key::KeyC, &control_c));
        assert!(is_global_paste_shortcut(Key::KeyV, &control_v));
        assert!(is_global_paste_shortcut(Key::Insert, &shift_insert));
        assert!(!is_global_paste_shortcut(Key::KeyV, &control_shift_v));
    }

    #[test]
    fn master_switch_blocks_all_presets_and_keeps_unconfigured_effects_silent() {
        let mut config = SoundConfig::default();
        for enabled in [false, true] {
            config.enabled = enabled;
            for preset in [
                SoundPreset::Disabled,
                SoundPreset::CrispChime,
                SoundPreset::BrightPing,
                SoundPreset::QuickPasteScroll,
                SoundPreset::External("提示.wav".to_owned()),
            ] {
                for volume in [0, 35, 100, u8::MAX] {
                    let should_play = enabled && preset != SoundPreset::Disabled && volume > 0;
                    let request = SoundRequest::new(&config, preset.clone(), volume, None);
                    assert_eq!(request.is_some(), should_play);
                    if let Some(request) = request {
                        assert_eq!(request.preset, preset);
                        assert_eq!(request.volume, f32::from(volume.min(100)) / 100.0);
                    }
                }
            }
        }
    }

    #[test]
    fn queued_sound_is_blocked_if_master_switch_was_disabled() {
        let settings = RwLock::new(SoundConfig::default());
        let (sink, _output) = Sink::new_idle();
        let request = SoundRequest::new(
            &settings.read().expect("无法读取测试音效配置"),
            SoundPreset::CrispChime,
            70,
            None,
        )
        .expect("启用音效时应生成播放请求");
        settings.write().expect("无法更新测试音效配置").enabled = false;
        append_if_enabled(
            &settings,
            &sink,
            SamplesBuffer::new(1, 44_100, generate_samples(request.preset)),
        );
        assert!(sink.empty());

        settings.write().expect("无法更新测试音效配置").enabled = true;
        append_if_enabled(
            &settings,
            &sink,
            SamplesBuffer::new(1, 44_100, generate_samples(SoundPreset::CrispChime)),
        );
        assert!(!sink.empty());
    }

    #[test]
    fn built_in_presets_generate_non_empty_samples() {
        for preset in [SoundPreset::CrispChime, SoundPreset::BrightPing] {
            let samples = generate_samples(preset);
            assert!(!samples.is_empty());
            assert!(samples.iter().any(|sample| *sample != 0.0));
        }
    }

    #[test]
    fn disabled_preset_generates_no_samples() {
        assert!(generate_samples(SoundPreset::Disabled).is_empty());
    }

    #[test]
    fn presets_use_a_short_sine_wave_envelope() {
        let paste = generate_samples(SoundPreset::CrispChime);
        let double_ping = generate_samples(SoundPreset::BrightPing);

        assert_eq!(paste.len(), 4_410);
        assert_eq!(double_ping.len(), 7_277);
        assert_ne!(paste.len(), double_ping.len());
        assert!(paste[0].abs() < 0.001);
        assert!(double_ping.iter().any(|sample| sample.abs() > 0.14));
        assert!(
            double_ping
                .last()
                .is_some_and(|sample| sample.abs() < 0.002)
        );
    }

    #[test]
    fn external_sound_scan_uses_file_stems_and_ignores_other_files() {
        let directory = tempfile::tempdir().expect("无法创建临时音效目录");
        fs::write(directory.path().join("清脆.wav"), []).expect("无法创建 wav 测试文件");
        fs::write(directory.path().join("明亮.MP3"), []).expect("无法创建 mp3 测试文件");
        fs::write(directory.path().join("忽略.ogg"), []).expect("无法创建 ogg 测试文件");

        let presets = scan_external_presets(directory.path());
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].display_name(), "明亮");
        assert_eq!(presets[1].display_name(), "清脆");
    }
}

//! Audio input capture for recording.
//!
//! A [`Recorder`] opens the default input device and appends interleaved samples to a shared
//! buffer while armed. The callback only `try_lock`s; if the UI thread holds the lock the block is
//! kept in a small local backlog and appended next time, so nothing blocks the audio thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Captured audio: planar f32 at `sample_rate`.
#[derive(Debug, Clone, Default)]
pub struct Take {
    pub sample_rate: u32,
    pub channels: Vec<Vec<f32>>,
}

struct Shared {
    armed: AtomicBool,
    data: Mutex<Vec<f32>>,
}

pub struct Recorder {
    shared: Arc<Shared>,
    #[cfg(not(target_os = "freebsd"))]
    _stream: Option<cpal::Stream>,
    pub device_name: String,
    pub sample_rate: u32,
    pub channels: usize,
}

/// Cap: one hour of 8-channel 192 kHz audio; 64 Mi samples (256 MB of f32) on 32-bit targets
/// (wasm32, i686), where the 64-bit product doesn't fit in `usize`.
const MAX_SAMPLES: usize = if usize::BITS >= 64 { (3600u64 * 192_000 * 8) as usize } else { 64 << 20 };

impl Recorder {
    /// Open the default input device. Errors when there is none (the caller shows a message).
    #[cfg(not(target_os = "freebsd"))]
    pub fn open() -> Result<Recorder, String> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or("no audio input device")?;
        #[allow(deprecated)]
        let name = device.name().unwrap_or_else(|_| "Input".into());
        let cfg = device.default_input_config().map_err(|e| e.to_string())?;
        if cfg.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!("unsupported input sample format {:?}", cfg.sample_format()));
        }
        let config = cfg.config();
        let shared = Arc::new(Shared { armed: AtomicBool::new(false), data: Mutex::new(Vec::new()) });
        let sh = Arc::clone(&shared);
        let mut backlog: Vec<f32> = Vec::new();
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if !sh.armed.load(Ordering::Relaxed) {
                        backlog.clear();
                        return;
                    }
                    if let Ok(mut d) = sh.data.try_lock() {
                        if d.len() < MAX_SAMPLES {
                            d.extend_from_slice(&backlog);
                            d.extend_from_slice(data);
                        }
                        backlog.clear();
                    } else if backlog.len() < 1 << 20 {
                        backlog.extend_from_slice(data);
                    }
                },
                |e| log::warn!("input stream error: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Recorder { shared, _stream: Some(stream), device_name: name, sample_rate: config.sample_rate.0, channels: usize::from(config.channels).max(1) })
    }

    #[cfg(target_os = "freebsd")]
    pub fn open() -> Result<Recorder, String> {
        Err("recording is not supported on this platform yet".into())
    }

    /// Start capturing (clears any previous take).
    pub fn arm(&self) {
        self.shared.data.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.shared.armed.store(true, Ordering::Relaxed);
    }

    pub fn is_armed(&self) -> bool {
        self.shared.armed.load(Ordering::Relaxed)
    }

    /// Stop capturing and return the take.
    pub fn take(&self) -> Take {
        self.shared.armed.store(false, Ordering::Relaxed);
        let data = std::mem::take(&mut *self.shared.data.lock().unwrap_or_else(PoisonError::into_inner));
        deinterleave(&data, self.channels, self.sample_rate)
    }

    /// Samples captured so far (per channel).
    pub fn captured_frames(&self) -> usize {
        self.shared.data.try_lock().map_or(0, |d| d.len() / self.channels.max(1))
    }
}

pub fn deinterleave(data: &[f32], channels: usize, sample_rate: u32) -> Take {
    let ch = channels.max(1);
    let frames = data.len() / ch;
    let mut out = vec![Vec::with_capacity(frames); ch];
    for frame in data.chunks_exact(ch) {
        for (c, v) in frame.iter().enumerate() {
            if let Some(o) = out.get_mut(c) {
                o.push(*v);
            }
        }
    }
    Take { sample_rate, channels: out }
}

#[cfg(test)]
mod tests {
    #[test]
    fn deinterleaves() {
        let t = super::deinterleave(&[1.0, 2.0, 3.0, 4.0, 5.0], 2, 48_000);
        assert_eq!(t.channels, vec![vec![1.0, 3.0], vec![2.0, 4.0]]);
        let t = super::deinterleave(&[], 0, 48_000);
        assert_eq!(t.channels.len(), 1);
    }
}

//! System sound capture on Linux: records the monitor of the default output
//! through PulseAudio's simple API (PipeWire serves it too on current
//! distributions). Same API as `MacAudioCapture`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;

use super::{AudioError, BLOCK_SIZE, BlockBuilder, SAMPLE_RATE};
use crate::{Closed, Latest};

/// The monitor of whatever output is currently the default.
const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";
const CHANNELS: u8 = 2;

pub struct LinuxAudioCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// Bounded queue of audio blocks: a slow engine drops old blocks instead
    /// of accumulating them without limit.
    queue: Option<Arc<Latest<Vec<f32>>>>,
}

impl LinuxAudioCapture {
    pub fn new() -> Result<Self, AudioError> {
        Ok(Self {
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            queue: None,
        })
    }

    /// Recapture on the new default output: `@DEFAULT_MONITOR@` already
    /// follows the default sink, nothing to do.
    pub fn restart(&mut self) -> Result<(), AudioError> {
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        if self.thread.is_some() {
            return Ok(());
        }
        let queue = Latest::new();
        let (ready, ready_rx) = mpsc::sync_channel(1);
        self.stop.store(false, Ordering::Relaxed);
        let stop = Arc::clone(&self.stop);
        let thread_queue = Arc::clone(&queue);

        // The PulseAudio connection lives on its own thread: reads block.
        let thread = std::thread::spawn(move || {
            let spec = Spec {
                format: Format::F32le,
                channels: CHANNELS,
                rate: SAMPLE_RATE as u32,
            };
            let simple = match Simple::new(
                None,
                "LscSync",
                Direction::Record,
                Some(DEFAULT_MONITOR),
                "Music mode",
                &spec,
                None,
                None,
            ) {
                Ok(simple) => {
                    let _ = ready.send(Ok(()));
                    simple
                }
                Err(error) => {
                    // libpulse's PAErr::to_string returns an Option.
                    let message = error.to_string().unwrap_or_else(|| format!("{error:?}"));
                    let _ = ready.send(Err(message));
                    return;
                }
            };

            let mut builder = BlockBuilder::new(
                usize::from(CHANNELS),
                SAMPLE_RATE as u32,
                thread_queue.sender(),
            );
            let mut bytes = vec![0_u8; BLOCK_SIZE * usize::from(CHANNELS) * 4];
            let mut samples = Vec::with_capacity(BLOCK_SIZE * usize::from(CHANNELS));
            while !stop.load(Ordering::Relaxed) {
                if simple.read(&mut bytes).is_err() {
                    break;
                }
                samples.clear();
                let (frames, _) = bytes.as_chunks::<4>();
                samples.extend(frames.iter().map(|b| f32::from_le_bytes(*b)));
                builder.push(&samples);
            }
        });

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(AudioError::Capture(format!("PulseAudio: {error}")));
            }
            Err(_) => {
                self.stop.store(true, Ordering::Relaxed);
                return Err(AudioError::Capture("PulseAudio did not answer".to_owned()));
            }
        }
        self.thread = Some(thread);
        self.queue = Some(queue);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.queue = None;
    }

    /// `Ok(None)`: no block within `timeout`. Only a closed queue is an error.
    pub fn read_block_timeout(&self, timeout: Duration) -> Result<Option<Vec<f32>>, AudioError> {
        let queue = self
            .queue
            .as_ref()
            .ok_or_else(|| AudioError::Capture("audio capture not started".to_owned()))?;
        match queue.recv_timeout(timeout) {
            Ok(block) => Ok(block),
            Err(Closed) => Err(AudioError::Capture("audio stream interrupted".to_owned())),
        }
    }
}

impl Drop for LinuxAudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

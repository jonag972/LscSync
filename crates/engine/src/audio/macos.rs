//! System sound capture through ScreenCaptureKit, with pure objc2 bindings
//! (no Swift).
//!
//! Implements the `SCStreamOutput` protocol in Rust with `define_class!`,
//! extracts Float32 PCM from each `CMSampleBuffer` and pushes it to a channel.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use block2::RcBlock;
use objc2::define_class;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{ClassType, DefinedClass, msg_send};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSError, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType,
};

use super::AudioError;
use crate::{Closed, Latest, LatestSender};

// ---------------------------------------------------------------------------
// ObjC handler receiving the audio buffers
// ---------------------------------------------------------------------------

/// State of the stream output handler: the bounded queue towards the engine,
/// plus a scratch buffer reused for the PCM copy so the real-time callback
/// does not allocate one `Vec` per sample buffer.
pub struct AudioOutputIvars {
    sender: LatestSender<Vec<f32>>,
    scratch: Mutex<Vec<u8>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = AudioOutputIvars]
    pub struct AudioOutputHandler;

    unsafe impl NSObjectProtocol for AudioOutputHandler {}

    unsafe impl SCStreamOutput for AudioOutputHandler {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn did_output_sample_buffer(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            output_type: SCStreamOutputType,
        ) {
            if output_type == SCStreamOutputType::Audio {
                let ivars = self.ivars();
                let samples = {
                    let mut scratch = ivars.scratch.lock().unwrap();
                    unsafe { extract_mono(sample_buffer, &mut scratch) }
                };
                if !samples.is_empty() {
                    // Never blocks; drops the oldest block if the engine lags.
                    ivars.sender.send(samples);
                }
            }
        }
    }
);

// Exposes `alloc` to Rust (`define_class!` does not generate it).
impl AudioOutputHandler {
    objc2::extern_methods!(
        #[unsafe(method(alloc))]
        pub unsafe fn alloc() -> objc2::rc::Allocated<Self>;
    );
}

// ---------------------------------------------------------------------------
// PCM extraction from CMSampleBuffer
// ---------------------------------------------------------------------------

/// Copies the sample buffer's PCM into `scratch` (reused between calls) and
/// mixes the stereo pairs down to mono.
unsafe fn extract_mono(sample_buffer: &CMSampleBuffer, scratch: &mut Vec<u8>) -> Vec<f32> {
    let block_buffer = unsafe { sample_buffer.data_buffer() };
    let Some(block_buffer) = block_buffer else {
        return Vec::new();
    };

    let length = unsafe { block_buffer.data_length() };
    if length == 0 || length % 8 != 0 {
        return Vec::new();
    }

    scratch.clear();
    scratch.resize(length, 0);
    let dest = NonNull::<c_void>::new(scratch.as_mut_ptr().cast()).expect("non-null pointer");
    let status = unsafe { block_buffer.copy_data_bytes(0, length, dest) };
    if status != 0 {
        return Vec::new();
    }

    let (chunks, _) = scratch.as_chunks::<8>();
    chunks
        .iter()
        .map(|chunk| {
            let left = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            let right = f32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
            (left + right) * 0.5
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Audio capture
// ---------------------------------------------------------------------------

/// System audio capture (`SCStreamConfiguration.capturesAudio`) needs macOS 13.
pub fn system_audio_available() -> bool {
    use objc2_foundation::{NSOperatingSystemVersion, NSProcessInfo};
    NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
        majorVersion: 13,
        minorVersion: 0,
        patchVersion: 0,
    })
}

pub struct MacAudioCapture {
    stream: Option<Retained<SCStream>>,
    /// SCStream only keeps a weak reference to its outputs: without this handle
    /// the handler (and the queue sender) would be freed right away.
    handler: Option<Retained<ProtocolObject<dyn SCStreamOutput>>>,
    /// Bounded queue of audio blocks: a slow engine drops old blocks instead
    /// of accumulating them without limit.
    queue: Option<Arc<Latest<Vec<f32>>>>,
}

impl MacAudioCapture {
    pub fn new() -> Result<Self, AudioError> {
        Ok(Self {
            stream: None,
            handler: None,
            queue: None,
        })
    }

    pub fn start(&mut self) -> Result<(), AudioError> {
        if self.stream.is_some() {
            return Ok(());
        }

        // 1. Get the first display (async -> synchronous channel).
        let content = unsafe { Self::get_shareable_content()? };
        let displays: Retained<NSArray<SCDisplay>> = unsafe { content.displays() };
        let Some(display) = displays.firstObject() else {
            return Err(AudioError::NoDisplay);
        };

        // 2. Filter: the whole display.
        let filter: Retained<SCContentFilter> = unsafe {
            let allocated: *mut SCContentFilter = msg_send![SCContentFilter::class(), alloc];
            let ptr: *mut SCContentFilter = msg_send![
                allocated,
                initWithDisplay: &*display,
                excludingWindows: &*NSArray::<objc2_screen_capture_kit::SCWindow>::new()
            ];
            Retained::from_raw(ptr).ok_or(AudioError::Capture("filter init failed".into()))?
        };

        // 3. Configuration: 48 kHz stereo audio, minimal video.
        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setWidth(2);
            config.setHeight(2);
            config.setCapturesAudio(true);
            config.setSampleRate(48000);
            config.setChannelCount(2);
        }

        // 4. Handler + bounded queue towards the engine.
        let queue = Latest::new();
        let handler: Retained<AudioOutputHandler> = {
            let allocated = unsafe { AudioOutputHandler::alloc() }.set_ivars(AudioOutputIvars {
                sender: queue.sender(),
                scratch: Mutex::new(Vec::new()),
            });
            unsafe { msg_send![super(allocated), init] }
        };

        // 5. Create the stream.
        let stream: Retained<SCStream> = unsafe {
            let allocated: *mut SCStream = msg_send![SCStream::class(), alloc];
            let ptr: *mut SCStream = msg_send![
                allocated,
                initWithFilter: &*filter,
                configuration: &*config,
                delegate: Option::<&NSObject>::None
            ];
            Retained::from_raw(ptr).ok_or(AudioError::Capture("stream init failed".into()))?
        };

        // 6. Register the handler.
        let proto = ProtocolObject::from_retained(handler);
        unsafe {
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    &proto,
                    SCStreamOutputType::Audio,
                    None,
                )
                .map_err(|e| AudioError::Capture(e.to_string()))?;
        }

        // 7. Start.
        unsafe {
            let (tx, rx) = mpsc::channel::<Result<(), String>>();
            let block = RcBlock::new(move |error: *mut NSError| {
                if error.is_null() {
                    let _ = tx.send(Ok(()));
                } else {
                    let desc = (*error).localizedDescription();
                    let _ = tx.send(Err(desc.to_string()));
                }
            });
            stream.startCaptureWithCompletionHandler(Some(&block));
            rx.recv_timeout(Duration::from_secs(5))
                .map_err(|_| AudioError::Capture("start timed out".into()))?
                .map_err(AudioError::Capture)?;
        }

        self.stream = Some(stream);
        self.handler = Some(proto);
        self.queue = Some(queue);
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            unsafe {
                let (tx, rx) = mpsc::channel::<()>();
                let block = RcBlock::new(move |_error: *mut NSError| {
                    let _ = tx.send(());
                });
                stream.stopCaptureWithCompletionHandler(Some(&block));
                let _ = rx.recv_timeout(Duration::from_secs(2));
            }
        }
        self.handler = None;
        self.queue = None;
    }

    /// `Ok(None)`: no block within `timeout`. Not a failure: the caller feeds
    /// silence instead. Only a closed queue is an error.
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

    unsafe fn get_shareable_content() -> Result<Retained<SCShareableContent>, AudioError> {
        let (tx, rx) = mpsc::channel::<Result<Retained<SCShareableContent>, String>>();

        let block = RcBlock::new(
            move |content: *mut SCShareableContent, error: *mut NSError| {
                if !content.is_null() {
                    if let Some(retained) = unsafe { Retained::retain(content) } {
                        let _ = tx.send(Ok(retained));
                    } else {
                        let _ = tx.send(Err("retain failed".to_owned()));
                    }
                } else if !error.is_null() {
                    let desc = unsafe { (*error).localizedDescription() };
                    let _ = tx.send(Err(desc.to_string()));
                } else {
                    let _ = tx.send(Err("empty response".to_owned()));
                }
            },
        );

        unsafe {
            SCShareableContent::getShareableContentWithCompletionHandler(&block);
        }

        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| AudioError::Capture("display enumeration timed out".into()))?
            .map_err(AudioError::Capture)
    }
}

impl Drop for MacAudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

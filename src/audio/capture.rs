//! Reading the adapter's cable.
//!
//! Whatever is plugged into the cassette adapter arrives here as raw f32
//! stereo and goes into a ring, from which the decoder feeds it to the same
//! DSP chain a disc uses. From the equaliser onward there is no difference
//! between Spotify and a FLAC on disk, which is the entire point of the
//! adapter.
//!
//! Capture is `pw-record --raw`, a subprocess, for the same reason the sink is
//! `pactl`: it is the tool that already exists. `parec` would have been the
//! obvious alternative and was tried first — under pipewire-pulse it exits
//! successfully having written nothing at all, which is a memorable way to
//! spend an afternoon.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ringbuf::traits::Producer;

/// Shared with the UI so the panel can say whether the cable is actually
/// carrying anything.
pub struct CaptureState {
    running: AtomicBool,
    /// Set when the capture process could not be started at all. Read by the
    /// retry loop rather than the UI: the panel already says "not carrying"
    /// when `running` is false, and why is not something it can act on.
    failed: AtomicBool,
}

impl CaptureState {
    fn new() -> Self {
        CaptureState { running: AtomicBool::new(false), failed: AtomicBool::new(false) }
    }
    pub fn running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

pub struct Capture {
    pub state: Arc<CaptureState>,
    stop: Arc<AtomicBool>,
}

impl Capture {
    /// Start reading `source` at `rate`, pushing interleaved stereo into
    /// `prod`. Returns immediately; the reading happens on its own thread.
    pub fn start<P>(source: &str, rate: u32, mut prod: P) -> Self
    where
        P: Producer<Item = f32> + Send + 'static,
    {
        let state = Arc::new(CaptureState::new());
        let stop = Arc::new(AtomicBool::new(false));
        let (t_state, t_stop, src) = (state.clone(), stop.clone(), source.to_string());

        std::thread::Builder::new()
            .name("ten-qd/capture".into())
            .spawn(move || {
                // Retry, because the monitor may not be ready the instant the
                // sink appears, and because a plugged stream ending can take
                // the capture down with it.
                while !t_stop.load(Ordering::Relaxed) {
                    match spawn(&src, rate) {
                        Ok(child) => {
                            t_state.failed.store(false, Ordering::Relaxed);
                            pump(child, &mut prod, &t_state, &t_stop);
                        }
                        Err(_) => {
                            t_state.failed.store(true, Ordering::Relaxed);
                            t_state.running.store(false, Ordering::Relaxed);
                        }
                    }
                    if t_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(400));
                }
                t_state.running.store(false, Ordering::Relaxed);
            })
            .ok();

        Capture { state, stop }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn spawn(source: &str, rate: u32) -> std::io::Result<Child> {
    // Name the node, so the graph reads correctly in qpwgraph and friends:
    // the adapter sink, its monitor, this capture, and our output should all
    // be identifiable as one device rather than as a stray `pw-record`
    // sitting next to an unrelated `alsa_playback.ten-qd`.
    Command::new("pw-record")
        .args([
            "--raw",
            &format!("--target={source}"),
            &format!("--rate={rate}"),
            "--channels=2",
            "--format=f32",
            "-P",
            "{ node.name = \"ten-qd adapter capture\"                 media.name = \"cassette adapter\"                 application.name = \"ten-qd\" }",
            "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

fn pump<P>(mut child: Child, prod: &mut P, state: &CaptureState, stop: &AtomicBool)
where
    P: Producer<Item = f32>,
{
    let Some(mut out) = child.stdout.take() else { return };
    // 16 KiB is 1024 stereo frames — about 21 ms at 48 kHz, short enough that
    // stopping feels immediate.
    let mut bytes = [0u8; 16384];
    let mut samples: Vec<f32> = Vec::with_capacity(4096);

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match out.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                state.running.store(true, Ordering::Relaxed);
                samples.clear();
                // A short read can split a sample across two reads; the
                // remainder is dropped rather than carried, which costs at
                // most three bytes and keeps this loop free of state.
                for c in bytes[..n].chunks_exact(4) {
                    samples.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
                // A full ring means nobody is draining — the adapter is not
                // the selected source. Dropping is right for a live input.
                prod.push_slice(&samples);
            }
            Err(_) => break,
        }
    }

    state.running.store(false, Ordering::Relaxed);
    let _ = child.kill();
    let _ = child.wait();
}

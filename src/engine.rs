//! Local zh->en translation on a dedicated worker thread.
//!
//! The worker owns the model: it loads it (so the UI and hotkey come up
//! immediately), runs one warm-up inference (the first call is ~70ms slower
//! than the rest), then serves `(revision, text)` jobs. The queue is drained
//! before and after each inference so fast typing never builds a backlog and
//! stale results are never published.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::thread;

use anyhow::{Context, Result};
use ct2rs::{Config, Translator};

pub struct Engine {
    translator: Translator<ct2rs::tokenizers::auto::Tokenizer>,
    opts: ct2rs::TranslationOptions<String, String>,
}

impl Engine {
    pub fn load(model_dir: &Path) -> Result<Self> {
        // FASTRANS_THREADS overrides the intra-op thread count (0 = CT2 default,
        // which is 4). On hybrid CPUs matching the P-core count can help.
        let threads = std::env::var("FASTRANS_THREADS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let config = Config {
            num_threads_per_replica: threads,
            ..Config::default()
        };
        let translator = Translator::new(model_dir, &config)
            .with_context(|| format!("failed to load model from {}", model_dir.display()))?;
        // Greedy decoding: ~2x faster than the default beam search, and for an
        // input-bar use case the quality difference is negligible.
        let opts = ct2rs::TranslationOptions {
            beam_size: 1,
            ..Default::default()
        };
        Ok(Self { translator, opts })
    }

    pub fn translate(&self, zh: &str) -> Result<String> {
        let res = self.translator.translate_batch(&[zh], &self.opts, None)?;
        Ok(res
            .into_iter()
            .next()
            .map(|(text, _)| text)
            .unwrap_or_default())
    }
}

pub struct Job {
    pub rev: u64,
    pub text: String,
}

pub struct TransResult {
    pub rev: u64,
    pub text: String,
    /// Engine failure (model load or inference). The UI shows it and must not
    /// commit.
    pub error: Option<String>,
}

/// Spawns the worker thread, which loads the model itself. `on_result` is
/// called from the worker after each result (used to wake the UI).
pub fn spawn_worker(
    model_dir: PathBuf,
    on_result: impl Fn() + Send + 'static,
) -> (Sender<Job>, Receiver<TransResult>) {
    let (job_tx, job_rx) = channel::<Job>();
    let (res_tx, res_rx) = channel::<TransResult>();
    thread::spawn(move || {
        let engine = match Engine::load(&model_dir) {
            Ok(e) => e,
            Err(e) => {
                let _ = res_tx.send(TransResult {
                    rev: 0,
                    text: String::new(),
                    error: Some(format!("{e:#}")),
                });
                on_result();
                return;
            }
        };
        let _ = engine.translate("你好"); // warm-up

        let mut pending: Option<Job> = None;
        loop {
            let mut job = match pending.take() {
                Some(j) => j,
                None => match job_rx.recv() {
                    Ok(j) => j,
                    Err(_) => return,
                },
            };
            // Drain: only the newest revision matters.
            loop {
                match job_rx.try_recv() {
                    Ok(newer) => job = newer,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            let result = if job.text.trim().is_empty() {
                Ok(String::new())
            } else {
                engine.translate(&job.text)
            };
            // A newer job may have arrived during inference; skip publishing
            // the stale result and go translate the new text instead.
            match job_rx.try_recv() {
                Ok(newer) => {
                    pending = Some(newer);
                    continue;
                }
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {}
            }
            let (text, error) = match result {
                Ok(t) => (t, None),
                Err(e) => (String::new(), Some(format!("{e:#}"))),
            };
            let _ = res_tx.send(TransResult {
                rev: job.rev,
                text,
                error,
            });
            on_result();
        }
    });
    (job_tx, res_rx)
}

/// Model directory resolution: FASTRANS_MODEL env var, then ./models/opus-mt-zh-en
/// next to the executable, then the current directory variant.
pub fn find_model_dir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("FASTRANS_MODEL") {
        let p = std::path::PathBuf::from(p);
        if p.join("model.bin").exists() {
            return Some(p);
        }
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("models/opus-mt-zh-en"));
        }
    }
    candidates.push(std::path::PathBuf::from("models/opus-mt-zh-en"));
    candidates.into_iter().find(|p| p.join("model.bin").exists())
}

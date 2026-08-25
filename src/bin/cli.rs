//! Headless check for the translation engine:
//! `fastrans-cli <model_dir>` then type Chinese lines on stdin.

use std::io::{BufRead, Write};
use std::time::Instant;

use fastrans::engine::Engine;

fn main() -> anyhow::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "models/opus-mt-zh-en".into());
    eprintln!("loading {dir} ...");
    let t0 = Instant::now();
    let engine = Engine::load(std::path::Path::new(&dir))?;
    eprintln!("loaded in {:?}", t0.elapsed());

    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let t = Instant::now();
        let en = fastrans::style::polish(&engine.translate(&line)?);
        writeln!(out, "[{:>6.1?}] {en}", t.elapsed())?;
    }
    Ok(())
}

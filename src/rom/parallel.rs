//! Multi-threaded decoding of any random-access source ([`ReadAt`]: `ChdReader`, `DiscReader`,
//! a file, ...): `threads` workers, one reader each, decode fixed-size blocks claimed from a
//! shared counter. The result is delivered either
//! - [`Delivery::InOrder`]: to a callback strictly in order -- a sequential stream (hashable,
//!   pipeable) with parallel decoding behind it; at most `2 * threads` blocks wait in memory;
//! - [`Delivery::ToFile`]: written straight to its offset in a file, in whatever order the
//!   workers finish (random writes, no reordering);
//! - [`Delivery::Unordered`]: handed to a callback on the worker threads as each block
//!   finishes.
//!
//! Thread count is the caller's choice with no cap; `native_lib::recommended_reader_threads`
//! gives one per logical CPU, `rom::decode_threads` a per-container recommendation.
//!
//! [`extract_and_hash_to_file`] / [`hash_while_decoding`] are the DEFAULT way to get a
//! decoded file and its hash: `tests/results/benchmark_streaming_hash_strategy_*.md`
//! measured them against extracting to a file first and hashing it in a second, separate
//! pass, for RVZ (2T/4T/20T, tmpfs and SSD) and CHD DVD/raw (16T, tmpfs and SSD) -- the
//! fused single pass won every single case, by 1.3x (CHD DVD/raw, tmpfs) up to 23x (RVZ,
//! SSD). Extract-then-hash is not a legitimate alternative on that evidence; only use a
//! separate [`parallel_decode`] + [`Delivery::ToFile`] call (no hashing) when a hash
//! genuinely isn't needed.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

use super::hash::HashDigest;
use super::source::ReadAt;

pub enum Delivery<'a> {
    /// `(offset, bytes)` in increasing, contiguous order; return `false` to stop.
    InOrder(&'a mut dyn FnMut(u64, &[u8]) -> bool),
    /// Written at each block's offset; the file is created (truncated) with the full size.
    ToFile(&'a Path),
    /// `(offset, bytes)` on the worker threads, in completion order (no reordering, no
    /// buffering); return `false` to stop.
    Unordered(&'a (dyn Fn(u64, &[u8]) -> bool + Sync)),
}

/// Decodes `size` bytes from sources made by `open` (called once per worker) in `block`-byte
/// blocks on `threads` workers. `Ok(false)` if an `InOrder` callback stopped it.
pub fn parallel_decode<R, O>(open: O, size: u64, block: usize, threads: usize, delivery: Delivery) -> io::Result<bool>
where
    R: ReadAt,
    O: Fn() -> io::Result<R> + Sync,
{
    let threads = threads.max(1);
    let block = block.max(1) as u64;
    let blocks = size.div_ceil(block);
    let next = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let failure: Mutex<Option<io::Error>> = Mutex::new(None);
    let fail = |err: io::Error| {
        failure.lock().unwrap().get_or_insert(err);
        stop.store(true, Ordering::Relaxed);
    };
    let range = |index: u64| {
        let offset = index * block;
        (offset, (size - offset).min(block) as usize)
    };

    match delivery {
        Delivery::ToFile(path) => {
            let file = std::fs::File::create(path)?;
            file.set_len(size)?;
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| {
                        let mut reader = match open() {
                            Ok(r) => r,
                            Err(err) => return fail(err),
                        };
                        let mut buf = vec![0u8; block as usize];
                        while !stop.load(Ordering::Relaxed) {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            if index >= blocks {
                                break;
                            }
                            let (offset, len) = range(index);
                            if let Err(err) = reader.read_at(offset, &mut buf[..len]).and_then(|_| write_at(&file, &buf[..len], offset)) {
                                return fail(err);
                            }
                        }
                    });
                }
            });
        }
        Delivery::Unordered(on_block) => {
            let stopped = AtomicBool::new(false);
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| {
                        let mut reader = match open() {
                            Ok(r) => r,
                            Err(err) => return fail(err),
                        };
                        let mut buf = vec![0u8; block as usize];
                        while !stop.load(Ordering::Relaxed) {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            if index >= blocks {
                                break;
                            }
                            let (offset, len) = range(index);
                            if let Err(err) = reader.read_at(offset, &mut buf[..len]) {
                                return fail(err);
                            }
                            if !on_block(offset, &buf[..len]) {
                                stopped.store(true, Ordering::Relaxed);
                                stop.store(true, Ordering::Relaxed);
                            }
                        }
                    });
                }
            });
            if let Some(err) = failure.into_inner().unwrap() {
                return Err(err);
            }
            return Ok(!stopped.load(Ordering::Relaxed));
        }
        Delivery::InOrder(on_block) => {
            // Workers may run at most `window` blocks ahead of the consumer.
            let window = 2 * threads as u64;
            let emitted = (Mutex::new(0u64), Condvar::new());
            let (tx, rx) = std::sync::mpsc::channel::<(u64, Vec<u8>)>();
            let mut delivered = 0u64;
            let mut stopped_by_callback = false;
            std::thread::scope(|s| {
                for _ in 0..threads {
                    let tx = tx.clone();
                    let (emitted, next, stop) = (&emitted, &next, &stop);
                    let (open, fail, range) = (&open, &fail, &range);
                    // A failure stops the others, and the ones parked on the window must be
                    // woken to see it: the block they wait on will never be delivered, and
                    // each holds a sender, so the consumer would wait on the channel forever.
                    let fail_and_wake = move |err: io::Error| {
                        fail(err);
                        let _guard = emitted.0.lock().unwrap();
                        emitted.1.notify_all();
                    };
                    s.spawn(move || {
                        let mut reader = match open() {
                            Ok(r) => r,
                            Err(err) => return fail_and_wake(err),
                        };
                        while !stop.load(Ordering::Relaxed) {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            if index >= blocks {
                                break;
                            }
                            let (lock, cv) = emitted;
                            drop(
                                cv.wait_while(lock.lock().unwrap(), |done| index >= *done + window && !stop.load(Ordering::Relaxed))
                                    .unwrap(),
                            );
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            let (offset, len) = range(index);
                            let mut buf = vec![0u8; len];
                            if let Err(err) = reader.read_at(offset, &mut buf) {
                                return fail_and_wake(err);
                            }
                            if tx.send((index, buf)).is_err() {
                                break;
                            }
                        }
                    });
                }
                drop(tx);

                let mut pending: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
                'consume: while delivered < blocks {
                    let Ok((index, buf)) = rx.recv() else { break };
                    pending.insert(index, buf);
                    while let Some(buf) = pending.remove(&delivered) {
                        if !on_block(delivered * block, &buf) {
                            stopped_by_callback = true;
                            break 'consume;
                        }
                        delivered += 1;
                        let (lock, cv) = &emitted;
                        *lock.lock().unwrap() = delivered;
                        cv.notify_all();
                    }
                }
                // Release any worker waiting on the window, then let the scope join them.
                stop.store(true, Ordering::Relaxed);
                {
                    let _guard = emitted.0.lock().unwrap();
                    emitted.1.notify_all();
                }
                drop(rx);
            });
            if let Some(err) = failure.into_inner().unwrap() {
                return Err(err);
            }
            if stopped_by_callback {
                return Ok(false);
            }
            if delivered < blocks {
                return Err(io::Error::other("decode ended early"));
            }
            return Ok(true);
        }
    }
    match failure.into_inner().unwrap() {
        Some(err) => Err(err),
        None => Ok(true),
    }
}

/// The default, recommended way to extract a container to `out` while also hashing its
/// content: one [`parallel_decode`] pass, strictly in order, that both writes each block to
/// `out` and feeds it to `digest` as it arrives -- not a separate extract-then-hash pass.
/// See this module's doc comment for the benchmark evidence.
///
/// `digest` is not finished; call `.finish()` on it once this returns to get the
/// [`super::hash::HashReport`] (so a caller can pick any [`super::hash::HashFlags`], not
/// just one). Returns `Ok(false)` only if writing to `out` or updating `digest` ever
/// failed after the first block (unlike [`parallel_decode`]'s other callers, nothing here
/// can request a stop on purpose, so `false` should not happen in practice; treated as a
/// decode failure).
pub fn extract_and_hash_to_file<R, O>(
    open: O,
    size: u64,
    block: usize,
    threads: usize,
    out: &Path,
    digest: &HashDigest,
) -> io::Result<bool>
where
    R: ReadAt,
    O: Fn() -> io::Result<R> + Sync,
{
    use std::io::Write;
    let mut file = std::io::BufWriter::with_capacity(4 << 20, std::fs::File::create(out)?);
    let mut write_err = None;
    let mut on_block = |_: u64, bytes: &[u8]| {
        if let Err(err) = file.write_all(bytes) {
            write_err = Some(err);
            return false;
        }
        digest.update(bytes).is_ok()
    };
    let ok = parallel_decode(open, size, block, threads, Delivery::InOrder(&mut on_block))?;
    if let Some(err) = write_err {
        return Err(err);
    }
    file.flush()?;
    Ok(ok)
}

/// [`extract_and_hash_to_file`] without writing anything to disk -- pure decode + hash, for
/// when only the hash is needed (e.g. verifying content without keeping a copy).
pub fn hash_while_decoding<R, O>(open: O, size: u64, block: usize, threads: usize, digest: &HashDigest) -> io::Result<bool>
where
    R: ReadAt,
    O: Fn() -> io::Result<R> + Sync,
{
    let mut on_block = |_: u64, bytes: &[u8]| digest.update(bytes).is_ok();
    parallel_decode(open, size, block, threads, Delivery::InOrder(&mut on_block))
}

fn write_at(file: &std::fs::File, buf: &[u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::write_all_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        let mut done = 0;
        while done < buf.len() {
            done += std::os::windows::fs::FileExt::seek_write(file, &buf[done..], offset + done as u64)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fails at one offset; every other block is read after a pause, so the workers run
    /// ahead of a slow consumer and park on the window.
    struct FailsAt(u64);

    impl ReadAt for FailsAt {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
            if offset == self.0 {
                return Err(io::Error::other("bad hunk"));
            }
            buf.fill(1);
            Ok(())
        }
    }

    #[test]
    fn a_read_error_ends_an_in_order_decode_instead_of_hanging_it() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut slow = |_: u64, _: &[u8]| {
                std::thread::sleep(std::time::Duration::from_millis(20));
                true
            };
            let result = parallel_decode(|| Ok(FailsAt(40 * 64)), 400 * 64, 64, 4, Delivery::InOrder(&mut slow));
            let _ = tx.send(result.map_err(|e| e.to_string()));
        });
        let result = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("the decode hung");
        assert_eq!(result.unwrap_err(), "bad hunk");
    }
}

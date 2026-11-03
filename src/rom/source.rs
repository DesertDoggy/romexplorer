//! Inputs for archive creation that come from anywhere, not just files: a [`Source`] is a
//! name, an exact size and either sequential ([`Read`]) or random ([`ReadAt`]) access. The
//! streaming builds of all three native libraries take them (see
//! `sevenzip_wrapper::create_archive_from_sources`, `chdman_wrapper::create_from_sources`,
//! `dolphin_wrapper::convert_from_source`), so an archive can be re-archived straight from
//! another one's decoded bytes -- and hashed on the way ([`Source::hashed`]).
//!
//! Where the bytes come from:
//! - a file ([`Source::file`], random access);
//! - a reader over another archive: `sevenzip_wrapper::EntryReader` (sequential, or emulated
//!   random), `chdman_wrapper::ChdReader`, `dolphin_wrapper::DiscReader` (random);
//! - any push-style `*_stream` callback, through [`pipe`] (sequential).

use std::io::{self, Read};
use std::path::Path;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use super::hash::{HashDigest, HashFlags, HashReport};

/// How an archive's contents are read or fed: front to back, or at arbitrary offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessMode {
    Sequential,
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Container {
    Zip,
    SevenZip,
    Chd,
    Rvz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Extract,
    Archive,
    /// Reading disc metadata (signature, title, serial) out of the container.
    Metadata,
}

/// The default access mode per container and operation, from the benchmarks in
/// `tests/src/bench_streaming.rs` (results in `tests/results/benchmark_streaming_*.md`). Where
/// the default is random, use one reader/thread per logical CPU
/// (`native_lib::recommended_reader_threads`), no cap.
/// - zip/7z: sequential everywhere -- their data is one compressed stream, so random reads
///   are emulated by decoding from the start (O(offset)).
/// - CHD metadata: random -- only the sectors the platform method needs are decoded
///   (sub-millisecond to tens of ms instead of seconds; see `detect::chd_disc`).
/// - CHD extract: random -- parallel decode (`rom::parallel`) runs ~4x chdman's single-threaded
///   extract at 20 threads, and in-order delivery (hashable) costs almost nothing extra.
///   (A CD's cue/bin layout still comes from chdman itself; parallel decode yields the logical
///   image, i.e. the ISO for DVD/raw CHDs.)
/// - CHD archive: sequential -- chdman reads its input front to back and compresses on its own
///   threads; a random source doesn't make it faster.
/// - RVZ extract: random -- parallel decode scales far past Dolphin's own loop.
/// - RVZ archive: random -- Dolphin's writer reads out of order; a sequential source must be
///   spooled first (~1/3 the speed).
pub fn default_access(container: Container, operation: Operation) -> AccessMode {
    match (container, operation) {
        (Container::Zip | Container::SevenZip, _) => AccessMode::Sequential,
        (Container::Chd, Operation::Archive) => AccessMode::Sequential,
        (Container::Chd, _) | (Container::Rvz, _) => AccessMode::Random,
    }
}

/// Random access: fill `buf` with the bytes at `offset` (all of them, or an error).
pub trait ReadAt: Send {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

impl<T: ReadAt + ?Sized> ReadAt for Box<T> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        (**self).read_at(offset, buf)
    }
}

impl ReadAt for std::fs::File {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
        }
        #[cfg(windows)]
        {
            let mut done = 0;
            while done < buf.len() {
                let n = std::os::windows::fs::FileExt::seek_read(self, &mut buf[done..], offset + done as u64)?;
                if n == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                done += n;
            }
            Ok(())
        }
    }
}

/// How a [`Source`]'s bytes are read.
pub enum Access {
    Sequential(Box<dyn Read + Send>),
    Random(Box<dyn ReadAt>),
}

/// One input: `name` is what the consumer calls it (the member name in a zip/7z, the file
/// name a cue sheet refers to, the `-i` value for chdman), `size` its exact length.
pub struct Source {
    pub name: String,
    pub size: u64,
    pub access: Access,
}

impl Source {
    pub fn sequential(name: impl Into<String>, size: u64, reader: impl Read + Send + 'static) -> Self {
        Self { name: name.into(), size, access: Access::Sequential(Box::new(reader)) }
    }

    pub fn random(name: impl Into<String>, size: u64, reader: impl ReadAt + 'static) -> Self {
        Self { name: name.into(), size, access: Access::Random(Box::new(reader)) }
    }

    /// A file, with random access.
    pub fn file(path: &Path, name: impl Into<String>) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        Ok(Self::random(name, size, file))
    }

    pub fn is_random(&self) -> bool {
        matches!(self.access, Access::Random(_))
    }

    /// Hashes the bytes as the consumer takes them. Sequential sources hash everything read;
    /// random sources hash the reads that continue in order and ignore re-reads, so a
    /// consumer that reads front to back (zip/7z, chdman) yields a complete hash. Returns
    /// the source to hand on, and the handle to collect the hash from afterwards.
    pub fn hashed(self, flags: HashFlags) -> (Self, SourceHash) {
        let size = self.size;
        let state = Arc::new(Mutex::new(InOrder { digest: Some(HashDigest::new(flags)), cursor: 0, gap: false }));
        let access = match self.access {
            Access::Sequential(inner) => Access::Sequential(Box::new(HashingRead { inner, state: state.clone() })),
            Access::Random(inner) => Access::Random(Box::new(HashingReadAt { inner, state: state.clone() })),
        };
        (Self { access, ..self }, SourceHash { state, size })
    }

    /// Counts every byte the consumer reads into `counter` (added, not stored), for progress
    /// reporting on the consumer side of an archive creation. Re-reads of a random source
    /// count again -- it measures work done, not distinct bytes.
    pub fn counted(self, counter: Arc<std::sync::atomic::AtomicU64>) -> Self {
        let access = match self.access {
            Access::Sequential(inner) => Access::Sequential(Box::new(CountingRead { inner, counter })),
            Access::Random(inner) => Access::Random(Box::new(CountingReadAt { inner, counter })),
        };
        Self { access, ..self }
    }

    /// Sequential read of up to `buf.len()` bytes (for C trampolines). Random sources are
    /// read through their own cursor.
    pub(crate) fn read_next(&mut self, cursor: &mut u64, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.access {
            Access::Sequential(r) => {
                let n = r.read(buf)?;
                *cursor += n as u64;
                Ok(n)
            }
            Access::Random(r) => {
                let n = buf.len().min((self.size - *cursor) as usize);
                r.read_at(*cursor, &mut buf[..n])?;
                *cursor += n as u64;
                Ok(n)
            }
        }
    }
}

struct InOrder {
    digest: Option<HashDigest>,
    cursor: u64,
    gap: bool,
}

impl InOrder {
    fn observe(&mut self, offset: u64, data: &[u8]) {
        let end = offset + data.len() as u64;
        if end <= self.cursor {
            return;
        }
        if offset > self.cursor {
            self.gap = true;
            return;
        }
        let tail = &data[(self.cursor - offset) as usize..];
        if let Some(digest) = &self.digest {
            let _ = digest.update(tail);
        }
        self.cursor = end;
    }
}

struct HashingRead {
    inner: Box<dyn Read + Send>,
    state: Arc<Mutex<InOrder>>,
}

impl Read for HashingRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let mut state = self.state.lock().unwrap();
        let at = state.cursor;
        state.observe(at, &buf[..n]);
        Ok(n)
    }
}

struct HashingReadAt {
    inner: Box<dyn ReadAt>,
    state: Arc<Mutex<InOrder>>,
}

impl ReadAt for HashingReadAt {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)?;
        self.state.lock().unwrap().observe(offset, buf);
        Ok(())
    }
}

struct CountingRead {
    inner: Box<dyn Read + Send>,
    counter: Arc<std::sync::atomic::AtomicU64>,
}

impl Read for CountingRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.counter.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
}

struct CountingReadAt {
    inner: Box<dyn ReadAt>,
    counter: Arc<std::sync::atomic::AtomicU64>,
}

impl ReadAt for CountingReadAt {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)?;
        self.counter.fetch_add(buf.len() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// Collects a [`Source::hashed`] source's hash once the consumer is done with it.
pub struct SourceHash {
    state: Arc<Mutex<InOrder>>,
    size: u64,
}

impl SourceHash {
    /// The hash of all `size` bytes, or `None` if the consumer skipped any (read out of
    /// order past the in-order cursor) or stopped before the end.
    pub fn finish(self) -> Option<HashReport> {
        let mut state = self.state.lock().unwrap();
        let complete = !state.gap && state.cursor == self.size;
        let report = state.digest.take()?.finish();
        complete.then_some(report)
    }
}

/// A bounded in-memory pipe: a push-style producer (e.g. an `extract_stream` callback,
/// running on its own thread) writes, a consumer reads it as a sequential [`Source`].
/// `capacity_chunks` bounds how many written chunks may wait unread.
pub fn pipe(capacity_chunks: usize) -> (PipeWriter, PipeReader) {
    let (tx, rx) = std::sync::mpsc::sync_channel(capacity_chunks.max(1));
    (PipeWriter { tx }, PipeReader { rx, current: Arc::from(Vec::new()), offset: 0, failed: None })
}

pub struct PipeWriter {
    tx: SyncSender<Result<Arc<[u8]>, String>>,
}

impl PipeWriter {
    /// Blocks while the pipe is full. `false` if the reader is gone (stop producing).
    pub fn write(&self, data: &[u8]) -> bool {
        self.write_arc(&Arc::from(data))
    }

    /// [`Self::write`] for a chunk that's already shared: no copy, the pipe holds a reference
    /// to the same allocation (`pipeline::ChunkPipeline` hands one chunk to a hash digest and
    /// to this pipe at once).
    pub fn write_arc(&self, data: &Arc<[u8]>) -> bool {
        self.tx.send(Ok(Arc::clone(data))).is_ok()
    }

    /// Ends the pipe with an error the reader will see instead of end-of-data.
    pub fn fail(self, message: impl Into<String>) {
        let _ = self.tx.send(Err(message.into()));
    }
}

pub struct PipeReader {
    rx: Receiver<Result<Arc<[u8]>, String>>,
    current: Arc<[u8]>,
    offset: usize,
    failed: Option<String>,
}

impl Read for PipeReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if let Some(message) = &self.failed {
            return Err(io::Error::other(message.clone()));
        }
        while self.offset == self.current.len() {
            match self.rx.recv() {
                Ok(Ok(chunk)) => {
                    self.current = chunk;
                    self.offset = 0;
                }
                Ok(Err(message)) => {
                    self.failed = Some(message.clone());
                    return Err(io::Error::other(message));
                }
                Err(_) => return Ok(0), // writer dropped: end of data
            }
        }
        let n = buf.len().min(self.current.len() - self.offset);
        buf[..n].copy_from_slice(&self.current[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// C callback trampolines, shared by the wrappers: every native source struct uses
//   int64_t (*read)(void* user_data, void* buf, uint64_t size)
//   int (*read_at)(void* user_data, uint64_t offset, void* buf, uint64_t size)
// with user_data pointing at a SourceState.
// ---------------------------------------------------------------------------

pub(crate) type RawReadFn = extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void, u64) -> i64;
pub(crate) type RawReadAtFn = extern "C" fn(*mut std::ffi::c_void, u64, *mut std::ffi::c_void, u64) -> std::ffi::c_int;

/// What a native source's user_data points at.
pub(crate) struct SourceState<'a> {
    pub source: &'a mut Source,
    cursor: u64,
    pub error: Option<io::Error>,
}

impl<'a> SourceState<'a> {
    pub fn new(source: &'a mut Source) -> Self {
        Self { source, cursor: 0, error: None }
    }

    /// (read, read_at) to hand the native library: exactly one is set, matching the access.
    pub fn callbacks(&self) -> (Option<RawReadFn>, Option<RawReadAtFn>) {
        if self.source.is_random() {
            (None, Some(source_read_at))
        } else {
            (Some(source_read), None)
        }
    }
}

extern "C" fn source_read(user_data: *mut std::ffi::c_void, buf: *mut std::ffi::c_void, size: u64) -> i64 {
    let state = unsafe { &mut *(user_data as *mut SourceState) };
    let buf = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, size as usize) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let SourceState { source, cursor, .. } = state;
        source.read_next(cursor, buf)
    }));
    match result {
        Ok(Ok(n)) => n as i64,
        Ok(Err(err)) => {
            state.error = Some(err);
            -1
        }
        Err(_) => {
            state.error = Some(io::Error::other("source read panicked"));
            -1
        }
    }
}

extern "C" fn source_read_at(
    user_data: *mut std::ffi::c_void,
    offset: u64,
    buf: *mut std::ffi::c_void,
    size: u64,
) -> std::ffi::c_int {
    let state = unsafe { &mut *(user_data as *mut SourceState) };
    let buf = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, size as usize) };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &mut state.source.access {
        Access::Random(r) => r.read_at(offset, buf),
        Access::Sequential(_) => Err(io::Error::other("read_at on a sequential source")),
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(err)) => {
            state.error = Some(err);
            1
        }
        Err(_) => {
            state.error = Some(io::Error::other("source read_at panicked"));
            1
        }
    }
}

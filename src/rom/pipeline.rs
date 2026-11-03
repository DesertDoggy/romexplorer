//! One read, every consumer: a [`ChunkPipeline`] takes the chunks of one content file as
//! they're produced (read from disk, or decoded out of a container) and, in one pass, feeds
//! them to the input hash digest, through the optional converter
//! ([`super::convert::StreamConverter`]), to the converted-output digest, and to every sink
//! ([`ChunkSink`]: a file being written, a pipe into an archive creator). Chunks are shared
//! (`Arc<[u8]>`), so a chunk is allocated once whatever the number of consumers.
//!
//! Stages downstream of a failed converter are cut off (the sinks are closed with an error,
//! so a consuming archive creator aborts) while the input digest keeps running to the end:
//! a failed convert must not cost the input hash (`operation::Single`'s failure rule).

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::convert::{ConvertError, StreamConverter};
use super::hash::{HashDigest, HashDigestError, HashFlags, HashReport};
use super::source::PipeWriter;

#[derive(Debug)]
pub enum PipelineError {
    Io(String),
    Hash(HashDigestError),
    Convert(ConvertError),
    /// A pipe's reader (the archive creator) went away.
    ConsumerDisconnected,
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "pipeline io error: {message}"),
            Self::Hash(err) => write!(f, "pipeline hash error: {err}"),
            Self::Convert(err) => write!(f, "{err}"),
            Self::ConsumerDisconnected => write!(f, "pipeline consumer disconnected"),
        }
    }
}

impl std::error::Error for PipelineError {}

impl From<HashDigestError> for PipelineError {
    fn from(err: HashDigestError) -> Self {
        Self::Hash(err)
    }
}

/// Where the (converted) stream goes.
pub enum ChunkSink {
    /// A file on disk, created (truncated) when the sink is made.
    File { path: PathBuf, writer: std::io::BufWriter<std::fs::File> },
    /// A push side of [`super::source::pipe`], read by an archive creator on another thread.
    Pipe(PipeWriter),
}

impl ChunkSink {
    pub fn file(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(path)?;
        Ok(Self::File { path: path.to_path_buf(), writer: std::io::BufWriter::with_capacity(4 << 20, file) })
    }

    pub fn pipe(writer: PipeWriter) -> Self {
        Self::Pipe(writer)
    }

    fn write(&mut self, chunk: &Arc<[u8]>) -> Result<(), PipelineError> {
        match self {
            Self::File { writer, path } => {
                writer.write_all(chunk).map_err(|err| PipelineError::Io(format!("write {}: {err}", path.display())))
            }
            Self::Pipe(writer) => {
                if writer.write_arc(chunk) {
                    Ok(())
                } else {
                    Err(PipelineError::ConsumerDisconnected)
                }
            }
        }
    }

    fn finish(self) -> Result<Option<PathBuf>, PipelineError> {
        match self {
            Self::File { mut writer, path } => {
                writer.flush().map_err(|err| PipelineError::Io(format!("flush {}: {err}", path.display())))?;
                Ok(Some(path))
            }
            Self::Pipe(writer) => {
                drop(writer); // end of data for the reader
                Ok(None)
            }
        }
    }

    fn fail(self, message: &str) {
        match self {
            Self::File { path, writer } => {
                drop(writer);
                let _ = std::fs::remove_file(path);
            }
            Self::Pipe(writer) => writer.fail(message.to_string()),
        }
    }
}

/// What a finished pipeline produced.
#[derive(Debug)]
pub struct PipelineOutcome {
    pub input_bytes: u64,
    pub input_report: Option<HashReport>,
    /// Present when a converter ran to completion.
    pub output_bytes: u64,
    pub output_report: Option<HashReport>,
    /// The converter's failure, if it failed (the input hash is still complete).
    pub convert_error: Option<ConvertError>,
    /// Files the sinks wrote.
    pub written: Vec<PathBuf>,
    /// A pipe sink's reader went away mid-stream (its archive creator failed); the rest of
    /// the pipeline ran to completion regardless.
    pub consumer_lost: bool,
}

pub struct ChunkPipeline {
    input_digest: Option<HashDigest>,
    converter: Option<StreamConverter>,
    output_digest: Option<HashDigest>,
    sinks: Vec<ChunkSink>,
    input_bytes: u64,
    output_bytes: u64,
    convert_error: Option<ConvertError>,
    consumer_lost: bool,
    /// Called with `(input_bytes_so_far, output_bytes_so_far)` after every chunk.
    on_progress: Option<Box<dyn FnMut(u64, u64) + Send>>,
}

impl ChunkPipeline {
    /// `hash_flags = None` hashes nothing. With a converter and hashing on, the converted
    /// output gets its own digest; without one the input digest is the output's too.
    pub fn new(hash_flags: Option<HashFlags>, converter: Option<StreamConverter>, sinks: Vec<ChunkSink>) -> Self {
        let flags = hash_flags.map(|f| f.difference(HashFlags::RAHASH));
        let input_digest = flags.map(HashDigest::new);
        let output_digest = match (&converter, flags) {
            (Some(_), Some(flags)) => Some(HashDigest::new(flags)),
            _ => None,
        };
        Self { input_digest, converter, output_digest, sinks, input_bytes: 0, output_bytes: 0, convert_error: None, consumer_lost: false, on_progress: None }
    }

    pub fn with_progress(mut self, on_progress: impl FnMut(u64, u64) + Send + 'static) -> Self {
        self.on_progress = Some(Box::new(on_progress));
        self
    }

    pub fn has_converter(&self) -> bool {
        self.converter.is_some()
    }

    /// Feeds one input chunk to everything downstream.
    pub fn push(&mut self, chunk: Arc<[u8]>) -> Result<(), PipelineError> {
        self.input_bytes += chunk.len() as u64;
        if let Some(digest) = &self.input_digest {
            digest.update_arc(Arc::clone(&chunk))?;
        }
        if self.convert_error.is_none() {
            match &mut self.converter {
                Some(converter) => match converter.push(&chunk) {
                    Ok(outputs) => {
                        for output in outputs {
                            self.emit(output)?;
                        }
                    }
                    Err(err) => self.fail_convert(err),
                },
                None => self.emit(chunk)?,
            }
        }
        if let Some(on_progress) = &mut self.on_progress {
            on_progress(self.input_bytes, self.output_bytes);
        }
        Ok(())
    }

    /// Feeds the sinks. A pipe whose reader has gone away (the archive creator failed) is
    /// dropped and noted (`consumer_lost`), not treated as this pipeline's failure: the
    /// hashes and any file sink continue -- an archive failure must not cost the hash.
    fn emit(&mut self, chunk: Arc<[u8]>) -> Result<(), PipelineError> {
        self.output_bytes += chunk.len() as u64;
        if let Some(digest) = &self.output_digest {
            digest.update_arc(Arc::clone(&chunk))?;
        }
        let mut failed: Option<PipelineError> = None;
        let mut lost = false;
        self.sinks.retain_mut(|sink| match sink.write(&chunk) {
            Ok(()) => true,
            Err(PipelineError::ConsumerDisconnected) => {
                lost = true;
                false
            }
            Err(err) => {
                if failed.is_none() {
                    failed = Some(err);
                }
                false
            }
        });
        if lost {
            self.consumer_lost = true;
        }
        match failed {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    fn fail_convert(&mut self, err: ConvertError) {
        let message = err.to_string();
        for sink in self.sinks.drain(..) {
            sink.fail(&message);
        }
        self.output_digest = None;
        self.convert_error = Some(err);
    }

    /// Flushes the converter and the sinks and collects the hashes.
    pub fn finish(mut self) -> Result<PipelineOutcome, PipelineError> {
        if self.convert_error.is_none() {
            if let Some(converter) = self.converter.take() {
                match converter.finish() {
                    Ok(outputs) => {
                        for output in outputs {
                            self.emit(output)?;
                        }
                    }
                    Err(err) => self.fail_convert(err),
                }
            }
        }
        let mut written = Vec::new();
        for sink in self.sinks.drain(..) {
            if let Some(path) = sink.finish()? {
                written.push(path);
            }
        }
        let input_report = self.input_digest.take().map(HashDigest::finish);
        let output_report = self.output_digest.take().map(HashDigest::finish);
        Ok(PipelineOutcome {
            input_bytes: self.input_bytes,
            input_report,
            output_bytes: self.output_bytes,
            output_report,
            convert_error: self.convert_error.take(),
            written,
            consumer_lost: self.consumer_lost,
        })
    }

    /// Abandons the pipeline after an upstream failure: sinks are failed/removed, nothing is
    /// hashed further.
    pub fn abort(mut self, message: &str) {
        for sink in self.sinks.drain(..) {
            sink.fail(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rom::convert::{N64Converter, StreamConverter};
    use crate::rom::models::N64Type;
    use std::io::Read;

    #[test]
    fn hashes_input_and_converted_output_and_feeds_a_pipe() {
        let input: Vec<u8> = [0x80u8, 0x37, 0x12, 0x40].iter().copied().chain((0..4092u32).map(|i| (i % 251) as u8)).collect();
        let converter = StreamConverter::new(Box::new(N64Converter { target: N64Type::ByteSwapped, known_order: None }), input.len() as u64, &input[..16]).unwrap();
        let (writer, mut reader) = crate::rom::source::pipe(4);
        let consumer = std::thread::spawn(move || {
            let mut out = Vec::new();
            reader.read_to_end(&mut out).unwrap();
            out
        });
        let mut pipeline = ChunkPipeline::new(Some(HashFlags::CRC32), Some(converter), vec![ChunkSink::pipe(writer)]);
        for piece in input.chunks(1000) {
            pipeline.push(Arc::from(piece)).unwrap();
        }
        let outcome = pipeline.finish().unwrap();
        let consumed = consumer.join().unwrap();
        assert_eq!(outcome.input_bytes, input.len() as u64);
        assert_eq!(outcome.output_bytes, input.len() as u64);
        assert_eq!(&consumed[..4], &[0x37, 0x80, 0x40, 0x12]);
        let mut expected = crc32fast::Hasher::new();
        expected.update(&input);
        assert_eq!(outcome.input_report.unwrap().crc32.unwrap(), format!("{:08x}", expected.finalize()));
        let mut expected_out = crc32fast::Hasher::new();
        expected_out.update(&consumed);
        assert_eq!(outcome.output_report.unwrap().crc32.unwrap(), format!("{:08x}", expected_out.finalize()));
        assert!(outcome.convert_error.is_none());
    }

    #[test]
    fn without_converter_output_is_input_and_file_sink_writes() {
        let dir = std::env::temp_dir().join(format!("romexplorer-pipeline-{}", std::process::id()));
        let path = dir.join("out.bin");
        let input: Vec<u8> = (0..10_000u32).map(|i| (i % 13) as u8).collect();
        let mut pipeline = ChunkPipeline::new(Some(HashFlags::XXH3_128), None, vec![ChunkSink::file(&path).unwrap()]);
        for piece in input.chunks(999) {
            pipeline.push(Arc::from(piece)).unwrap();
        }
        let outcome = pipeline.finish().unwrap();
        assert_eq!(outcome.written, vec![path.clone()]);
        assert_eq!(std::fs::read(&path).unwrap(), input);
        assert!(outcome.output_report.is_none());
        assert!(outcome.input_report.unwrap().xxh3_128.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}

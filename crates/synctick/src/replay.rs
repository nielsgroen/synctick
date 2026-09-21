//! Versioned deterministic session input logs and bounded replay.
use crate::protocol::{
    MAX_REPLAY_RECORD_BYTES, PROTOCOL_ID, TickAdvance, decode_tick_advance, encode,
};
use crate::session::{SessionControl, SessionError, SessionResult};
use crate::simulation::{SessionSimulation, TickPhase, advance_tick, validate_tick};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;

const MAGIC: [u8; 4] = *b"SESS";

/// Bumped on incompatible header changes. `PROTOCOL_ID` already gates
/// payload-shape compatibility (any `CommandPayload` change forces a
/// protocol bump), so this version is reserved for header-only edits.
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayHeader {
    pub magic: [u8; 4],
    pub protocol_id: u64,
    pub format_version: u32,
    pub game_id: [u8; 16],
    pub game_version: u32,
    pub tick_nanos: u64,
    pub initialization: Vec<u8>,
}

impl ReplayHeader {
    pub const fn new(
        game_id: [u8; 16],
        game_version: u32,
        tick_nanos: u64,
        initialization: Vec<u8>,
    ) -> Self {
        Self {
            magic: MAGIC,
            protocol_id: PROTOCOL_ID,
            format_version: FORMAT_VERSION,
            game_id,
            game_version,
            tick_nanos,
            initialization,
        }
    }

    #[cfg(test)]
    pub fn current() -> Self {
        Self {
            magic: MAGIC,
            protocol_id: PROTOCOL_ID,
            format_version: FORMAT_VERSION,
            game_id: *b"session-testgame",
            game_version: 1,
            tick_nanos: 33_333_333,
            initialization: vec![0],
        }
    }
}

/// Append-only writer for a replay log. Buffered; flush is explicit.
///
/// The server calls `flush` at the per-second desync-hash boundary so a
/// process failure normally loses at most the unflushed tick interval.
/// This does not synchronize durable storage or guarantee power-loss recovery.
#[derive(Debug)]
pub struct ReplayWriter {
    inner: BufWriter<File>,
}

impl ReplayWriter {
    /// Create the file, write the header.
    ///
    /// Errors here surface before `wait_for_clients` so a typo'd path
    /// fails fast.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from exclusive file creation and the header write.
    pub fn create(path: &Path, header: &ReplayHeader) -> io::Result<Self> {
        let file = File::options().write(true).create_new(true).open(path)?;
        let mut inner = BufWriter::new(file);
        let header_bytes = encode(header);
        inner.write_all(&header_bytes)?;
        // Header scalar fields use bincode; initialization is bounded and read
        // incrementally by read_header. Tick records are length-prefixed.
        Ok(Self { inner })
    }

    /// Append one `TickAdvance` record, length-prefixed.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from the underlying buffered writer, plus
    /// `InvalidData` if the encoded payload exceeds the lockstep record budget.
    pub fn append(&mut self, advance: &TickAdvance) -> io::Result<()> {
        let payload = encode(advance);
        if payload.len() > MAX_REPLAY_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "replay record exceeds lockstep budget",
            ));
        }
        // Lengths are bounded by `bincode` payload size for one
        // `TickAdvance`; in practice tens to hundreds of bytes per tick.
        // `try_from` keeps us honest if a future record ever crosses
        // 4 GiB, which would itself indicate a bug.
        let len = u32::try_from(payload.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "TickAdvance payload exceeds u32::MAX",
            )
        })?;
        self.inner.write_all(&len.to_le_bytes())?;
        self.inner.write_all(&payload)?;
        Ok(())
    }

    /// Flush buffered records to disk.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from the underlying buffered writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Streaming reader for a replay log.
///
/// Generic over any `BufRead` source so the *same parser* handles
/// recorded files (`BufReader<File>`) and wire-delivered save bytes
/// (`Cursor<Vec<u8>>`, used by the client save-receive path).
///
/// Construction parses + validates the header against this build's
/// `PROTOCOL_ID` so wire-incompatible inputs are rejected up front.
/// Each `next_record` yields the next `TickAdvance` or `Ok(None)` at
/// clean end-of-source. A torn trailing record (recorder crashed
/// mid-write, network truncation) surfaces as an `Err` rather than
/// being silently dropped — corrupt logs shouldn't shorten without
/// notice.
#[derive(Debug)]
pub struct ReplayReader<R: BufRead> {
    inner: R,
    header: ReplayHeader,
}

impl ReplayReader<BufReader<File>> {
    /// Open the replay log at `path` and validate its header.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from `File::open` and any header
    /// validation failures (`InvalidData` on bad magic, mismatched
    /// `PROTOCOL_ID`, or wrong format version).
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        Self::from_reader(BufReader::new(file))
    }
}

impl<R: BufRead> ReplayReader<R> {
    /// Construct from any `BufRead` source. Parses and validates the
    /// header before returning.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` if the header magic, `PROTOCOL_ID`, or
    /// format version don't match this build; otherwise propagates I/O
    /// errors from the underlying reader.
    pub fn from_reader(mut inner: R) -> io::Result<Self> {
        let header = read_header(&mut inner)?;
        Ok(Self { inner, header })
    }

    #[must_use]
    pub const fn header(&self) -> &ReplayHeader {
        &self.header
    }

    /// `Ok(Some(_))` for a record, `Ok(None)` for clean EOF, `Err(_)`
    /// for a torn or malformed record.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from the underlying reader, plus
    /// `InvalidData` for malformed records (length-prefix overflow,
    /// truncated payload, or bincode decode failure).
    pub fn next_record(&mut self) -> io::Result<Option<TickAdvance>> {
        // `fill_buf` returns `Ok(&[])` only at EOF (not on a transient
        // empty buffer), which is how we distinguish clean end-of-log
        // from a torn record mid-payload.
        if self.inner.fill_buf()?.is_empty() {
            return Ok(None);
        }
        let mut len_bytes = [0u8; 4];
        self.inner.read_exact(&mut len_bytes).map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                io::Error::new(io::ErrorKind::InvalidData, "truncated replay record length")
            } else {
                error
            }
        })?;
        let len = u32::from_le_bytes(len_bytes);
        let len_usize = usize::try_from(len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "record length exceeds usize::MAX on this platform",
            )
        })?;
        if len_usize > MAX_REPLAY_RECORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "replay record exceeds lockstep budget",
            ));
        }
        let mut payload = Vec::new();
        while payload.len() < len_usize {
            let available = self.inner.fill_buf()?;
            if available.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated replay record",
                ));
            }
            let count = available.len().min(len_usize - payload.len());
            payload.try_reserve(count).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("replay allocation: {error}"),
                )
            })?;
            payload.extend_from_slice(&available[..count]);
            self.inner.consume(count);
        }
        let advance: TickAdvance = decode_tick_advance(&payload).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bincode decode of TickAdvance: {err}"),
            )
        })?;
        Ok(Some(advance))
    }
}

fn read_header<R: Read>(reader: &mut R) -> io::Result<ReplayHeader> {
    fn scalar<T: for<'de> Deserialize<'de>>(reader: &mut impl Read) -> io::Result<T> {
        bincode::serde::decode_from_std_read(reader, bincode::config::standard())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
    let magic = scalar(reader)?;
    if magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported save magic; this reader requires a framework-v1 session",
        ));
    }
    let protocol_id = scalar(reader)?;
    let format_version = scalar(reader)?;
    let game_id = scalar(reader)?;
    let game_version = scalar(reader)?;
    let tick_nanos = scalar(reader)?;
    let len: u64 = scalar(reader)?;
    let length = usize::try_from(len).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "initialization length overflow")
    })?;
    if length > crate::codec::MAX_PAYLOAD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "initialization too large",
        ));
    }
    let mut initialization = Vec::new();
    let mut buffer = [0; 4096];
    while initialization.len() < length {
        let count = buffer.len().min(length - initialization.len());
        reader.read_exact(&mut buffer[..count])?;
        initialization
            .try_reserve(count)
            .map_err(io::Error::other)?;
        initialization.extend_from_slice(&buffer[..count]);
    }
    let header = ReplayHeader {
        magic,
        protocol_id,
        format_version,
        game_id,
        game_version,
        tick_nanos,
        initialization,
    };
    if header.magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "replay log magic mismatch: got {:?}, expected {:?}",
                header.magic, MAGIC
            ),
        ));
    }
    if header.protocol_id != PROTOCOL_ID {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "replay log PROTOCOL_ID mismatch: got {:#x}, this build is {:#x}",
                header.protocol_id, PROTOCOL_ID
            ),
        ));
    }
    if header.format_version != FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "replay log format_version mismatch: got {}, this build is {}",
                header.format_version, FORMAT_VERSION
            ),
        ));
    }
    crate::api::validate_tick_duration(std::time::Duration::from_nanos(header.tick_nanos))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(header)
}

/// Replay to EOF with cancellation between ticks and bounded batches.
/// # Errors
/// Returns malformed records, simulation failures, and recording errors.
pub fn replay_into<R: BufRead>(
    sim: &mut impl SessionSimulation,
    reader: &mut ReplayReader<R>,
    mut mirror: Option<&mut ReplayWriter>,
    control: &SessionControl,
) -> SessionResult<u64> {
    let mut total = 0;
    while !control.is_cancelled() {
        let batch = replay_batch(sim, reader, mirror.as_deref_mut(), control)?;
        total += batch.records;
        if batch.finished {
            break;
        }
    }
    Ok(total)
}

pub struct ReplayBatch {
    pub records: u64,
    pub finished: bool,
}

/// Execute at most 128 records or 5 ms, observing cancellation between ticks.
/// A single deterministic tick is indivisible.
/// # Errors
/// Returns malformed-record, tick-continuity, and recording I/O errors.
pub fn replay_batch<R: BufRead>(
    sim: &mut impl SessionSimulation,
    reader: &mut ReplayReader<R>,
    mut mirror: Option<&mut ReplayWriter>,
    control: &crate::session::SessionControl,
) -> SessionResult<ReplayBatch> {
    let started = std::time::Instant::now();
    let mut records = 0;
    while records < 128 && started.elapsed() < std::time::Duration::from_millis(5) {
        if control.is_cancelled() {
            break;
        }
        let Some(advance) = reader.next_record()? else {
            return Ok(ReplayBatch {
                records,
                finished: true,
            });
        };
        // Validate before mirroring: a rejected tick must never enter a recording.
        validate_tick(sim, advance.tick).map_err(|error| {
            SessionError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                error.to_string(),
            ))
        })?;
        if let Some(writer) = mirror.as_deref_mut() {
            writer.append(&advance)?;
        }
        advance_tick(sim, advance, TickPhase::Replay)?;
        records += 1;
    }
    Ok(ReplayBatch {
        records,
        finished: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CommandPayload;
    use std::io::{Cursor, Seek};
    use tempfile::NamedTempFile;

    #[test]
    fn round_trips_header_and_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("record.save");

        let header = ReplayHeader::current();
        let mut writer = ReplayWriter::create(&path, &header).expect("create");
        writer
            .append(&TickAdvance {
                tick: 1,
                inputs: vec![],
            })
            .expect("append empty");
        writer
            .append(&TickAdvance {
                tick: 2,
                inputs: vec![
                    CommandPayload(vec![0]).into(),
                    CommandPayload(vec![0]).into(),
                ],
            })
            .expect("append two");
        writer.flush().expect("flush");
        drop(writer);

        let mut reader = ReplayReader::open(&path).expect("open");
        assert_eq!(reader.header().protocol_id, PROTOCOL_ID);
        assert_eq!(reader.header().format_version, FORMAT_VERSION);
        assert_eq!(reader.header().initialization, vec![0]);

        let r1 = reader.next_record().expect("read 1").expect("some");
        assert_eq!(r1.tick, 1);
        assert!(r1.inputs.is_empty());

        let r2 = reader.next_record().expect("read 2").expect("some");
        assert_eq!(r2.tick, 2);
        assert_eq!(
            r2.inputs,
            vec![
                CommandPayload(vec![0]).into(),
                CommandPayload(vec![0]).into()
            ]
        );

        assert!(reader.next_record().expect("read 3").is_none());
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut tmp = NamedTempFile::new().expect("tempfile");
        let bad = ReplayHeader {
            magic: *b"XXXX",
            protocol_id: PROTOCOL_ID,
            format_version: FORMAT_VERSION,
            game_id: *b"session-testgame",
            game_version: 1,
            tick_nanos: 33_333_333,
            initialization: vec![0],
        };
        let bytes = encode(&bad);
        tmp.write_all(&bytes).expect("write");
        tmp.flush().expect("flush");
        tmp.as_file_mut().rewind().expect("rewind");

        let err = ReplayReader::open(tmp.path()).expect_err("should reject");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("magic"));
    }

    #[test]
    fn rejects_wrong_protocol_id() {
        let mut tmp = NamedTempFile::new().expect("tempfile");
        let bad = ReplayHeader {
            magic: MAGIC,
            protocol_id: 0xDEAD_BEEF,
            format_version: FORMAT_VERSION,
            game_id: *b"session-testgame",
            game_version: 1,
            tick_nanos: 33_333_333,
            initialization: vec![0],
        };
        let bytes = encode(&bad);
        tmp.write_all(&bytes).expect("write");
        tmp.flush().expect("flush");
        tmp.as_file_mut().rewind().expect("rewind");

        let err = ReplayReader::open(tmp.path()).expect_err("should reject");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("PROTOCOL_ID"));
    }

    #[test]
    fn recording_never_overwrites_existing_files_or_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.save");
        std::fs::write(&source, b"precious source").unwrap();
        let hard = dir.path().join("hard.save");
        std::fs::hard_link(&source, &hard).unwrap();
        for path in [&source, &hard] {
            assert_eq!(
                ReplayWriter::create(path, &ReplayHeader::current())
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(std::fs::read(&source).unwrap(), b"precious source");
        }
        #[cfg(unix)]
        {
            let link = dir.path().join("link.save");
            std::os::unix::fs::symlink(&source, &link).unwrap();
            assert!(ReplayWriter::create(&link, &ReplayHeader::current()).is_err());
            assert_eq!(std::fs::read(&source).unwrap(), b"precious source");
            let dangling = dir.path().join("dangling.save");
            std::os::unix::fs::symlink(dir.path().join("absent"), &dangling).unwrap();
            assert!(ReplayWriter::create(&dangling, &ReplayHeader::current()).is_err());
        }
    }

    #[test]
    fn replay_batches_yield_cancel_and_reject_tick_gaps() {
        let mut bytes = encode(&ReplayHeader::current());
        for tick in 1..=300 {
            let payload = encode(&TickAdvance {
                tick,
                inputs: vec![],
            });
            bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
            bytes.extend(payload);
        }
        let mut reader = ReplayReader::from_reader(Cursor::new(bytes)).unwrap();
        let mut app = crate::test_simulation::TestSimulation::default();
        let control = crate::session::SessionControl::default();
        let first = replay_batch(&mut app, &mut reader, None, &control).unwrap();
        assert!(!first.finished);
        assert!((1..=128).contains(&first.records));
        control.cancel();
        let cancelled = replay_batch(&mut app, &mut reader, None, &control).unwrap();
        assert_eq!(cancelled.records, 0);
        assert_eq!(app.current_tick(), first.records);
        let control = crate::session::SessionControl::default();
        app.tick += 1;
        assert!(matches!(
            replay_batch(&mut app, &mut reader, None, &control),
            Err(SessionError::Io(error)) if error.kind() == io::ErrorKind::InvalidData
        ));
    }
    #[test]
    fn malformed_record_lengths_and_truncation_are_errors() {
        for length in [u32::MAX, 1] {
            let mut bytes = encode(&ReplayHeader::current());
            bytes.extend_from_slice(&length.to_le_bytes());
            let mut reader = ReplayReader::from_reader(Cursor::new(bytes)).unwrap();
            assert!(reader.next_record().is_err());
        }
    }
    #[test]
    fn rejects_legacy_and_truncated_headers() {
        let legacy = b"PLNT\xfd\x05\x00\x54\x45\x4e\x41\x4c\x50\x01\x00";
        assert!(ReplayReader::from_reader(Cursor::new(legacy)).is_err());
        let bytes = encode(&ReplayHeader::current());
        for end in 0..bytes.len() {
            assert!(ReplayReader::from_reader(Cursor::new(&bytes[..end])).is_err());
        }
        let mut header = ReplayHeader::current();
        header.tick_nanos = 0;
        assert!(ReplayReader::from_reader(Cursor::new(encode(&header))).is_err());
    }
}

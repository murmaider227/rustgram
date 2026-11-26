//! The frame-trace text file, written in whole lines only (TGB-2949).
//!
//! Lines are collected in memory and written in chunks that end at a line end, each chunk with
//! one `write_all` to a file opened for appending. A process never writes part of a line, and
//! when a second process writes the same file by mistake (two benchmarks sharing an output
//! directory), the appends land after each other instead of over each other, so the lines of the
//! two stay whole. `trace_start` names the process id, so the analyzer can tell such a file apart.

use std::fmt::{self, Write as _};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Bytes collected before they are written.
const FLUSH_AT: usize = 16 * 1024;

pub(super) struct TraceOut<W = File> {
    sink: W,
    buf: String,
}

impl TraceOut<File> {
    /// Starts an empty trace file at `path`, replacing an older one.
    pub(super) fn create(path: &Path) -> io::Result<Self> {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self::new(file))
    }
}

impl<W: Write> TraceOut<W> {
    pub(super) fn new(sink: W) -> Self {
        Self {
            sink,
            buf: String::with_capacity(FLUSH_AT * 2),
        }
    }

    /// Adds one line (`args` and a line end); writes the collected lines once there are enough.
    pub(super) fn line(&mut self, args: fmt::Arguments<'_>) {
        let _ = self.buf.write_fmt(args);
        self.buf.push('\n');
        if self.buf.len() >= FLUSH_AT {
            self.flush();
        }
    }

    /// Writes every collected line in one `write_all`.
    pub(super) fn flush(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let _ = self.sink.write_all(self.buf.as_bytes());
        let _ = self.sink.flush();
        self.buf.clear();
    }

    #[cfg(test)]
    pub(super) fn sink(&self) -> &W {
        &self.sink
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::{FLUSH_AT, TraceOut};

    /// Records the size and the content of every `write` call.
    #[derive(Default)]
    struct Writes(Vec<Vec<u8>>);

    impl Write for Writes {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.push(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn every_write_ends_at_a_line_end() {
        let mut out = TraceOut::new(Writes::default());
        for i in 0..5_000_u64 {
            out.line(format_args!(
                "F\t{i}\t{}\t37\t131\tchatlist_scroll",
                i * 4_000
            ));
        }
        out.flush();
        let writes = &out.sink().0;
        assert!(writes.len() > 1, "the lines are written in several chunks");
        for chunk in writes {
            assert_eq!(chunk.last(), Some(&b'\n'), "a chunk ends inside a line");
            assert!(chunk.len() < FLUSH_AT * 2);
        }
        let text: String = writes
            .iter()
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        assert_eq!(text.lines().count(), 5_000);
        assert!(
            text.lines()
                .enumerate()
                .all(|(i, l)| l.starts_with(&format!("F\t{i}\t")) && l.split('\t').count() == 6)
        );
    }

    #[test]
    fn two_writers_of_one_file_keep_their_lines_whole() {
        let dir = std::env::temp_dir().join(format!("tgb2949_trace_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("trace.tsv");
        let mut a = TraceOut::create(&path).expect("first writer");
        // The second writer opens the same file the way a second process would.
        let mut b = TraceOut::new(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("second writer"),
        );
        for i in 0..3_000_u64 {
            a.line(format_args!(
                "F\t{i}\t{}\t40\t150\tchatlist_scroll",
                i * 4_100
            ));
            b.line(format_args!(
                "F\t{i}\t{}\t1300\t1700\tchatlist_scroll",
                i * 4_000
            ));
        }
        a.flush();
        b.flush();
        let text = std::fs::read_to_string(&path).expect("read trace");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(text.lines().count(), 6_000);
        for line in text.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            assert_eq!(parts.len(), 6, "garbled line {line:?}");
            assert!(parts[1..5].iter().all(|p| p.parse::<u64>().is_ok()));
        }
    }
}

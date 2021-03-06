use anyhow::Context;
use ndarray::prelude::*;
use ndarray_csv::Array2Reader;
use std::cmp::min;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::Path;
use std::time::Duration;

pub struct DataFrame {
    newlines: LineOffsets,
    headers: csv::StringRecord,
    file: File,
}
impl DataFrame {
    pub fn new(path: &Path) -> anyhow::Result<DataFrame> {
        let mut newlines = LineOffsets::new(path).context("Scanning newlines")?;
        const MIN_LINES: usize = 3;
        let n = newlines.len();
        if n < MIN_LINES {
            let mut sleep = Duration::from_millis(1);
            while newlines.len() < MIN_LINES {
                newlines.update().context("Updating newlines")?;
                std::thread::sleep(sleep);
                if sleep < Duration::from_millis(500) {
                    sleep *= 2; // Start with an exponential backoff
                }
                if sleep > Duration::from_millis(500) {
                    // Looks like we'll be waiting a while...
                    eprintln!(
                        "Waiting for more data... (saw {} lines but need at least {})",
                        n, MIN_LINES
                    );
                    sleep = Duration::from_millis(500);
                }
            }
        }

        let mut file = File::open(path)
            .context(path.display().to_string())
            .context("Opening file again to read actual data")?;
        let headers = csv::Reader::from_reader(&mut file).headers()?.clone();
        Ok(DataFrame {
            newlines,
            headers,
            file,
        })
    }

    pub fn refresh(&mut self) -> anyhow::Result<()> {
        self.newlines.update()
    }

    pub fn get_headers(&self) -> impl Iterator<Item = &str> + '_ {
        self.headers.iter()
    }

    pub fn get_data(
        &mut self,
        start_line: usize,
        end_line: usize,
    ) -> anyhow::Result<Array2<String>> {
        fn take_range(file: &mut File, r: Range<u64>) -> std::io::Result<impl Read + '_> {
            file.seek(SeekFrom::Start(r.start))?;
            Ok(file.take(r.end - r.start))
        }

        let byte_range =
            self.newlines.line2range(start_line).start..self.newlines.line2range(end_line).end;
        let rdr = take_range(&mut self.file, byte_range)?;
        let mut rdr = csv::ReaderBuilder::new()
            .trim(csv::Trim::All)
            .from_reader(rdr);
        Ok(rdr.deserialize_array2::<String>((end_line - start_line, self.headers.len()))?)
    }

    pub fn len(&self) -> usize {
        self.newlines.len()
    }

    pub fn search(&mut self, start_line: usize, pattern: &str) -> anyhow::Result<Option<usize>> {
        let max_line = self.len() - 2;
        let add = |start_line: usize, x: usize| min(max_line, start_line.saturating_add(x));
        let x = self.newlines.line2range(start_line).start;
        self.file.seek(SeekFrom::Start(x))?;
        let matcher = grep_regex::RegexMatcher::new(pattern)?;
        let mut ret = None;
        let sink = grep_searcher::sinks::UTF8(|line, _| {
            ret = Some(add(start_line - 1, line as usize - 1));
            Ok(false)
        });
        grep_searcher::Searcher::new().search_file(&matcher, &self.file, sink)?;
        Ok(ret)
    }
}

struct LineOffsets {
    offset: u64,
    newlines: Vec<u64>,
    file: BufReader<File>,
}
impl LineOffsets {
    fn new(path: &Path) -> anyhow::Result<LineOffsets> {
        let mut ret = LineOffsets {
            offset: 0,
            file: BufReader::new(File::open(path)?),
            newlines: vec![],
        };
        ret.update()?;
        Ok(ret)
    }
    fn update(&mut self) -> anyhow::Result<()> {
        loop {
            let buf = self.file.fill_buf()?;
            if buf.is_empty() {
                break;
            }
            if let Some(x) = memchr::memchr(b'\n', buf) {
                self.newlines.push(self.offset + x as u64);
                self.offset += x as u64 + 1;
                self.file.consume(x + 1);
            } else {
                let x = buf.len();
                self.offset += x as u64;
                self.file.consume(x);
            }
        }
        Ok(())
    }
    /// Gives a byte-range which doesn't include the newline
    fn line2range(&self, line: usize) -> Range<u64> {
        let lhs = if line == 0 {
            0
        } else {
            self.newlines[line - 1] as u64 + 1
        };
        let rhs = self.newlines[line] as u64;
        lhs..rhs
    }
    fn len(&self) -> usize {
        self.newlines.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::*;

    #[test]
    fn test() {
        let mut f = NamedTempFile::new().unwrap();
        let s = b"foo,bar\n1,2\n3,4\n";
        f.write_all(s).unwrap();
        let lines = LineOffsets::new(f.path()).unwrap();
        assert_eq!(lines.len(), 3);
        // line2range never includes the newline char, hence the non-contiguous
        // ranges
        assert_eq!(lines.line2range(0), 0..7);
        assert_eq!(lines.line2range(1), 8..11);
        assert_eq!(lines.line2range(2), 12..15);
        assert_eq!(s.len(), 16);
    }
}

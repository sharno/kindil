//! Bounded UTF-8 text reader. No duplicate renderer or external viewer process.
use std::path::{Path, PathBuf};

pub const MAX_BOOK_BYTES: u64 = 4 * 1024 * 1024;

pub struct Reader {
    pub title: String,
    lines: Vec<String>,
    page: usize,
    rows: usize,
    source: String,
}

impl Reader {
    pub fn open(path: &Path, columns: usize, rows: usize) -> std::io::Result<Self> {
        use std::io::Read;
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
        {
            return Self::open_epub(path, columns, rows);
        }
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(MAX_BOOK_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BOOK_BYTES {
            return Err(std::io::Error::other("text file exceeds 4 MiB limit"));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| std::io::Error::other("book must be UTF-8 text"))?;
        Ok(Self::from_text(
            &path.file_name().unwrap_or_default().to_string_lossy(),
            &text,
            columns,
            rows,
        ))
    }

    fn open_epub(path: &Path, columns: usize, rows: usize) -> std::io::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        if file.metadata()?.len() > 16 * 1024 * 1024 {
            return Err(std::io::Error::other("EPUB exceeds 16 MiB device budget"));
        }
        {
            let mut archive = zip::ZipArchive::new(&mut file).map_err(std::io::Error::other)?;
            if archive.len() > 2048 {
                return Err(std::io::Error::other("EPUB has too many entries"));
            }
            let mut total = 0u64;
            for i in 0..archive.len() {
                let entry = archive.by_index(i).map_err(std::io::Error::other)?;
                total = total
                    .checked_add(entry.size())
                    .ok_or_else(|| std::io::Error::other("EPUB size overflow"))?;
                if entry.size() > MAX_BOOK_BYTES || total > 16 * 1024 * 1024 {
                    return Err(std::io::Error::other(
                        "EPUB expanded size exceeds device budget",
                    ));
                }
            }
        }
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        let epub = rbook::Epub::read(file).map_err(std::io::Error::other)?;
        let mut text = String::new();
        for chapter in epub.reader() {
            let chapter = chapter.map_err(std::io::Error::other)?;
            let plain = html2text::from_read(chapter.content().as_bytes(), columns.max(1))
                .map_err(std::io::Error::other)?;
            if text.len() + plain.len() > MAX_BOOK_BYTES as usize {
                return Err(std::io::Error::other("EPUB text exceeds device budget"));
            }
            text.push_str(&plain);
            text.push_str("\n\n");
        }
        let title = epub.metadata().title().map(|t| t.value()).unwrap_or("EPUB");
        Ok(Self::from_text(title, &text, columns, rows))
    }

    pub fn from_text(title: &str, text: &str, columns: usize, rows: usize) -> Self {
        let columns = columns.max(1);
        let mut lines = Vec::new();
        for paragraph in text.lines() {
            lines.extend(
                textwrap::wrap(paragraph, columns)
                    .into_iter()
                    .map(|line| line.into_owned()),
            );
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            title: title.into(),
            lines,
            page: 0,
            rows: rows.max(1),
            source: text.into(),
        }
    }

    pub fn page(&self) -> usize {
        self.page
    }
    pub fn pages(&self) -> usize {
        self.lines.len().div_ceil(self.rows)
    }
    pub fn visible(&self) -> &[String] {
        let start = self.page * self.rows;
        &self.lines[start..(start + self.rows).min(self.lines.len())]
    }
    pub fn turn(&mut self, forward: bool) {
        self.page = if forward {
            (self.page + 1).min(self.pages() - 1)
        } else {
            self.page.saturating_sub(1)
        };
    }
    /// Repaginate using a conservative width/height budget for the new font.
    /// Preserve approximate progress rather than jumping to the first page.
    pub fn resize_font(&mut self, font_px: u32) {
        let progress = self.page as f64 / self.pages() as f64;
        let mut new = Self::from_text(
            &self.title,
            &self.source,
            // Textwrap estimates columns; Slint does final pixel wrapping.
            // A Latin glyph averages roughly 0.6em, not a whole font-size.
            (650 * 5 / (font_px.max(16) * 3)) as usize,
            (620 / (font_px.max(16) * 3 / 2)) as usize,
        );
        new.page = ((progress * new.pages() as f64) as usize).min(new.pages() - 1);
        *self = new;
    }
}

/// Deterministic flat library; do not follow symlinks or scan the whole disk.
pub fn library(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("txt") || e.eq_ignore_ascii_case("epub"))
        })
        .take(256)
        .collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wraps_and_pages_without_losing_words() {
        let mut r = Reader::from_text("test", "one two three\nabcdefghijk\nlast", 5, 2);
        assert!(r.lines.iter().all(|l| l.chars().count() <= 5));
        assert_eq!(r.visible(), &["one", "two"]);
        for _ in 0..100 {
            r.turn(true);
        }
        assert_eq!(r.page(), r.pages() - 1);
        for _ in 0..100 {
            r.turn(false);
        }
        assert_eq!(r.page(), 0);
    }
    #[test]
    fn empty_text_has_one_page() {
        let r = Reader::from_text("empty", "", 10, 10);
        assert_eq!(r.pages(), 1);
        assert_eq!(r.visible(), &[""]);
    }
    #[test]
    fn opens_an_actual_epub_through_rbook() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.epub");
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let files = [
            ("mimetype", "application/epub+zip"),
            (
                "META-INF/container.xml",
                r#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="EPUB/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            ),
            (
                "EPUB/book.opf",
                r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">kindil-test</dc:identifier><dc:title>Test EPUB</dc:title><dc:language>ar</dc:language><meta property="dcterms:modified">2026-11-11T00:00:00Z</meta></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
            ),
            (
                "EPUB/chapter.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter</title></head><body><h1>Hello EPUB</h1><p>قنديل</p></body></html>"#,
            ),
            (
                "EPUB/nav.xhtml",
                r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="chapter.xhtml">Chapter</a></li></ol></nav></body></html>"#,
            ),
        ];
        for (name, body) in files {
            archive
                .start_file(
                    name,
                    zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored),
                )
                .unwrap();
            archive.write_all(body.as_bytes()).unwrap();
        }
        archive.finish().unwrap();
        let book = Reader::open(&path, 48, 23).unwrap();
        assert_eq!(book.title, "Test EPUB");
        let text = book.visible().join("\n");
        assert!(text.contains("Hello EPUB"));
        assert!(text.contains("قنديل"));
    }
    #[test]
    fn rejects_zip_expansion_before_parsing_xml() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.epub");
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        archive
            .start_file(
                "huge.xhtml",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        archive
            .write_all(&vec![b' '; MAX_BOOK_BYTES as usize + 1])
            .unwrap();
        archive.finish().unwrap();
        let error = Reader::open(&path, 48, 23).err().unwrap();
        assert!(error.to_string().contains("expanded size"));
    }
}

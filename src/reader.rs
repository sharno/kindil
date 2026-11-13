//! Bounded UTF-8 text and EPUB reader: text reflow, not a full EPUB engine.
use crate::prefs::{FontSize, Progress};
use std::path::{Path, PathBuf};

pub const MAX_BOOK_BYTES: u64 = 4 * 1024 * 1024;

/// Text area of the reader screen, in px: the panel minus side margins, and
/// minus the top bar, reader header, page bar, tab bar and padding.
/// The UI owns the real layout; this is the budget pagination plans for.
pub const PAGE_WIDTH_PX: u32 = 758 - 2 * 44;
pub const PAGE_HEIGHT_PX: u32 = 1024 - 56 - 68 - 76 - 92 - 32;

/// Characters per line and lines per page for a font size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    columns: usize,
    rows: usize,
}

impl Layout {
    /// Conservative: IBM Plex averages ~0.5em per Latin glyph, and IBM Plex
    /// Sans Arabic's tall vertical metrics give a ~1.5em line (budget 1.6em). Slint still wraps any line that overflows,
    /// and the page scrolls if text ever exceeds the estimate.
    pub fn for_font(font: FontSize) -> Self {
        let px = u32::from(font.px());
        Self {
            columns: (PAGE_WIDTH_PX * 2 / px) as usize,
            rows: (PAGE_HEIGHT_PX * 10 / (px * 16)) as usize,
        }
    }
    #[cfg(test)]
    fn new(columns: usize, rows: usize) -> Self {
        Self { columns, rows }
    }
}

pub struct Reader {
    pub title: String,
    source: String,
    lines: Vec<String>,
    rows: usize,
    page: usize,
}

impl Reader {
    pub fn open(path: &Path, layout: Layout) -> std::io::Result<Self> {
        use std::io::Read;
        if Format::of(path) == Some(Format::Epub) {
            return Self::open_epub(path, layout);
        }
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(MAX_BOOK_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BOOK_BYTES {
            return Err(std::io::Error::other("text file exceeds 4 MiB limit"));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| std::io::Error::other("book must be UTF-8 text"))?;
        let title = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .replace(['_', '-'], " ");
        Ok(Self::from_text(&title, &text, layout))
    }

    fn open_epub(path: &Path, layout: Layout) -> std::io::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        if file.metadata()?.len() > 16 * 1024 * 1024 {
            return Err(std::io::Error::other("EPUB exceeds 16 MiB device budget"));
        }
        {
            let mut archive = zip::ZipArchive::new(&mut file).map_err(std::io::Error::other)?;
            if archive.len() > 2048 {
                return Err(std::io::Error::other("EPUB has too many entries"));
            }
            // Refuse zip bombs before any XML is parsed.
            (0..archive.len()).try_fold(0u64, |total, i| {
                let size = archive.by_index(i).map_err(std::io::Error::other)?.size();
                total
                    .checked_add(size)
                    .filter(|t| size <= MAX_BOOK_BYTES && *t <= 16 * 1024 * 1024)
                    .ok_or_else(|| {
                        std::io::Error::other("EPUB expanded size exceeds device budget")
                    })
            })?;
        }
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))?;
        let epub = rbook::Epub::read(file).map_err(std::io::Error::other)?;
        let text = epub.reader().try_fold(String::new(), |mut text, chapter| {
            let chapter = chapter.map_err(std::io::Error::other)?;
            let plain = html2text::from_read(chapter.content().as_bytes(), layout.columns.max(1))
                .map_err(std::io::Error::other)?;
            if text.len() + plain.len() > MAX_BOOK_BYTES as usize {
                return Err(std::io::Error::other("EPUB text exceeds device budget"));
            }
            text.push_str(&plain);
            text.push_str("\n\n");
            Ok(text)
        })?;
        let title = epub.metadata().title().map(|t| t.value()).unwrap_or("EPUB");
        Ok(Self::from_text(title, &text, layout))
    }

    pub fn from_text(title: &str, text: &str, layout: Layout) -> Self {
        let mut lines: Vec<String> = text
            .lines()
            .flat_map(|paragraph| textwrap::wrap(paragraph, layout.columns.max(1)))
            .map(|line| line.into_owned())
            .collect();
        if lines.is_empty() {
            lines.push(String::new());
        }
        Self {
            title: title.into(),
            source: text.into(),
            lines,
            rows: layout.rows.max(1),
            page: 0,
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
    pub fn next(&mut self) {
        self.page = (self.page + 1).min(self.pages() - 1);
    }
    pub fn previous(&mut self) {
        self.page = self.page.saturating_sub(1);
    }
    pub fn progress(&self) -> Progress {
        Progress::of(self.page, self.pages())
    }
    pub fn seek(&mut self, progress: Progress) {
        self.page = progress.page(self.pages());
    }
    /// Repaginate for a new font, keeping the reading position.
    pub fn relayout(&mut self, layout: Layout) {
        let progress = self.progress();
        *self = Self::from_text(&self.title, &self.source, layout);
        self.seek(progress);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Epub,
    Text,
}

impl Format {
    fn of(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "epub" => Some(Self::Epub),
            "txt" => Some(Self::Text),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Epub => "EPUB",
            Self::Text => "Text",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    pub path: PathBuf,
    pub format: Format,
    pub bytes: u64,
}

impl Book {
    /// The file name: the key for saved positions and file actions.
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
    pub fn title(&self) -> String {
        self.path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .replace(['_', '-'], " ")
    }
}

/// Deterministic flat library; does not follow symlinks or scan the whole disk.
pub fn library(root: &Path) -> Vec<Book> {
    let mut books: Vec<Book> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            // DirEntry::file_type does not follow symlinks, so links are skipped.
            entry.file_type().ok().filter(|t| t.is_file())?;
            let path = entry.path();
            Some(Book {
                format: Format::of(&path)?,
                bytes: entry.metadata().ok()?.len(),
                path,
            })
        })
        .take(256)
        .collect();
    books.sort_by_key(|book| book.file_name().to_lowercase());
    books
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wraps_and_pages_without_losing_words() {
        let mut r = Reader::from_text(
            "test",
            "one two three\nabcdefghijk\nlast",
            Layout::new(5, 2),
        );
        assert!(r.lines.iter().all(|l| l.chars().count() <= 5));
        assert_eq!(r.visible(), &["one", "two"]);
        (0..100).for_each(|_| r.next());
        assert_eq!(r.page(), r.pages() - 1);
        (0..100).for_each(|_| r.previous());
        assert_eq!(r.page(), 0);
    }
    #[test]
    fn relayout_keeps_the_position() {
        let text = "word ".repeat(5000);
        let mut r = Reader::from_text("t", &text, Layout::for_font(FontSize::default()));
        r.seek(Progress::try_from(0.5).unwrap());
        r.relayout(Layout::for_font(FontSize::default().larger().larger()));
        let progress = f32::from(r.progress());
        assert!((0.45..=0.55).contains(&progress), "{progress}");
    }
    #[test]
    fn empty_text_has_one_page() {
        let r = Reader::from_text("empty", "", Layout::new(10, 10));
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
        let book = Reader::open(&path, Layout::new(48, 23)).unwrap();
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
        let error = Reader::open(&path, Layout::new(48, 23)).err().unwrap();
        assert!(error.to_string().contains("expanded size"));
    }
}

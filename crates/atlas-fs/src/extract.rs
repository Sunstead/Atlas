//! Searchable text from a file. Capped twice: files over a size limit aren't
//! opened at all, and the text kept from any file stops at
//! [`MAX_TEXT_BYTES`]. PDF parsing runs under `catch_unwind`, because a
//! malformed PDF can panic the parser and must not take a sync down with it.

use std::io::Read;
use std::path::Path;

/// Text kept per file. Enough for search and snippets; the index stays small.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;

/// Text files larger than this are indexed by name only.
const MAX_TEXT_FILE: u64 = 8 * 1024 * 1024;
/// PDFs larger than this are indexed by name only.
const MAX_PDF_FILE: u64 = 64 * 1024 * 1024;

/// How a file's content can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    Markdown,
    Plain,
    Pdf,
    Image,
    Video,
    Audio,
    /// Nothing Atlas can read yet (office documents, archives, binaries).
    Opaque,
}

const PLAIN: &[&str] = &[
    "txt", "text", "log", "csv", "tsv", "json", "jsonl", "yaml", "yml", "toml", "ini", "cfg", "conf", "xml", "html",
    "htm", "css", "js", "mjs", "ts", "tsx", "jsx", "rs", "py", "go", "java", "kt", "c", "h", "cpp", "hpp", "cs", "rb",
    "php", "sh", "bash", "zsh", "ps1", "sql", "lua", "swift", "tex", "bib", "org", "rst", "adoc", "srt", "vtt", "ics",
    "vcf", "canvas",
];

pub fn kind_for(path: &Path) -> TextKind {
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "md" | "markdown" => TextKind::Markdown,
        "pdf" => TextKind::Pdf,
        e if PLAIN.contains(&e) => TextKind::Plain,
        _ => match mime_guess::from_path(path).first().map(|m| m.type_().as_str().to_owned()).as_deref() {
            Some("image") => TextKind::Image,
            Some("video") => TextKind::Video,
            Some("audio") => TextKind::Audio,
            Some("text") => TextKind::Plain,
            _ => TextKind::Opaque,
        },
    }
}

/// The MIME type for a file name (`application/octet-stream` when unknown).
pub fn mime_for(path: &Path) -> String {
    mime_guess::from_path(path).first_or_octet_stream().essence_str().to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub mime: String,
    pub kind: TextKind,
    /// `None` when the content isn't searchable (or was too big to read).
    pub text: Option<String>,
}

/// Cuts `s` to at most `max` bytes, on a character boundary.
pub(crate) fn cap(mut s: String, max: usize) -> String {
    if s.len() > max {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

fn read_capped(path: &Path, max: usize) -> std::io::Result<String> {
    let mut buf = Vec::with_capacity(max.min(64 * 1024));
    std::fs::File::open(path)?.take(max as u64).read_to_end(&mut buf)?;
    Ok(cap(String::from_utf8_lossy(&buf).into_owned(), max))
}

/// Markdown to the words a reader sees: no syntax, no link targets.
fn markdown_text(md: &str) -> String {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    let mut out = String::with_capacity(md.len());
    for event in Parser::new(md) {
        match event {
            Event::Text(t) | Event::Code(t) => out.push_str(&t),
            Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::Item) => {
                out.push('\n')
            }
            Event::Start(Tag::Item) => out.push_str("- "),
            _ => {}
        }
    }
    out
}

fn pdf_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let result = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes));
    match result {
        Ok(Ok(text)) => Some(text),
        Ok(Err(e)) => {
            tracing::debug!(path = %path.display(), error = %e, "can't read PDF text");
            None
        }
        Err(_) => {
            tracing::warn!(path = %path.display(), "PDF parser panicked; indexing by name only");
            None
        }
    }
}

/// Reads what's searchable from a file. Errors only when the file can't be
/// read at all; unreadable content just yields `text: None`.
pub fn extract(path: &Path) -> std::io::Result<Extracted> {
    let kind = kind_for(path);
    let mime = mime_for(path);
    let size = std::fs::metadata(path)?.len();

    let text = match kind {
        TextKind::Markdown if size <= MAX_TEXT_FILE => Some(markdown_text(&read_capped(path, MAX_TEXT_BYTES * 2)?)),
        TextKind::Plain if size <= MAX_TEXT_FILE => Some(read_capped(path, MAX_TEXT_BYTES)?),
        TextKind::Pdf if size <= MAX_PDF_FILE => pdf_text(path),
        _ => None,
    };
    let text = text.map(|t| cap(t, MAX_TEXT_BYTES)).filter(|t| !t.trim().is_empty());
    Ok(Extracted { mime, kind, text })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn markdown_loses_its_syntax() {
        let t = markdown_text("# Taxes\n\nSee [the form](https://irs.example) and `1040`.\n\n- one\n- two\n");
        assert!(t.contains("Taxes"));
        assert!(t.contains("See the form and 1040."));
        assert!(!t.contains("irs.example"));
        assert!(t.contains("- one"));
    }

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_for(Path::new("a.MD")), TextKind::Markdown);
        assert_eq!(kind_for(Path::new("a.rs")), TextKind::Plain);
        assert_eq!(kind_for(Path::new("a.pdf")), TextKind::Pdf);
        assert_eq!(kind_for(Path::new("a.jpg")), TextKind::Image);
        assert_eq!(kind_for(Path::new("a.mp4")), TextKind::Video);
        assert_eq!(kind_for(Path::new("a.docx")), TextKind::Opaque);
        assert_eq!(kind_for(Path::new("README")), TextKind::Opaque);
    }

    #[test]
    fn caps_on_a_char_boundary() {
        assert_eq!(cap("héllo".into(), 2), "h");
        assert_eq!(cap("abc".into(), 10), "abc");
    }

    #[test]
    fn extracts_text_and_skips_binaries() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "hello world").unwrap();
        fs::write(d.path().join("b.png"), [0x89, b'P', b'N', b'G']).unwrap();
        fs::write(d.path().join("empty.txt"), "   ").unwrap();
        let a = extract(&d.path().join("a.txt")).unwrap();
        assert_eq!(a.text.as_deref(), Some("hello world"));
        assert_eq!(a.mime, "text/plain");
        let b = extract(&d.path().join("b.png")).unwrap();
        assert_eq!(b.kind, TextKind::Image);
        assert_eq!(b.text, None);
        assert_eq!(extract(&d.path().join("empty.txt")).unwrap().text, None);
    }

    #[test]
    fn a_broken_pdf_is_name_only() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("broken.pdf"), b"%PDF-1.4 this is not really a pdf").unwrap();
        let e = extract(&d.path().join("broken.pdf")).unwrap();
        assert_eq!(e.kind, TextKind::Pdf);
        assert_eq!(e.text, None);
    }

    #[test]
    fn big_text_is_capped() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("big.txt"), "x".repeat(MAX_TEXT_BYTES + 10)).unwrap();
        assert_eq!(extract(&d.path().join("big.txt")).unwrap().text.unwrap().len(), MAX_TEXT_BYTES);
    }
}

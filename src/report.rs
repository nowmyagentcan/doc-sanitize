//! The report every face prints: what each check found in each file, as one JSON shape.

use serde::{Serialize, Serializer};

/// The checks, in the order every `checks` array and every report lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckId {
    Metadata,
    Comments,
    TrackedChanges,
    HiddenText,
    FakeRedactions,
    HiddenLayers,
    HiddenSheets,
    HiddenRowsColumns,
    HiddenSlides,
    SpeakerNotes,
    HiddenObjects,
    EmbeddedFiles,
    CustomData,
    Macros,
    ExternalLinks,
    ImageMetadata,
    CroppedImages,
    PivotCaches,
    OldRevisions,
    LeftoverData,
    Signatures,
    FormData,
}

/// Each check's id and the line `--help` gives it, in check order.
pub const CHECKS: [(&str, &str); 22] = [
    (
        "metadata",
        "properties, author names, thumbnails, editing ids",
    ),
    ("comments", "comments and annotations, and their authors"),
    (
        "tracked-changes",
        "insertions and deletions (with the deleted text), shared-workbook logs",
    ),
    (
        "hidden-text",
        "hidden, white, tiny, clipped or off-page text",
    ),
    (
        "fake-redactions",
        "text still under black boxes, images or unapplied redaction marks",
    ),
    ("hidden-layers", "PDF layers that are switched off"),
    ("hidden-sheets", "hidden and very hidden sheets"),
    (
        "hidden-rows-columns",
        "hidden rows and columns that hold values",
    ),
    ("hidden-slides", "slides hidden from the show"),
    ("speaker-notes", "notes under slides"),
    ("hidden-objects", "hidden shapes, and shapes off the slide"),
    (
        "embedded-files",
        "attachments, OLE objects, chart workbooks",
    ),
    (
        "custom-data",
        "custom XML, document variables, tags, add-in data",
    ),
    (
        "macros",
        "VBA, ActiveX, Excel 4.0 sheets, DDE fields, PDF scripts and actions",
    ),
    (
        "external-links",
        "paths to local or network files, templates, data connections",
    ),
    ("image-metadata", "EXIF, GPS, XMP and comments in pictures"),
    ("cropped-images", "picture areas hidden by cropping"),
    ("pivot-caches", "pivot table source data and data models"),
    ("old-revisions", "earlier saved versions inside a PDF"),
    (
        "leftover-data",
        "unused objects, orphan parts, bytes outside the file",
    ),
    (
        "signatures",
        "digital signatures (any change breaks them, so clean removes them)",
    ),
    ("form-data", "XFA forms and values of hidden form fields"),
];

impl CheckId {
    pub const ALL: [CheckId; 22] = {
        use CheckId::*;
        [
            Metadata,
            Comments,
            TrackedChanges,
            HiddenText,
            FakeRedactions,
            HiddenLayers,
            HiddenSheets,
            HiddenRowsColumns,
            HiddenSlides,
            SpeakerNotes,
            HiddenObjects,
            EmbeddedFiles,
            CustomData,
            Macros,
            ExternalLinks,
            ImageMetadata,
            CroppedImages,
            PivotCaches,
            OldRevisions,
            LeftoverData,
            Signatures,
            FormData,
        ]
    };

    pub fn id(self) -> &'static str {
        CHECKS[self as usize].0
    }

    /// The check with the id `id`.
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.id() == id)
    }
}

impl Serialize for CheckId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.id())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Found,
    NoneFound,
    Incomplete,
    Error,
    /// A re-check's check that the person chose to keep.
    Kept,
}

/// Where a finding is. Every field is a place in the file's structure, never a found value.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Where {
    /// 1-based page number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    /// `[x0, y0, x1, y1]` in points in the page's default user space, rounded to 0.1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bbox: Option<[f64; 4]>,
    /// A PDF object as `"12 0"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object: Option<String>,
    /// 1-based PDF revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    /// 1-based position in the workbook's sheet list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    /// The A1 range of the found cells.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cells: Option<String>,
    /// 1-based position in the presentation's slide list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slide: Option<usize>,
    /// A property or key name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl Where {
    /// On page `n`.
    pub fn page(n: usize) -> Self {
        Self {
            page: Some(n),
            ..Self::default()
        }
    }

    /// At the PDF object `number generation`.
    pub fn object((number, generation): (u32, u16)) -> Self {
        Self {
            object: Some(format!("{number} {generation}")),
            ..Self::default()
        }
    }

    /// Named by the property or key `name`.
    pub fn field(name: &str) -> Self {
        Self {
            field: Some(safe(name)),
            ..Self::default()
        }
    }

    /// In the part `name`.
    pub fn part(name: &str) -> Self {
        Self {
            part: Some(safe(name)),
            ..Self::default()
        }
    }

    /// How people read the place: `page 3`, `slide 2`, `sheet Summary, F1:F40`, `sheet 3`,
    /// `revision 1`, `object 12 0`, the field, or the part when there is no page, slide or
    /// sheet.
    pub fn shown(&self) -> String {
        let sheet = match (&self.sheet, self.sheet_index) {
            (Some(name), _) => Some(format!("sheet {name}")),
            (None, Some(i)) => Some(format!("sheet {i}")),
            _ => None,
        };
        let part = self.part.clone().filter(|_| position(self) == 0);
        [
            self.page.map(|p| format!("page {p}")),
            self.slide.map(|s| format!("slide {s}")),
            sheet,
            self.cells.clone(),
            self.revision.map(|r| format!("revision {r}")),
            self.object.as_ref().map(|o| format!("object {o}")),
            self.field.clone(),
            part,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    }
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub check: CheckId,
    pub kind: &'static str,
    #[serde(rename = "where")]
    pub place: Where,
    pub count: u64,
    pub unit: &'static str,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

impl Finding {
    pub fn new(
        check: CheckId,
        kind: &'static str,
        place: Where,
        count: u64,
        unit: &'static str,
        detail: String,
    ) -> Self {
        Self {
            check,
            kind,
            place,
            count,
            unit,
            detail,
            value: None,
        }
    }

    /// The finding with `text` as its value when values are shown.
    pub fn with_value(mut self, values: bool, text: Option<&str>) -> Self {
        self.value = text.filter(|_| values).map(value);
        self
    }
}

/// The page, slide or sheet a place is on; 0 for the file as a whole.
fn position(w: &Where) -> usize {
    w.page.or(w.slide).or(w.sheet_index).unwrap_or(0)
}

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub id: CheckId,
    pub status: Status,
    pub count: usize,
    /// Why the check couldn't look at everything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A cleaned copy, before the re-check.
#[derive(Default)]
pub struct Cleaned {
    pub bytes: Vec<u8>,
    pub variant: &'static str,
    pub changes: Vec<Change>,
    pub warnings: Vec<Warning>,
    /// Checks the clean had to leave in place beyond the kept ones.
    pub kept: Vec<CheckId>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Warning {
    pub code: &'static str,
    pub message: String,
}

impl Warning {
    pub fn repaired() -> Self {
        Self {
            code: "repaired",
            message: "The file's cross-reference table was damaged and was rebuilt; other apps may show different content.".into(),
        }
    }

    pub fn encryption_removed() -> Self {
        Self {
            code: "encryption_removed",
            message: "The copy has no password or permission settings; the original had them."
                .into(),
        }
    }

    pub fn pdfa_title_removed() -> Self {
        Self {
            code: "pdfa_title_removed",
            message:
                "The copy is still marked PDF/A, but its title was removed, which PDF/UA requires."
                    .into(),
        }
    }
}

/// A file that couldn't be checked, or a usage error. Messages name the file as `{name}`
/// until [`Error::named`] fills it in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}

const REPORT_BUG: &str =
    "Run again with --debug and report it at https://github.com/nowmyagentcan/doc-sanitize/issues.";
const NEW_COPY: &str = "Open it in the app that made it and save a new copy.";

impl Error {
    fn new(code: &'static str, message: impl Into<String>, hint: &str) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.into(),
        }
    }

    /// The message with the file's name in place of each `{name}`.
    pub fn named(mut self, name: &str) -> Self {
        self.message = self.message.split("{name}").collect::<Vec<_>>().join(name);
        self
    }

    pub fn not_found() -> Self {
        Self::new("not_found", "Can't find {name}.", "Check the path.")
    }

    pub fn unreadable(reason: impl std::fmt::Display) -> Self {
        let message = format!("Can't read {{name}}: {}.", safe(&reason.to_string()));
        Self::new("unreadable", message, "Check that you can open the file.")
    }

    pub fn unsupported_format() -> Self {
        Self::new(
            "unsupported_format",
            "{name} isn't a PDF, Word, Excel or PowerPoint file.",
            "doc-sanitize reads .pdf, .docx, .xlsx and .pptx files, and their macro and template versions.",
        )
    }

    pub fn empty_folder() -> Self {
        Self::new(
            "unsupported_format",
            "No PDF, Word, Excel or PowerPoint files in {name}.",
            "Pass files directly, or use -r for subfolders.",
        )
    }

    pub fn legacy_office() -> Self {
        Self::new(
            "legacy_office",
            "{name} is an older Office file (.doc, .xls or .ppt).",
            "Save it as .docx, .xlsx or .pptx in Office, then check the new file.",
        )
    }

    pub fn strict() -> Self {
        Self::new(
            "unsupported_variant",
            "{name} is saved as Strict Open XML, which this version can't read.",
            "Save it in Office as a normal .docx, .xlsx or .pptx.",
        )
    }

    pub fn xlsb() -> Self {
        Self::new(
            "unsupported_variant",
            "{name} is a binary Excel workbook (.xlsb).",
            "Save it as .xlsx in Excel.",
        )
    }

    pub fn office_password() -> Self {
        Self::new(
            "password_required",
            "{name} is password-protected.",
            "Remove the password in Office (File › Info › Protect), save, and check again.",
        )
    }

    pub fn pdf_password() -> Self {
        Self::new(
            "password_required",
            "{name} needs a password to open.",
            "Pass it with --password.",
        )
    }

    pub fn wrong_password() -> Self {
        Self::new(
            "wrong_password",
            "The password for {name} isn't right.",
            "Check the password and try again.",
        )
    }

    pub fn unsupported_encryption(method: &str) -> Self {
        Self::new(
            "unsupported_encryption",
            format!(
                "{{name}} uses encryption this tool can't open ({}).",
                safe(method)
            ),
            "Save an unencrypted copy from the app that made it.",
        )
    }

    pub fn damaged(reason: &str) -> Self {
        Self::new(
            "damaged",
            format!("{{name}} is damaged: {reason}."),
            NEW_COPY,
        )
    }

    /// `damaged` because a part is not well-formed XML.
    pub fn bad_xml(part: &str) -> Self {
        Self::damaged(&format!("part {} isn't valid XML", safe(part)))
    }

    pub fn ambiguous(how: &str) -> Self {
        Self::new(
            "ambiguous_file",
            format!("{{name}} is ambiguous: {how}."),
            "This file may have been altered to hide content. Ask for a fresh copy.",
        )
    }

    pub fn two_versions(part: &str) -> Self {
        Self {
            message: format!(
                "{{name}} holds two different versions of {}, so apps may show either one.",
                safe(part)
            ),
            ..Self::ambiguous("")
        }
    }

    pub fn too_large(what: &str, limit: &str) -> Self {
        Self::new(
            "too_large",
            format!("{{name}} is too large to check ({what} is over {limit})."),
            "Check a smaller copy, or split the file.",
        )
    }

    pub fn internal() -> Self {
        Self::new(
            "internal",
            "Something went wrong inside doc-sanitize while reading {name}.",
            REPORT_BUG,
        )
    }

    pub fn usage(message: impl Into<String>, hint: &str) -> Self {
        Self::new("usage", message, hint)
    }

    /// `cannot_clean`, one line per blocking check: (check, reason, hint).
    pub fn cannot_clean(blocks: &[(CheckId, String, String)]) -> Self {
        let message: Vec<_> = blocks
            .iter()
            .map(|(c, reason, _)| format!("Can't remove {} from {{name}}: {reason}.", c.id()))
            .collect();
        let hint: Vec<_> = blocks.iter().map(|b| b.2.as_str()).collect();
        Self::new("cannot_clean", message.join("\n"), &hint.join("\n"))
    }

    pub fn recheck_failed(check: CheckId) -> Self {
        Self::new(
            "recheck_failed",
            format!(
                "The cleaned copy of {{name}} still held {}, so it wasn't saved.",
                check.id()
            ),
            "This is a bug. Please report it at https://github.com/nowmyagentcan/doc-sanitize/issues with the --json output (no file needed).",
        )
    }

    pub fn output_exists(output: &str) -> Self {
        Self::new(
            "output_exists",
            format!("{} already exists.", safe(output)),
            "Pass --overwrite to replace it, or choose another name with -o.",
        )
    }

    pub fn write_failed(output: &str, reason: impl std::fmt::Display) -> Self {
        Self::new(
            "write_failed",
            format!(
                "Can't write {}: {}.",
                safe(output),
                safe(&reason.to_string())
            ),
            "Check that the folder exists and you can write to it.",
        )
    }

    pub fn exit_code(&self) -> u8 {
        match self.code {
            "usage" => 2,
            "internal" | "write_failed" => 3,
            _ => 1,
        }
    }

    /// A usage error as `--json` prints it.
    pub fn to_json(&self) -> String {
        #[derive(Serialize)]
        struct Usage<'a> {
            error: &'a Error,
        }
        serde_json::to_string_pretty(&Usage { error: self }).expect("an error always serialises")
    }
}

/// One checked file.
#[derive(Debug, Serialize)]
pub struct FileResult {
    pub path: String,
    pub format: Option<Format>,
    pub variant: Option<&'static str>,
    pub status: Status,
    pub checks: Vec<CheckResult>,
    pub findings: Vec<Finding>,
    pub warnings: Vec<Warning>,
    pub error: Option<Error>,
}

impl FileResult {
    /// A file whose checks ran: `ran` lists them in check order, `findings` holds what they
    /// found in any order.
    pub fn checked(
        path: String,
        format: Format,
        variant: &'static str,
        ran: &[CheckId],
        mut findings: Vec<Finding>,
        incomplete: &[(CheckId, String)],
        warnings: Vec<Warning>,
    ) -> Self {
        findings.sort_by_key(|f| (f.check, position(&f.place)));
        let checks: Vec<_> = ran
            .iter()
            .map(|&id| {
                let count = findings.iter().filter(|f| f.check == id).count();
                let notes: Vec<_> = incomplete
                    .iter()
                    .filter(|n| n.0 == id)
                    .map(|n| n.1.as_str())
                    .collect();
                let note = (!notes.is_empty()).then(|| notes.join("; "));
                let status = match (count, &note) {
                    (1.., _) => Status::Found,
                    (0, Some(_)) => Status::Incomplete,
                    (0, None) => Status::NoneFound,
                };
                CheckResult {
                    id,
                    status,
                    count,
                    note,
                }
            })
            .collect();
        let status = if !findings.is_empty() {
            Status::Found
        } else if checks.iter().any(|c| c.note.is_some()) {
            Status::Incomplete
        } else {
            Status::NoneFound
        };
        Self {
            path,
            format: Some(format),
            variant: Some(variant),
            status,
            checks,
            findings,
            warnings,
            error: None,
        }
    }

    /// A file that couldn't be checked; the error's `{name}` becomes `path`.
    pub fn failed(path: String, error: Error) -> Self {
        Self {
            error: Some(error.named(&path)),
            path,
            format: None,
            variant: None,
            status: Status::Error,
            checks: vec![],
            findings: vec![],
            warnings: vec![],
        }
    }

    /// 0 when every check is complete and found nothing, 3 for an internal error, else 1.
    pub fn exit_code(&self) -> u8 {
        match &self.error {
            Some(e) => e.exit_code(),
            None if self.status == Status::NoneFound => 0,
            None => 1,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub files: usize,
    pub with_findings: usize,
    pub incomplete: usize,
    pub failed: usize,
}

/// The whole run, as `--json` prints it.
#[derive(Debug, Serialize)]
pub struct Report<'a> {
    pub version: &'static str,
    pub command: &'static str,
    pub files: &'a [FileResult],
    pub summary: Summary,
}

impl<'a> Report<'a> {
    pub fn check(files: &'a [FileResult]) -> Self {
        let count = |s| files.iter().filter(|f| f.status == s).count();
        Self {
            version: env!("CARGO_PKG_VERSION"),
            command: "check",
            files,
            summary: Summary {
                files: files.len(),
                with_findings: count(Status::Found),
                incomplete: count(Status::Incomplete),
                failed: count(Status::Error),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanStatus {
    Cleaned,
    CleanedIncomplete,
    NotCleaned,
}

/// What clean removed of one check, in one unit.
#[derive(Debug, Serialize)]
pub struct Removed {
    pub check: CheckId,
    pub count: u64,
    pub unit: &'static str,
    pub detail: String,
}

/// Something about the copy that a reader will see differently.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Change {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<usize>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slides: Option<Vec<usize>>,
    pub detail: String,
}

impl Change {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            count: None,
            pages: None,
            slides: None,
            detail: detail.into(),
        }
    }

    pub fn counted(code: &'static str, n: u64, detail: impl Into<String>) -> Self {
        Self {
            count: Some(n),
            ..Self::new(code, detail)
        }
    }
}

/// The check of the copy: kept checks show as `kept`, and the status leaves them out.
#[derive(Debug, Serialize)]
pub struct Recheck {
    pub status: Status,
    pub checks: Vec<CheckResult>,
    pub findings: Vec<Finding>,
}

impl Recheck {
    /// `copy`'s check with the `kept` checks marked; its findings are those of the rest.
    pub fn of(copy: FileResult, kept: &[CheckId]) -> Self {
        let mut checks = copy.checks;
        for c in &mut checks {
            if kept.contains(&c.id) {
                c.status = Status::Kept;
            }
        }
        let findings: Vec<_> = copy
            .findings
            .into_iter()
            .filter(|f| !kept.contains(&f.check))
            .collect();
        let status = if !findings.is_empty() {
            Status::Found
        } else if checks.iter().any(|c| c.status == Status::Incomplete) {
            Status::Incomplete
        } else {
            Status::NoneFound
        };
        Self {
            status,
            checks,
            findings,
        }
    }
}

/// One cleaned (or not cleaned) file.
#[derive(Debug, Serialize)]
pub struct CleanResult {
    pub path: String,
    pub format: Option<Format>,
    pub variant: Option<&'static str>,
    pub output: Option<String>,
    pub status: CleanStatus,
    pub removed: Vec<Removed>,
    pub kept: Vec<CheckId>,
    pub changes: Vec<Change>,
    pub recheck: Option<Recheck>,
    pub warnings: Vec<Warning>,
    pub error: Option<Error>,
}

impl CleanResult {
    /// A file that wasn't cleaned, with what was known about it.
    pub fn failed(checked: FileResult, error: Error) -> Self {
        Self {
            error: Some(error.named(&checked.path)),
            path: checked.path,
            format: checked.format,
            variant: checked.variant,
            output: None,
            status: CleanStatus::NotCleaned,
            removed: vec![],
            kept: vec![],
            changes: vec![],
            recheck: None,
            warnings: checked.warnings,
        }
    }

    /// 0 when cleaned, 3 for write and internal errors, else 1.
    pub fn exit_code(&self) -> u8 {
        match (&self.error, self.status) {
            (Some(e), _) => e.exit_code(),
            (None, CleanStatus::Cleaned) => 0,
            _ => 1,
        }
    }
}

/// The input's findings summed per check and unit, the kept checks left out.
pub fn removed(findings: &[Finding], kept: &[CheckId]) -> Vec<Removed> {
    let mut out: Vec<Removed> = vec![];
    for f in findings.iter().filter(|f| !kept.contains(&f.check)) {
        match out
            .iter_mut()
            .find(|r| r.check == f.check && r.unit == f.unit)
        {
            Some(r) => r.count += f.count,
            None => out.push(Removed {
                check: f.check,
                count: f.count,
                unit: f.unit,
                detail: String::new(),
            }),
        }
    }
    for r in &mut out {
        r.detail = format!("{} removed", quantity(r.count, r.unit));
    }
    out
}

/// `1 sheet`, `3 sheets`, `1 person`: a count with its unit, singular for one.
pub fn quantity(n: u64, unit: &str) -> String {
    let one = match unit {
        "people" => "person",
        u => u.strip_suffix('s').unwrap_or(u),
    };
    counted(n, one, unit)
}

#[derive(Debug, Serialize)]
pub struct CleanSummary {
    pub files: usize,
    pub cleaned: usize,
    pub cleaned_incomplete: usize,
    pub not_cleaned: usize,
}

/// A clean run, as `--json` prints it.
#[derive(Debug, Serialize)]
pub struct CleanReport<'a> {
    pub version: &'static str,
    pub command: &'static str,
    pub files: &'a [CleanResult],
    pub summary: CleanSummary,
}

impl<'a> CleanReport<'a> {
    pub fn new(files: &'a [CleanResult]) -> Self {
        let count = |s| files.iter().filter(|f| f.status == s).count();
        Self {
            version: env!("CARGO_PKG_VERSION"),
            command: "clean",
            files,
            summary: CleanSummary {
                files: files.len(),
                cleaned: count(CleanStatus::Cleaned),
                cleaned_incomplete: count(CleanStatus::CleanedIncomplete),
                not_cleaned: count(CleanStatus::NotCleaned),
            },
        }
    }
}

/// `s` safe for a terminal: control characters (tab and newline aside) and the bidi controls
/// that can disguise text as `<U+XXXX>`.
pub fn safe(s: &str) -> String {
    s.chars().map(shown).collect()
}

fn shown(c: char) -> String {
    match c {
        '\n' | '\t' => c.into(),
        '\0'..='\x1F'
        | '\x7F'..='\u{9F}'
        | '\u{61C}'
        | '\u{200E}'
        | '\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}' => format!("<U+{:04X}>", c as u32),
        c => c.into(),
    }
}

/// A found value as `--show-values` prints it: terminal-safe, at most 200 characters, cut on a
/// character boundary with `…`.
pub fn value(s: &str) -> String {
    const MAX: usize = 200;
    let shown: Vec<String> = s.chars().map(shown).collect();
    if shown.iter().map(|t| t.chars().count()).sum::<usize>() <= MAX {
        return shown.concat();
    }
    let mut out = String::new();
    let mut len = 0;
    for t in shown {
        len += t.chars().count();
        if len > MAX - 1 {
            break;
        }
        out += &t;
    }
    out + "…"
}

/// `n` with thousands separators: 1284 → `1,284`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d);
    }
    out
}

/// `1 string`, `2 strings`, `1,284 strings`.
pub fn plural(n: u64, word: &str) -> String {
    counted(n, word, &format!("{word}s"))
}

/// `n` with the singular or plural word: `1 property`, `3 properties`.
pub fn counted(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(check: CheckId, part: Option<&str>, value: Option<&str>) -> Finding {
        Finding {
            check,
            kind: "orphan_parts",
            place: Where {
                part: part.map(Into::into),
                ..Where::default()
            },
            count: 1,
            unit: "parts",
            detail: "1 part that nothing in the file links to".into(),
            value: value.map(Into::into),
        }
    }

    #[test]
    fn json_keeps_spec_key_order_and_leaves_out_empty_fields() {
        let file = FileResult::checked(
            "a.docx".into(),
            Format::Docx,
            "docm",
            &[CheckId::LeftoverData],
            vec![finding(CheckId::LeftoverData, None, None)],
            &[],
            vec![],
        );
        let files = [file];
        let json = serde_json::to_string(&Report::check(&files)).unwrap();
        assert_eq!(
            json,
            r#"{"version":"0.1.0","command":"check","files":[{"path":"a.docx","format":"docx","variant":"docm","status":"found","checks":[{"id":"leftover-data","status":"found","count":1}],"findings":[{"check":"leftover-data","kind":"orphan_parts","where":{},"count":1,"unit":"parts","detail":"1 part that nothing in the file links to"}],"warnings":[],"error":null}],"summary":{"files":1,"with_findings":1,"incomplete":0,"failed":0}}"#
        );
    }

    #[test]
    fn a_failed_file_names_itself_and_has_no_checks() {
        let f = FileResult::failed("x.odt".into(), Error::unsupported_format());
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["format"], serde_json::Value::Null);
        assert_eq!(json["status"], "error");
        assert_eq!(
            json["error"]["message"],
            "x.odt isn't a PDF, Word, Excel or PowerPoint file."
        );
        assert_eq!(f.exit_code(), 1);
        let f = FileResult::failed("{name}.docx".into(), Error::internal());
        assert_eq!(
            f.error.as_ref().unwrap().message,
            "Something went wrong inside doc-sanitize while reading {name}.docx."
        );
        assert_eq!(f.exit_code(), 3);
    }

    #[test]
    fn values_and_parts_are_found_and_the_status_follows() {
        let none = FileResult::checked(
            "a.pptx".into(),
            Format::Pptx,
            "pptx",
            &[CheckId::LeftoverData],
            vec![],
            &[],
            vec![],
        );
        assert_eq!((none.status, none.exit_code()), (Status::NoneFound, 0));
        let notes = [
            (CheckId::HiddenText, "page 2: why".to_string()),
            (CheckId::HiddenText, "page 5: why not".to_string()),
        ];
        let ran = [CheckId::Metadata, CheckId::HiddenText];
        let part = FileResult::checked(
            "a.pdf".into(),
            Format::Pdf,
            "pdf",
            &ran,
            vec![],
            &notes,
            vec![],
        );
        assert_eq!((part.status, part.exit_code()), (Status::Incomplete, 1));
        assert_eq!(
            serde_json::to_string(&part.checks).unwrap(),
            r#"[{"id":"metadata","status":"none_found","count":0},{"id":"hidden-text","status":"incomplete","count":0,"note":"page 2: why; page 5: why not"}]"#
        );
        assert_eq!(Report::check(&[part]).summary.incomplete, 1);
        let f = finding(CheckId::LeftoverData, Some("ppt/x.bin"), Some("v"));
        assert_eq!(
            serde_json::to_string(&f).unwrap(),
            r#"{"check":"leftover-data","kind":"orphan_parts","where":{"part":"ppt/x.bin"},"count":1,"unit":"parts","detail":"1 part that nothing in the file links to","value":"v"}"#
        );
    }

    #[test]
    fn usage_errors_print_as_one_error_object() {
        let e = Error::usage("No file given.", "Try: doc-sanitize contract.docx");
        assert_eq!(
            e.to_json(),
            "{\n  \"error\": {\n    \"code\": \"usage\",\n    \"message\": \"No file given.\",\n    \"hint\": \"Try: doc-sanitize contract.docx\"\n  }\n}"
        );
        assert_eq!(e.exit_code(), 2);
    }

    #[test]
    fn controls_and_bidi_characters_show_as_code_points() {
        assert_eq!(
            safe("a\x1b[31m\r\0\x7f\u{9b}\u{202e}b\u{61c}\u{2069}\tc\n"),
            "a<U+001B>[31m<U+000D><U+0000><U+007F><U+009B><U+202E>b<U+061C><U+2069>\tc\n"
        );
        assert_eq!(safe("Grüße 東京 ✓"), "Grüße 東京 ✓");
    }

    #[test]
    fn values_are_cut_at_200_characters() {
        assert_eq!(value("short"), "short");
        let exact = "é".repeat(200);
        assert_eq!(value(&exact), exact);
        let cut = value(&"é".repeat(201));
        assert_eq!(cut.chars().count(), 200);
        assert!(cut.ends_with("é…"));
        let cut = value(&"\u{1}".repeat(100));
        assert_eq!(cut.chars().count(), 193);
        assert!(cut.ends_with("<U+0001>…"));
    }

    #[test]
    fn places_read_like_the_page_shows_them() {
        let at = |w: Where| w.shown();
        assert_eq!(at(Where::part("word/document.xml")), "word/document.xml");
        let sheet = Where {
            sheet_index: Some(3),
            part: Some("xl/worksheets/sheet3.xml".into()),
            ..Where::default()
        };
        assert_eq!(at(sheet.clone()), "sheet 3");
        let named = Where {
            sheet: Some("Summary".into()),
            cells: Some("F1:F40".into()),
            ..sheet
        };
        assert_eq!(at(named), "sheet Summary, F1:F40");
        let slide = Where {
            slide: Some(2),
            ..Where::part("ppt/slides/slide2.xml")
        };
        assert_eq!(at(slide), "slide 2");
        let page = Where {
            bbox: Some([72.0, 640.2, 210.5, 652.0]),
            part: Some("ignored".into()),
            ..Where::page(3)
        };
        assert_eq!(at(page), "page 3");
        let revision = Where {
            revision: Some(1),
            ..Where::object((12, 0))
        };
        assert_eq!(at(revision), "revision 1, object 12 0");
        assert_eq!(at(Where::field("/Info")), "/Info");
        assert_eq!(at(Where::default()), "");
    }

    #[test]
    fn numbers_get_thousands_separators() {
        let shown: Vec<_> = [0, 7, 999, 1000, 1284, 1_000_000]
            .map(thousands)
            .into_iter()
            .collect();
        assert_eq!(shown, ["0", "7", "999", "1,000", "1,284", "1,000,000"]);
        assert_eq!(plural(1, "string"), "1 string");
        assert_eq!(plural(4096, "byte"), "4,096 bytes");
    }
}

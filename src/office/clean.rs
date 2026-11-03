//! Cleaning an Office package: the copy is built in memory as a list of parts, each class is
//! removed by its procedure in the spec's order, then the sweep deletes what nothing reaches,
//! the consistency check proves every reference still resolves, and the parts are written as a
//! new ZIP with fixed times.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use super::common::rels_source;
use super::tree::{Doc, El, Node};
use super::{
    CONTENT_TYPES, Ctx, MAIN_TYPES, Package, RELATIONSHIPS, Rel, common, drawing, excel, key, ns,
    part_name, powerpoint, rels_name, resolve, revisions, word,
};
use crate::report::{Change, CheckId, Error, Finding, Format, safe};

const TYPES: &str = "[Content_Types].xml";

/// The copy being built: its parts in the input's order and its content types.
pub struct Draft {
    pub format: Format,
    pub variant: &'static str,
    pub main: String,
    entries: Vec<Option<(String, Vec<u8>)>>,
    index: HashMap<String, usize>,
    types: Doc,
    /// What a reader of the copy will see differently.
    pub changes: Vec<Change>,
}

/// A relationship and the part whose `.rels` holds it.
#[derive(Debug, Clone)]
pub struct Link {
    pub source: String,
    pub rel: Rel,
    /// The internal target, when the copy has it.
    pub part: Option<String>,
}

impl Draft {
    pub fn new(pkg: &Package) -> Result<Self, Error> {
        let mut d = Self {
            format: pkg.format,
            variant: pkg.variant,
            main: pkg.main.clone(),
            entries: vec![],
            index: HashMap::new(),
            types: Doc::parse(&pkg.read(TYPES)?, TYPES)?,
            changes: vec![],
        };
        for e in &pkg.zip.entries {
            let folder = e.name.ends_with('/') && e.data.is_empty();
            if !folder && !e.name.eq_ignore_ascii_case(TYPES) {
                d.set(&e.name, pkg.read(&e.name)?);
            }
        }
        Ok(d)
    }

    /// The parts, in order.
    pub fn names(&self) -> Vec<String> {
        self.entries.iter().flatten().map(|e| e.0.clone()).collect()
    }

    pub fn has(&self, name: &str) -> bool {
        self.index.contains_key(&key(name))
    }

    pub fn bytes(&self, name: &str) -> Option<&[u8]> {
        let i = *self.index.get(&key(name))?;
        self.entries[i].as_ref().map(|e| e.1.as_slice())
    }

    /// Replaces a part's bytes, or adds the part at the end.
    pub fn set(&mut self, name: &str, bytes: Vec<u8>) {
        match self.index.get(&key(name)) {
            Some(&i) => {
                if let Some(e) = &mut self.entries[i] {
                    e.1 = bytes;
                }
            }
            None => {
                self.index.insert(key(name), self.entries.len());
                self.entries.push(Some((name.into(), bytes)));
            }
        }
    }

    /// Takes the part (and nothing else) out of the copy.
    pub fn remove(&mut self, name: &str) {
        if let Some(i) = self.index.remove(&key(name)) {
            self.entries[i] = None;
        }
    }

    /// A part as a tree.
    pub fn doc(&self, name: &str) -> Result<Option<Doc>, Error> {
        self.bytes(name).map(|b| Doc::parse(b, name)).transpose()
    }

    /// Edits the part `name` as a tree; `f` says whether it changed it, and only then is the
    /// part written back. Gives what `f` said (false for a missing part).
    pub fn edit(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut El) -> Result<bool, Error>,
    ) -> Result<bool, Error> {
        let Some(mut doc) = self.doc(name)? else {
            return Ok(false);
        };
        let changed = f(&mut doc.root)?;
        if changed {
            self.set(name, doc.write());
        }
        Ok(changed)
    }

    /// A part's content type: its Override, else the Default for its extension.
    pub fn content_type(&self, name: &str) -> Option<String> {
        let k = key(name);
        let ext = k.rsplit('/').next()?.rsplit_once('.').map(|(_, e)| e);
        let types = self.types.root.els();
        let mut default = None;
        for t in types {
            let ct = t.attr("", "ContentType").unwrap_or_default();
            if t.is(CONTENT_TYPES, "Override")
                && key(t
                    .attr("", "PartName")
                    .unwrap_or_default()
                    .trim_start_matches('/'))
                    == k
            {
                return Some(ct.into());
            }
            if t.is(CONTENT_TYPES, "Default")
                && ext.is_some_and(|e| {
                    t.attr("", "Extension")
                        .unwrap_or_default()
                        .eq_ignore_ascii_case(e)
                })
            {
                default = Some(ct.to_string());
            }
        }
        default
    }

    /// Changes the Override of `name` to `ct`.
    pub fn set_type(&mut self, name: &str, ct: &str) {
        let k = key(name);
        for t in self.types.root.els_mut() {
            if t.is(CONTENT_TYPES, "Override")
                && key(t
                    .attr("", "PartName")
                    .unwrap_or_default()
                    .trim_start_matches('/'))
                    == k
            {
                t.set("", "ContentType", ct);
            }
        }
    }

    fn drop_override(&mut self, keep: impl Fn(&str) -> bool) {
        self.types.root.kids.retain(|n| match n {
            Node::El(t) if t.is(CONTENT_TYPES, "Override") => keep(
                t.attr("", "PartName")
                    .unwrap_or_default()
                    .trim_start_matches('/'),
            ),
            _ => true,
        });
    }

    /// The relationships of `source` ("" for the package).
    pub fn rels(&self, source: &str) -> Result<Vec<Rel>, Error> {
        let Some(doc) = self.doc(&rels_name(source))? else {
            return Ok(vec![]);
        };
        Ok(doc
            .root
            .els()
            .filter(|e| e.is(RELATIONSHIPS, "Relationship"))
            .map(|e| {
                let get = |a| e.attr("", a).unwrap_or_default().to_string();
                Rel {
                    id: get("Id"),
                    kind: get("Type"),
                    target: get("Target"),
                    external: get("TargetMode") == "External",
                }
            })
            .collect())
    }

    /// The internal target of `source`'s relationship `id`, when the copy has it.
    pub fn target(&self, source: &str, id: &str) -> Result<Option<String>, Error> {
        let rels = self.rels(source)?;
        let rel = rels.iter().find(|r| r.id == id && !r.external);
        Ok(rel
            .and_then(|r| resolve(source, &r.target))
            .filter(|p| self.has(p))
            .map(|p| self.name(&p)))
    }

    /// The part's name as the copy spells it.
    fn name(&self, part: &str) -> String {
        self.index
            .get(&key(part))
            .and_then(|&i| self.entries[i].as_ref())
            .map_or(part.into(), |e| e.0.clone())
    }

    /// Every relationship in the copy.
    pub fn links(&self) -> Result<Vec<Link>, Error> {
        let mut out = vec![];
        for name in self.names() {
            let Some(source) = rels_source(&name) else {
                continue;
            };
            if !source.is_empty() && !self.has(&source) {
                continue;
            }
            for rel in self.rels(&source)? {
                let part = resolve(&source, &rel.target)
                    .filter(|p| !rel.external && self.has(p))
                    .map(|p| self.name(&p));
                out.push(Link {
                    source: source.clone(),
                    rel,
                    part,
                });
            }
        }
        Ok(out)
    }

    /// The internal relationships of type `short` (see [`Rel::short`]).
    pub fn linked(&self, short: &str) -> Result<Vec<Link>, Error> {
        let mut links = self.links()?;
        links.retain(|l| l.part.is_some() && l.rel.short() == short);
        Ok(links)
    }

    /// Drops `source`'s relationship `id` and every relationship-id attribute in `source` that
    /// names it.
    pub fn unlink(&mut self, source: &str, id: &str) -> Result<(), Error> {
        self.edit(&rels_name(source), |root| {
            Ok(root.prune(&|e: &El| {
                e.is(RELATIONSHIPS, "Relationship") && e.attr("", "Id") == Some(id)
            }) > 0)
        })?;
        if self.is_xml(source) {
            self.edit(source, |root| {
                let mut n = 0;
                root.each_mut(&mut |e| n += e.drop_attrs(|a| &*a.ns == ns::R && a.value == id));
                Ok(n > 0)
            })?;
        }
        Ok(())
    }

    /// Whether a part is XML the tree can read.
    pub fn is_xml(&self, name: &str) -> bool {
        name.to_ascii_lowercase().ends_with(".rels")
            || self.content_type(name).is_some_and(|t| t.ends_with("xml"))
    }

    /// Deletes a part: every relationship to it (with the attributes that name it), the part,
    /// its relationships and its Override.
    pub fn delete(&mut self, name: &str) -> Result<(), Error> {
        let k = key(name);
        for l in self.links()? {
            if l.part.as_deref().is_some_and(|p| key(p) == k) {
                self.unlink(&l.source, &l.rel.id)?;
            }
        }
        self.remove(name);
        self.remove(&rels_name(name));
        self.drop_override(|p| key(p) != k);
        Ok(())
    }

    /// The XML parts but relationships and content types.
    pub fn xml_parts(&self) -> Vec<String> {
        let mut names = self.names();
        names.retain(|n| !n.to_ascii_lowercase().ends_with(".rels") && self.is_xml(n));
        names
    }

    /// Edits `part` with `f`, which gives how many things it changed; when any, the part is
    /// written back and the relationships no element names any more go (the sweep then takes
    /// the parts nothing else reaches).
    pub fn change(&mut self, part: &str, f: impl FnOnce(&mut El) -> usize) -> Result<usize, Error> {
        let Some(mut doc) = self.doc(part)? else {
            return Ok(0);
        };
        let before = ids(&doc.root);
        let n = f(&mut doc.root);
        if n == 0 {
            return Ok(0);
        }
        self.set(part, doc.write());
        for id in before.difference(&ids(&doc.root)) {
            self.unlink(part, id)?;
        }
        Ok(n)
    }

    /// Removes the elements of `part` that `pick` picks (see [`El::prune`]) and the
    /// relationships only they named.
    pub fn prune_linked(&mut self, part: &str, pick: impl Fn(&El) -> bool) -> Result<usize, Error> {
        self.change(part, |root| root.prune(&pick))
    }
    /// Deletes every part that a relationship of type `short` reaches.
    pub fn delete_linked(&mut self, short: &str) -> Result<usize, Error> {
        let links = self.linked(short)?;
        for l in &links {
            if let Some(p) = &l.part {
                self.delete(p)?;
            }
        }
        Ok(links.len())
    }

    /// The parts reachable from the package's relationships.
    pub fn reachable(&self) -> Result<HashSet<String>, Error> {
        let mut seen = HashSet::new();
        let mut pending = VecDeque::from([String::new()]);
        while let Some(source) = pending.pop_front() {
            for rel in self.rels(&source)? {
                if let Some(p) =
                    resolve(&source, &rel.target).filter(|p| !rel.external && self.has(p))
                    && seen.insert(key(&p))
                {
                    pending.push_back(self.name(&p));
                }
            }
        }
        Ok(seen)
    }

    /// Deletes the parts our edits left unreachable (those `before` reached); with `all`, every
    /// unreachable part, every entry with no content type and Overrides for missing parts.
    pub fn sweep(&mut self, all: bool, before: &HashSet<String>) -> Result<(), Error> {
        loop {
            let reached = self.reachable()?;
            let mut gone = 0;
            for name in self.names() {
                let k = key(&name);
                let rels = k.ends_with(".rels");
                let orphan = match rels_source(&name) {
                    Some(source) if rels => !source.is_empty() && !self.has(&source),
                    _ => !reached.contains(&k) && self.content_type(&name).is_some(),
                };
                let untyped = !part_name(&name) || (!rels && self.content_type(&name).is_none());
                if orphan && (all || before.contains(&k) || rels) || (all && untyped) {
                    self.remove(&name);
                    self.drop_override(|p| key(p) != k);
                    gone += 1;
                }
            }
            if gone == 0 {
                break;
            }
        }
        // An Override for a missing part holds nothing, so it goes whatever is kept.
        let present: HashSet<String> = self.names().iter().map(|n| key(n)).collect();
        self.drop_override(|p| present.contains(&key(p)));
        Ok(())
    }

    /// Checks that every internal target exists, every relationship-id attribute resolves,
    /// every part has a content type (unless `untyped` entries are kept), no Override names a
    /// missing part and the main part's type is the variant's.
    pub fn consistent(&self, untyped: bool) -> Result<(), String> {
        let fail = |e: Error| e.message;
        for name in self.names() {
            if rels_source(&name).is_some() || !part_name(&name) {
                continue;
            }
            if self.content_type(&name).is_none() {
                if untyped {
                    continue;
                }
                return Err(format!("part {name} has no content type"));
            }
            let ids: HashSet<String> = self
                .rels(&name)
                .map_err(fail)?
                .into_iter()
                .map(|r| r.id)
                .collect();
            for l in self.rels(&name).map_err(fail)? {
                if !l.external && !resolve(&name, &l.target).is_some_and(|p| self.has(&p)) {
                    return Err(format!("{name} links to missing {}", l.target));
                }
            }
            if self.is_xml(&name)
                && let Ok(Some(doc)) = self.doc(&name)
            {
                let mut missing = None;
                doc.root.each(&mut |e| {
                    for a in &e.attrs {
                        if &*a.ns == ns::R && !a.declares() && !ids.contains(&a.value) {
                            missing.get_or_insert(format!(
                                "{name} names missing relationship {}",
                                a.value
                            ));
                        }
                    }
                });
                if let Some(m) = missing {
                    return Err(m);
                }
            }
        }
        for l in self.rels("").map_err(fail)? {
            if !l.external && !resolve("", &l.target).is_some_and(|p| self.has(&p)) {
                return Err(format!("the package links to missing {}", l.target));
            }
        }
        for t in self
            .types
            .root
            .els()
            .filter(|t| t.is(CONTENT_TYPES, "Override"))
        {
            let part = t
                .attr("", "PartName")
                .unwrap_or_default()
                .trim_start_matches('/');
            if !self.has(part) {
                return Err(format!("an Override names missing {part}"));
            }
        }
        let main = self.content_type(&self.main).unwrap_or_default();
        match MAIN_TYPES.iter().find(|(t, ..)| *t == main) {
            Some((_, _, v)) if *v == self.variant => Ok(()),
            _ => Err(format!(
                "the main part's type {main} isn't a {}",
                self.variant
            )),
        }
    }

    /// The copy as a ZIP: content types first, then the parts in order, deflated at level 6
    /// with every time 1980-01-01 00:00 and no extra fields or comments.
    pub fn finish(self) -> Result<Vec<u8>, Error> {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6))
            .last_modified_time(DateTime::default());
        let broken = |_| Error::internal();
        let mut z = ZipWriter::new(Cursor::new(vec![]));
        let types = self.types.write();
        for (name, bytes) in [(TYPES.to_string(), types)]
            .into_iter()
            .chain(self.entries.into_iter().flatten())
        {
            z.start_file(name, options).map_err(broken)?;
            z.write_all(&bytes).map_err(|_| Error::internal())?;
        }
        Ok(z.finish().map_err(broken)?.into_inner())
    }
}

/// A cleaned copy.
pub struct Cleaned {
    pub bytes: Vec<u8>,
    pub variant: &'static str,
    pub changes: Vec<Change>,
}

/// The relationship ids an element and everything in it name.
fn ids(root: &El) -> HashSet<String> {
    let mut out = HashSet::new();
    root.each(&mut |e| {
        out.extend(
            e.attrs
                .iter()
                .filter(|a| &*a.ns == ns::R && !a.declares())
                .map(|a| a.value.clone()),
        )
    });
    out
}

/// The checks whose findings clean can remove from Office files.
const CLEANABLE: [CheckId; 17] = [
    CheckId::Metadata,
    CheckId::Comments,
    CheckId::TrackedChanges,
    CheckId::HiddenText,
    CheckId::FakeRedactions,
    CheckId::HiddenSheets,
    CheckId::HiddenRowsColumns,
    CheckId::HiddenSlides,
    CheckId::SpeakerNotes,
    CheckId::HiddenObjects,
    CheckId::EmbeddedFiles,
    CheckId::CustomData,
    CheckId::Macros,
    CheckId::ExternalLinks,
    CheckId::PivotCaches,
    CheckId::LeftoverData,
    CheckId::Signatures,
];

/// Why each check in `remove` can't be removed: (check, reason, hint).
fn blocking(
    ctx: &Ctx,
    found: &[Finding],
    remove: &[CheckId],
) -> Result<Vec<(CheckId, String, String)>, Error> {
    let pkg = ctx.pkg;
    let typed = |t: &str| {
        pkg.parts
            .iter()
            .any(|p| pkg.content_type(p).is_some_and(|ct| ct.ends_with(t)))
    };
    let mut out = vec![];
    for &c in remove {
        let kinds: Vec<&str> = found
            .iter()
            .filter(|f| f.check == c)
            .map(|f| f.kind)
            .collect();
        if kinds.is_empty() {
            continue;
        }
        let why = match c {
            CheckId::EmbeddedFiles if kinds.contains(&"imported_chunk") => Some((
                "it holds an imported document (w:altChunk)",
                "Open and save the file in Word, which turns the imported content into normal text, then run clean again; or use --keep embedded-files.",
            )),
            CheckId::PivotCaches if typed(".slicer+xml") || typed(".slicerCache+xml") => Some((
                "its pivot table has slicers",
                "Remove the slicers in Excel, or run clean again with --keep pivot-caches.",
            )),
            CheckId::PivotCaches if typed(".timeline+xml") || typed(".timelineCache+xml") => {
                Some((
                    "its pivot table has timelines",
                    "Remove the timelines in Excel, or run clean again with --keep pivot-caches.",
                ))
            }
            CheckId::PivotCaches if kinds.contains(&"data_model") => Some((
                "it has a data model",
                "Remove the data model in Excel (Data › Manage Data Model), or run clean again with --keep pivot-caches.",
            )),
            CheckId::ExternalLinks
                if !remove.contains(&CheckId::PivotCaches)
                    && excel::caches_use_connections(ctx)? =>
            {
                Some((
                    "a pivot cache that is kept reads one of its data connections",
                    "Run clean again without --keep pivot-caches, or with --keep external-links.",
                ))
            }
            _ if !CLEANABLE.contains(&c) => Some(("this version can't remove it yet", "")),
            _ => None,
        };
        if let Some((reason, hint)) = why {
            let hint = match hint {
                "" => format!("Run clean again with --keep {}.", c.id()),
                h => h.to_string(),
            };
            out.push((c, reason.to_string(), hint));
        }
    }
    Ok(out)
}

/// Removes the checks in `remove` from the package `pkg`, whose check found `found`.
pub fn clean(pkg: &Package, found: &[Finding], remove: &[CheckId]) -> Result<Cleaned, Error> {
    let ctx = Ctx::new(pkg, false)?;
    let blocks = blocking(&ctx, found, remove)?;
    if !blocks.is_empty() {
        return Err(Error::cannot_clean(&blocks));
    }
    let before: HashSet<String> = pkg.parts.iter().map(|p| key(p)).collect();
    let mut d = Draft::new(pkg)?;
    let has = |c: CheckId| remove.contains(&c) && found.iter().any(|f| f.check == c);
    if remove.contains(&CheckId::Macros) {
        macros(&mut d)?;
    }
    if has(CheckId::Signatures) {
        d.delete_linked("origin")?;
        d.changes.push(Change::new(
            "signatures_removed",
            "The digital signature was removed; no signature survives a change.",
        ));
    }
    if has(CheckId::Comments) {
        comments(&ctx, &mut d)?;
    }
    if has(CheckId::TrackedChanges) {
        tracked_changes(&ctx, &mut d)?;
    }
    let (hidden, dark) = (has(CheckId::HiddenText), has(CheckId::FakeRedactions));
    if (hidden || dark) && d.format == Format::Docx {
        let look = word::Look::read(&ctx)?;
        for part in word::pages(&ctx) {
            d.edit(part, |root| {
                look.clean_runs(root, hidden, dark);
                Ok(true)
            })?;
        }
    }
    if has(CheckId::HiddenObjects) {
        hidden_objects(&ctx, &mut d)?;
    }
    if has(CheckId::HiddenSlides) {
        let hidden: Vec<String> = found
            .iter()
            .filter(|f| f.check == CheckId::HiddenSlides)
            .filter_map(|f| f.place.slide.map(|i| ctx.slides[i - 1].clone()))
            .collect();
        powerpoint::delete_slides(&mut d, &hidden)?;
    }
    if has(CheckId::SpeakerNotes) {
        d.delete_linked("notesSlide")?;
    }
    if has(CheckId::EmbeddedFiles) {
        embedded_files(&ctx, &mut d)?;
    }
    if has(CheckId::CustomData) {
        custom_data(&mut d)?;
    }
    if has(CheckId::ExternalLinks) {
        external_links(&ctx, &mut d)?;
    }
    if d.format == Format::Xlsx {
        workbook(&ctx, &mut d, &has, remove.contains(&CheckId::Comments))?;
    }
    if has(CheckId::Metadata) {
        metadata(&mut d)?;
    }
    let leftovers = remove.contains(&CheckId::LeftoverData);
    if leftovers && has(CheckId::LeftoverData) {
        leftover_data(&ctx, &mut d, found)?;
    }
    d.sweep(leftovers, &before)?;
    if let Err(why) = d.consistent(!leftovers) {
        panic!("the cleaned copy isn't consistent: {why}");
    }
    let (variant, changes) = (d.variant, std::mem::take(&mut d.changes));
    Ok(Cleaned {
        bytes: d.finish()?,
        variant,
        changes,
    })
}

/// The non-macro variant of each macro-enabled one.
const MACRO_FREE: [(&str, &str); 7] = [
    ("docm", "docx"),
    ("dotm", "dotx"),
    ("xlsm", "xlsx"),
    ("xltm", "xltx"),
    ("pptm", "pptx"),
    ("potm", "potx"),
    ("ppsm", "ppsx"),
];

const VML: &str = "urn:schemas-microsoft-com:vml";
const VML_EXCEL: &str = "urn:schemas-microsoft-com:office:excel";
const P188: &str = "http://schemas.microsoft.com/office/powerpoint/2018/8/main";

/// `macros`: VBA projects, custom ribbons and ActiveX controls; a macro-enabled main part
/// becomes the plain type of its kind, and the copy takes that extension.
fn macros(d: &mut Draft) -> Result<(), Error> {
    d.delete_linked("vbaProject")?;
    d.delete_linked("extensibility")?;
    let controls =
        |e: &El| e.is(ns::W, "control") || e.is(ns::S, "controls") || e.is(ns::P, "controls");
    for part in d.xml_parts() {
        d.prune_linked(&part, controls)?;
    }
    d.delete_linked("control")?;
    if d.format == Format::Docx {
        for part in d.xml_parts() {
            d.edit(&part, |root| Ok(word::unlink_fields(root, false, true) > 0))?;
        }
    }
    if let Some(&(_, plain)) = MACRO_FREE.iter().find(|m| m.0 == d.variant) {
        let (ct, ..) = MAIN_TYPES
            .iter()
            .find(|t| t.2 == plain)
            .expect("every plain variant has a type");
        let main = d.main.clone();
        d.set_type(&main, ct);
        d.variant = plain;
        d.changes.push(Change::new(
            "extension_changed",
            format!("Macros were removed, so the copy is .{plain}."),
        ));
    }
    Ok(())
}

/// `hidden-objects`: hidden shapes (and shapes off the slide) deleted; a slide's animations
/// go when they name one of them.
fn hidden_objects(ctx: &Ctx, d: &mut Draft) -> Result<(), Error> {
    let parts: Vec<String> = drawing::drawing_parts(ctx)
        .into_iter()
        .map(String::from)
        .collect();
    let mut animated = vec![];
    for part in parts {
        let mut gone = vec![];
        d.change(&part, |root| {
            gone = drawing::remove_hidden(root, ctx.pkg.format, ctx.slide_size);
            gone.len()
        })?;
        let named = |t: &El| {
            t.is(ns::P, "timing")
                && t.has(&|s: &El| {
                    s.is(ns::P, "spTgt")
                        && s.attr("", "spid")
                            .is_some_and(|id| gone.iter().any(|g| g == id))
                })
        };
        if !gone.is_empty() && d.prune_linked(&part, named)? > 0 {
            animated.extend(
                ctx.slides
                    .iter()
                    .position(|s| key(s) == key(&part))
                    .map(|i| i + 1),
            );
        }
    }
    if !animated.is_empty() {
        let list: Vec<String> = animated.iter().map(|n| n.to_string()).collect();
        let slides = match list.len() {
            1 => format!("slide {}", list[0]),
            n => format!("slides {} and {}", list[..n - 1].join(", "), list[n - 1]),
        };
        d.changes.push(Change {
            slides: Some(animated),
            ..Change::new(
                "animations_removed",
                format!("Animations were removed from {slides}."),
            )
        });
    }
    Ok(())
}

const OFFICE: &str = "urn:schemas-microsoft-com:office:office";
const CHART: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";

/// `embedded-files`: Word objects keep their picture, PowerPoint objects become their preview
/// picture, Excel objects go with their shapes, and charts lose their workbooks (they still
/// draw from their caches).
fn embedded_files(ctx: &Ctx, d: &mut Draft) -> Result<(), Error> {
    let (mut pictures, mut gone) = (0u64, 0u64);
    let object = |e: &El| {
        e.is(OFFICE, "OLEObject") || e.is(ns::W, "objectEmbed") || e.is(ns::W, "objectLink")
    };
    match d.format {
        Format::Docx => {
            for part in word::stories(ctx) {
                let Some(doc) = d.doc(part)? else { continue };
                doc.root.each(&mut |e| {
                    if e.is(ns::W, "object") && e.has(&object) {
                        match e.has(&|s: &El| s.is(VML, "shape")) {
                            true => pictures += 1,
                            false => gone += 1,
                        }
                    }
                });
                d.prune_linked(part, |e| {
                    e.is(ns::W, "object") && e.has(&object) && !e.has(&|s: &El| s.is(VML, "shape"))
                })?;
                d.prune_linked(part, object)?;
            }
        }
        Format::Xlsx => {
            for sheet in excel::cell_sheets(d)? {
                let mut shapes = HashSet::new();
                if let Some(doc) = d.doc(&sheet)? {
                    doc.root.each(&mut |e| {
                        if e.is(ns::S, "oleObject") {
                            gone += 1;
                            shapes.extend(e.attr("", "shapeId").map(|id| format!("_x0000_s{id}")));
                        }
                    });
                }
                d.prune_linked(&sheet, |e| e.is(ns::S, "oleObjects"))?;
                drop_vml_shapes(d, &sheet, |e| {
                    e.attr("", "id").is_some_and(|id| shapes.contains(id))
                })?;
            }
        }
        _ => {
            for slide in ctx.slides.clone() {
                let Some(mut doc) = d.doc(&slide)? else {
                    continue;
                };
                let before = ids(&doc.root);
                let (p, g) = powerpoint::ole_to_pictures(&mut doc.root);
                if p + g == 0 {
                    continue;
                }
                (pictures, gone) = (pictures + p, gone + g);
                d.set(&slide, doc.write());
                for id in before.difference(&ids(&doc.root)) {
                    d.unlink(&slide, id)?;
                }
            }
        }
    }
    for part in d.names() {
        if d.content_type(&part).as_deref() == Some(CHART_TYPE) {
            d.prune_linked(&part, |e| e.is(CHART, "externalData"))?;
        }
    }
    let objects = |n: u64| {
        if n == 1 {
            "1 embedded object is"
        } else {
            "embedded objects are"
        }
    };
    if pictures > 0 {
        let what = match pictures {
            1 => "1 embedded object is now a picture.".to_string(),
            n => format!("{n} {} now pictures.", objects(n)),
        };
        d.changes
            .push(Change::counted("objects_to_pictures", pictures, what));
    }
    if gone > 0 {
        let what = match gone {
            1 => "1 embedded object had no picture and was removed.".to_string(),
            n => format!("{n} embedded objects had no picture and were removed."),
        };
        d.changes
            .push(Change::counted("objects_removed", gone, what));
    }
    Ok(())
}

const CHART_TYPE: &str = "application/vnd.openxmlformats-officedocument.drawingml.chart+xml";

/// Removes the VML shapes of `sheet`'s drawing that `pick` picks; a drawing left with no shape
/// goes with the sheet's `legacyDrawing`.
fn drop_vml_shapes(d: &mut Draft, sheet: &str, pick: impl Fn(&El) -> bool) -> Result<(), Error> {
    for l in d.linked("vmlDrawing")? {
        if key(&l.source) != key(sheet) {
            continue;
        }
        let vml = l.part.unwrap_or_default();
        let left = match d.doc(&vml) {
            Ok(Some(mut doc)) => {
                if doc.root.prune(&|e: &El| e.is(VML, "shape") && pick(e)) == 0 {
                    continue;
                }
                let left = doc.root.has(&|e: &El| e.is(VML, "shape"));
                d.set(&vml, doc.write());
                left
            }
            _ => false,
        };
        if !left {
            let id = l.rel.id.clone();
            d.prune_linked(&l.source, |e| {
                e.is(ns::S, "legacyDrawing") && e.attr(ns::R, "id") == Some(&id)
            })?;
        }
    }
    Ok(())
}

/// The workbook's own steps: hidden and macro sheets deleted, cells cleared, pivot caches,
/// linked workbooks and connections removed, then the values rule and the shared strings.
fn workbook(
    ctx: &Ctx,
    d: &mut Draft,
    has: &dyn Fn(CheckId) -> bool,
    comments_gone: bool,
) -> Result<(), Error> {
    let mut deleted: Vec<String> = vec![];
    for s in &ctx.sheets {
        let macro_sheet = s.part.as_deref().is_some_and(|p| {
            ctx.pkg
                .content_type(p)
                .is_some_and(|t| t.contains("macrosheet"))
        });
        if (has(CheckId::HiddenSheets) && s.state != "visible")
            || (has(CheckId::Macros) && macro_sheet)
        {
            deleted.push(s.name.clone());
        }
    }
    if !deleted.is_empty() {
        excel::delete_sheets(d, &deleted)?;
    }
    let (hidden, dark, lines) = (
        has(CheckId::HiddenText),
        has(CheckId::FakeRedactions),
        has(CheckId::HiddenRowsColumns),
    );
    let mut cleared = 0;
    if hidden || dark || lines {
        let clear = excel::Clear::new(ctx, lines, hidden, dark)?;
        for sheet in excel::cell_sheets(d)? {
            let mut refs = vec![];
            d.edit(&sheet, |root| {
                refs = excel::clear_cells(root, &clear);
                Ok(!refs.is_empty())
            })?;
            cleared += refs.len();
            if !comments_gone && !refs.is_empty() {
                drop_cell_comments(d, &sheet, &refs)?;
            }
        }
    }
    if has(CheckId::PivotCaches) {
        d.delete_linked("pivotTable")?;
        let book = d.main.clone();
        d.prune_linked(&book, |e| e.is(ns::S, "pivotCaches"))?;
    }
    let mut external = false;
    if has(CheckId::ExternalLinks) {
        let book = d.main.clone();
        external = d.prune_linked(&book, |e| e.is(ns::S, "externalReferences"))? > 0;
        let ac = "http://schemas.microsoft.com/office/spreadsheetml/2010/11/ac";
        d.prune_linked(&book, |e| e.is(ac, "absPath"))?;
        if external {
            d.edit(&book, |root| {
                let linked = |e: &El| {
                    e.is(ns::S, "definedName") && e.text().contains('[') && e.text().contains(']')
                };
                Ok(root.prune(&linked) > 0)
            })?;
        }
        d.delete_linked("connections")?;
        d.delete_linked("queryTable")?;
        for part in d.xml_parts() {
            if d.content_type(&part)
                .is_some_and(|t| t.ends_with(".table+xml"))
            {
                d.edit(&part, |root| {
                    let mut n = 0;
                    root.each_mut(&mut |e| {
                        n += e.drop_attrs(|a| {
                            a.name == "queryTableFieldId"
                                || (a.name == "tableType" && a.value == "queryTable")
                        });
                    });
                    Ok(n > 0)
                })?;
            }
        }
    }
    if !deleted.is_empty() || cleared > 0 || external {
        values_rule(d, &deleted)?;
    }
    excel::rebuild_strings(d)
}

/// The values rule: every formula becomes its saved result and every chart literal.
fn values_rule(d: &mut Draft, deleted: &[String]) -> Result<(), Error> {
    let (mut replaced, mut empty, mut charts) = (0, 0, 0);
    for sheet in excel::cell_sheets(d)? {
        d.edit(&sheet, |root| {
            let (r, e) = excel::to_values(root);
            (replaced, empty) = (replaced + r, empty + e);
            Ok(r + e > 0)
        })?;
    }
    for part in d.names() {
        if d.content_type(&part).as_deref() != Some(CHART_TYPE) {
            continue;
        }
        let mut stuck = None;
        let changed = d.edit(&part, |root| match excel::chart_to_values(root, deleted) {
            Ok(changed) => Ok(changed),
            Err(f) => {
                stuck = Some(f);
                Ok(false)
            }
        })?;
        if stuck.is_some() {
            return Err(Error::cannot_clean(&[(
                CheckId::HiddenSheets,
                format!("chart {} has multi-level labels from a deleted sheet", safe(&part)),
                "Change the chart's labels to one level in Excel, or run clean again with --keep hidden-sheets.".into(),
            )]));
        }
        charts += u64::from(changed);
    }
    d.delete_linked("calcChain")?;
    if replaced > 0 {
        let what = match replaced {
            1 => "1 formula was replaced by its saved result.".to_string(),
            n => format!("{n} formulas were replaced by their saved results."),
        };
        d.changes
            .push(Change::counted("formulas_to_values", replaced, what));
    }
    if empty > 0 {
        let what = match empty {
            1 => "1 formula had no saved result and is now empty.".to_string(),
            n => format!("{n} formulas had no saved result and are now empty."),
        };
        d.changes
            .push(Change::counted("formulas_without_value", empty, what));
    }
    if charts > 0 {
        let what = match charts {
            1 => "1 chart now shows its saved values instead of linked cells.".to_string(),
            n => format!("{n} charts now show their saved values instead of linked cells."),
        };
        d.changes
            .push(Change::counted("charts_to_values", charts, what));
    }
    Ok(())
}

/// Kept notes and threaded comments on cleared cells go with the cells.
fn drop_cell_comments(d: &mut Draft, sheet: &str, refs: &[String]) -> Result<(), Error> {
    let on_cleared = |e: &El| {
        e.attr("", "ref")
            .is_some_and(|r| refs.iter().any(|c| c == r))
    };
    for l in d.links()? {
        if key(&l.source) != key(sheet) || !matches!(l.rel.short(), "comments" | "threadedComment")
        {
            continue;
        }
        if let Some(part) = l.part {
            d.edit(&part, |root| {
                Ok(root.prune(&|e: &El| {
                    (e.local() == "comment" || e.local() == "threadedComment") && on_cleared(e)
                }) > 0)
            })?;
        }
    }
    let at = |e: &El, local: &str| {
        e.child(VML_EXCEL, "ClientData")
            .and_then(|c| c.child(VML_EXCEL, local))
            .and_then(|v| v.text().trim().parse::<u32>().ok())
    };
    drop_vml_shapes(d, sheet, |e| match (at(e, "Row"), at(e, "Column")) {
        (Some(r), Some(c)) => refs.contains(&format!("{}{}", excel::column(c + 1), r + 1)),
        _ => false,
    })
}

/// `external-links` in Word and PowerPoint files and in workbook drawings: the template and
/// mail merge source, fields that read files, links and linked pictures to local files, and
/// alt text that holds a path.
fn external_links(ctx: &Ctx, d: &mut Draft) -> Result<(), Error> {
    if d.format == Format::Docx {
        for l in d.linked("settings")? {
            let settings = l.part.unwrap_or_default();
            d.prune_linked(&settings, |e| {
                e.is(ns::W, "attachedTemplate") || e.is(ns::W, "mailMerge")
            })?;
        }
        for part in word::stories(ctx) {
            d.edit(part, |root| Ok(word::unlink_fields(root, true, false) > 0))?;
        }
    }
    let mut files: Vec<(String, Vec<String>)> = vec![];
    for l in d.links()? {
        let own = common::OWN_LINKS.contains(&l.rel.short());
        if l.rel.external && !own && common::is_path(&l.rel.target, true) {
            match files.iter_mut().find(|f| f.0 == l.source) {
                Some(f) => f.1.push(l.rel.id),
                None => files.push((l.source, vec![l.rel.id])),
            }
        }
    }
    for (source, links) in files {
        let named = |e: &El, a: &str| {
            e.attr(ns::R, a)
                .is_some_and(|v| links.iter().any(|l| l == v))
        };
        if d.is_xml(&source) {
            d.edit(&source, |root| {
                root.unwrap(&|e| e.is(ns::W, "hyperlink") && named(e, "id"));
                let linked_only = |e: &El| {
                    e.is(ns::A, "blip") && named(e, "link") && e.attr(ns::R, "embed").is_none()
                };
                root.prune(&|e| {
                    (e.is(ns::A, "hlinkClick")
                        || e.is(ns::A, "hlinkHover")
                        || e.is(ns::S, "hyperlink"))
                        && named(e, "id")
                        || e.is(OFFICE, "OLEObject") && named(e, "id")
                        || (e.is(ns::W, "drawing")
                            || e.is(ns::P, "pic")
                            || e.local().ends_with("Anchor"))
                            && e.has(&linked_only)
                });
                root.each_mut(&mut |e| {
                    if e.is(ns::A, "blip") && named(e, "link") {
                        e.drop_attrs(|a| &*a.ns == ns::R && a.local() == "link");
                    }
                });
                Ok(true)
            })?;
        }
        for id in &links {
            d.unlink(&source, id)?;
        }
    }
    for part in d.xml_parts() {
        d.edit(&part, |root| {
            let mut n = 0;
            root.each_mut(&mut |e| {
                for a in &mut e.attrs {
                    let alt = a.ns.is_empty() && (a.name == "descr" || a.name == "title");
                    if alt && common::is_path(&a.value, false) {
                        a.value.clear();
                        n += 1;
                    }
                }
            });
            Ok(n > 0)
        })?;
    }
    Ok(())
}

/// `comments`: comment and author parts, and what points at them in the text.
fn comments(ctx: &Ctx, d: &mut Draft) -> Result<(), Error> {
    match d.format {
        Format::Docx => {
            let marks = |e: &El| {
                e.is(ns::W, "commentRangeStart")
                    || e.is(ns::W, "commentRangeEnd")
                    || (e.is(ns::W, "r") && e.child(ns::W, "commentReference").is_some())
            };
            for part in word::stories(ctx) {
                d.prune_linked(part, marks)?;
            }
            for short in [
                "comments",
                "commentsExtended",
                "commentsIds",
                "commentsExtensible",
                "people",
                "documenttasks",
            ] {
                d.delete_linked(short)?;
            }
        }
        Format::Xlsx => {
            for short in ["comments", "threadedComment", "person"] {
                d.delete_linked(short)?;
            }
            let note = |e: &El| {
                e.child(VML_EXCEL, "ClientData")
                    .is_some_and(|c| c.attr("", "ObjectType") == Some("Note"))
            };
            for sheet in excel::cell_sheets(d)? {
                drop_vml_shapes(d, &sheet, note)?;
            }
        }
        _ => {
            for short in ["comments", "commentAuthors", "authors"] {
                d.delete_linked(short)?;
            }
            for slide in &ctx.slides {
                d.prune_linked(slide, |e| {
                    e.local() == "ext" && e.child(P188, "commentRel").is_some()
                })?;
                d.edit(slide, |root| Ok(empty_extensions(root) > 0))?;
            }
        }
    }
    Ok(())
}

/// `tracked-changes`: every change in a Word file accepted; a workbook's shared-workbook logs
/// deleted.
fn tracked_changes(ctx: &Ctx, d: &mut Draft) -> Result<(), Error> {
    if d.format == Format::Xlsx {
        d.delete_linked("revisionHeaders")?;
        d.delete_linked("usernames")?;
        return Ok(());
    }
    for short in ["styles", "numbering"] {
        for l in d.linked(short)? {
            let part = l.part.unwrap_or_default();
            d.edit(&part, |root| Ok(revisions::accept_properties(root) > 0))?;
        }
    }
    for part in word::stories(ctx) {
        let mut left = None;
        d.edit(part, |root| {
            revisions::accept(root);
            left = revisions::leftover(root);
            Ok(true)
        })?;
        if let Some(name) = left {
            return Err(Error::cannot_clean(&[(
                CheckId::TrackedChanges,
                format!("a kind of change this version can't accept ({name} in {})", safe(part)),
                "Accept all changes in Word (Review › Accept › Accept All Changes), save, and run clean again, or use --keep tracked-changes.".into(),
            )]));
        }
    }
    Ok(())
}

/// Removes the `ext` elements of extension lists left with nothing in them, then empty
/// `extLst`s.
fn empty_extensions(root: &mut El) -> usize {
    let empty = |e: &El| e.els().next().is_none();
    let mut n = 0;
    root.each_mut(&mut |e| {
        if e.local() == "extLst" {
            n += e.prune(&|x: &El| x.local() == "ext" && empty(x));
        }
    });
    n + root.prune(&|e: &El| e.local() == "extLst" && empty(e))
}

/// `custom-data`: custom XML parts, data bindings, document variables, tags, add-in data,
/// scenarios and custom sheet properties.
fn custom_data(d: &mut Draft) -> Result<(), Error> {
    d.delete_linked("customXml")?;
    d.delete_linked("webextensiontaskpanes")?;
    d.delete_linked("webextension")?;
    let pick = |e: &El| {
        e.is(ns::W, "dataBinding")
            || e.is(ns::W, "docVars")
            || e.is(ns::P, "tags")
            || e.is(ns::S, "scenarios")
            || e.is(ns::S, "customProperties")
    };
    for part in d.xml_parts() {
        d.prune_linked(&part, pick)?;
    }
    let empty_tags = |e: &El| e.is(ns::P, "custDataLst") && e.els().next().is_none();
    for part in d.xml_parts() {
        d.prune_linked(&part, empty_tags)?;
    }
    d.delete_linked("tags")?;
    Ok(())
}

/// `metadata`: the property, label, thumbnail and printer settings parts, tracking ids and
/// editor names.
fn metadata(d: &mut Draft) -> Result<(), Error> {
    for short in [
        "core-properties",
        "extended-properties",
        "custom-properties",
        "classificationlabels",
        "thumbnail",
        "printerSettings",
        "revisionInfo",
    ] {
        d.delete_linked(short)?;
    }
    let format = d.format;
    for part in d.xml_parts() {
        d.edit(&part, |root| {
            let mut n = 0;
            match format {
                Format::Docx => {
                    root.each_mut(&mut |e| {
                        n += e.drop_attrs(|a| &*a.ns == ns::W && a.local().starts_with("rsid"))
                    });
                    n += root.prune(&|e: &El| {
                        e.is(ns::W, "rsids")
                            || e.is(ns::W14, "docId")
                            || e.is(ns::W15, "docId")
                            || e.is(ns::W, "permStart")
                            || e.is(ns::W, "permEnd")
                    });
                }
                Format::Xlsx => {
                    root.each_mut(&mut |e| {
                        n += e.drop_attrs(|a| &*a.ns == ns::XR && a.local() == "uid");
                        if e.is(ns::S, "fileSharing") {
                            n += e.drop_attrs(|a| a.name == "userName");
                        }
                    });
                    n += root.prune(&|e: &El| e.is(ns::XR, "revisionPtr"));
                }
                _ => {
                    n += root.prune(&|e: &El| e.is(ns::P14, "creationId"));
                    if n > 0 {
                        empty_extensions(root);
                    }
                }
            }
            Ok(n > 0)
        })?;
    }
    Ok(())
}

/// `leftover-data`: the glossary and unused shared strings; the sweep takes the rest.
fn leftover_data(ctx: &Ctx, d: &mut Draft, found: &[Finding]) -> Result<(), Error> {
    let kinds = |k: &str| {
        found
            .iter()
            .any(|f| f.check == CheckId::LeftoverData && f.kind == k)
    };
    if kinds("glossary") {
        for g in word::glossaries(ctx) {
            d.delete(g)?;
        }
    }
    if kinds("unused_strings") {
        excel::rebuild_strings(d)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_target_or_type_is_inconsistent() {
        let rels = "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"x/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
        let types = format!(
            "<Types xmlns=\"{CONTENT_TYPES}\"><Override PartName=\"/word/document.xml\" ContentType=\"{}\"/></Types>",
            MAIN_TYPES[0].0
        );
        let mut d = Draft {
            format: Format::Docx,
            variant: "docx",
            main: "word/document.xml".into(),
            entries: vec![],
            index: HashMap::new(),
            types: Doc::parse(types.as_bytes(), TYPES).unwrap(),
            changes: vec![],
        };
        d.set("_rels/.rels", rels.into());
        assert_eq!(
            d.consistent(false).unwrap_err(),
            "the package links to missing word/document.xml"
        );
        d.set(
            "word/document.xml",
            format!(
                "<w:document xmlns:w=\"{}\" xmlns:r=\"{}\"><w:p r:id=\"rId9\"/></w:document>",
                ns::W,
                ns::R
            )
            .into(),
        );
        assert_eq!(
            d.consistent(false).unwrap_err(),
            "word/document.xml names missing relationship rId9"
        );
        d.edit("word/document.xml", |root| {
            Ok(root.prune(&|e: &El| e.local() == "p") > 0)
        })
        .unwrap();
        assert_eq!(d.consistent(false), Ok(()));
        d.set("word/x.bin", vec![]);
        assert_eq!(
            d.consistent(false).unwrap_err(),
            "part word/x.bin has no content type"
        );
        d.remove("word/x.bin");
        d.variant = "docm";
        assert!(d.consistent(false).unwrap_err().ends_with("isn't a docm"));
    }
}

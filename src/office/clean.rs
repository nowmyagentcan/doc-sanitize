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
    CONTENT_TYPES, Ctx, MAIN_TYPES, Package, RELATIONSHIPS, Rel, excel, key, ns, part_name,
    rels_name, resolve, revisions, word,
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

    /// Removes the elements of `part` that `pick` picks (see [`El::prune`]) and the
    /// relationships only they named; the sweep then takes the parts nothing else reaches.
    pub fn prune_linked(&mut self, part: &str, pick: impl Fn(&El) -> bool) -> Result<usize, Error> {
        let Some(mut doc) = self.doc(part)? else {
            return Ok(0);
        };
        let before = ids(&doc.root);
        let n = doc.root.prune(&pick);
        if n == 0 {
            return Ok(0);
        }
        self.set(part, doc.write());
        let after = ids(&doc.root);
        for id in before.difference(&after) {
            self.unlink(part, id)?;
        }
        Ok(n)
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

/// What clean can't remove yet: (check, kind, what it is).
const NOT_YET: [(CheckId, &str); 2] = [
    (CheckId::Macros, "macro_sheet"),
    (CheckId::Macros, "dde_field"),
];

/// The checks whose findings clean can remove from Office files.
const CLEANABLE: [CheckId; 7] = [
    CheckId::Metadata,
    CheckId::Comments,
    CheckId::TrackedChanges,
    CheckId::CustomData,
    CheckId::Macros,
    CheckId::LeftoverData,
    CheckId::Signatures,
];

/// Why each check in `remove` can't be removed: (check, reason, hint).
fn blocking(found: &[Finding], remove: &[CheckId]) -> Vec<(CheckId, String, String)> {
    let mut out = vec![];
    for &c in remove {
        let not_yet = found
            .iter()
            .any(|f| f.check == c && NOT_YET.iter().any(|n| n.0 == c && n.1 == f.kind));
        if found.iter().any(|f| f.check == c) && (!CLEANABLE.contains(&c) || not_yet) {
            out.push((
                c,
                "this version can't remove it yet".into(),
                format!("Run clean again with --keep {}.", c.id()),
            ));
        }
    }
    out
}

/// Removes the checks in `remove` from the package `pkg`, whose check found `found`.
pub fn clean(pkg: &Package, found: &[Finding], remove: &[CheckId]) -> Result<Cleaned, Error> {
    let blocks = blocking(found, remove);
    if !blocks.is_empty() {
        return Err(Error::cannot_clean(&blocks));
    }
    let ctx = Ctx::new(pkg, false)?;
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
    if has(CheckId::CustomData) {
        custom_data(&mut d)?;
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
            for l in d.linked("vmlDrawing")? {
                let vml = l.part.unwrap_or_default();
                let note = |e: &El| {
                    e.is(VML, "shape")
                        && e.child(VML_EXCEL, "ClientData")
                            .is_some_and(|c| c.attr("", "ObjectType") == Some("Note"))
                };
                let left = match d.doc(&vml) {
                    Ok(Some(mut doc)) => {
                        doc.root.prune(&note);
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

/// Removes `ext` elements left with nothing in them, then empty `extLst`s.
fn empty_extensions(root: &mut El) -> usize {
    let empty = |local: &'static str| move |e: &El| e.local() == local && e.els().next().is_none();
    root.prune(&empty("ext")) + root.prune(&empty("extLst"))
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

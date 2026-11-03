//! A builder for Office packages with any parts and relationships.

use super::*;

/// A package under construction: the main part of `variant`, then parts added with their
/// content types and the relationships that reach them.
pub struct Pkg {
    main: &'static str,
    types: Vec<(String, String)>,
    rels: Vec<(String, Vec<String>)>,
    parts: Vec<Entry>,
}

impl Pkg {
    pub fn new(variant: &str) -> Self {
        let (main, ct, body) = main_part(variant);
        let mut p = Self {
            main,
            types: vec![(main.into(), ct)],
            rels: vec![],
            parts: vec![entry(main, body.as_bytes())],
        };
        p.rel("", rel("rId1", "officeDocument", main));
        p
    }

    /// The main part's name.
    pub fn main(&self) -> &'static str {
        self.main
    }

    /// Replaces the main part's body.
    pub fn body(mut self, xml: &str) -> Self {
        self.parts[0].data = xml.as_bytes().into();
        self
    }

    /// Adds a relationship (made by `rel` or `external`) to `source`'s `.rels`.
    pub fn rel(&mut self, source: &str, rel: String) {
        match self.rels.iter_mut().find(|(s, _)| s == source) {
            Some((_, list)) => list.push(rel),
            None => self.rels.push((source.into(), vec![rel])),
        }
    }

    /// Adds `rel` to `source`'s `.rels`.
    pub fn link(mut self, source: &str, rel: String) -> Self {
        self.rel(source, rel);
        self
    }

    /// Adds the part `name` of content type `ct` (none when empty) and the relationship
    /// `id` of type `kind` that reaches it from `source`.
    pub fn part(
        mut self,
        source: &str,
        id: &str,
        kind: &str,
        name: &str,
        ct: &str,
        data: &[u8],
    ) -> Self {
        let target = relative(source, name);
        self.rel(source, rel(id, kind, &target));
        self.add(name, ct, data)
    }

    /// Adds the part `name` with no relationship to it.
    pub fn add(mut self, name: &str, ct: &str, data: &[u8]) -> Self {
        if !ct.is_empty() {
            self.types.push((name.into(), ct.into()));
        }
        self.parts.push(entry(name, data));
        self
    }

    pub fn entries(self) -> Vec<Entry> {
        let overrides: String = self
            .types
            .iter()
            .map(|(p, t)| format!("<Override PartName=\"/{p}\" ContentType=\"{t}\"/>"))
            .collect();
        let types = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
             <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
             <Default Extension=\"png\" ContentType=\"image/png\"/>\
             <Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\
             <Default Extension=\"bin\" ContentType=\"application/vnd.openxmlformats-officedocument.oleObject\"/>{overrides}</Types>"
        );
        let mut out = vec![entry("[Content_Types].xml", types.as_bytes())];
        for (source, list) in &self.rels {
            let name = rels_part(source);
            out.push(entry(&name, rels_of(list).as_bytes()));
        }
        out.extend(self.parts);
        out
    }

    pub fn zip(self) -> Vec<u8> {
        zip(&self.entries())
    }
}

/// The `.rels` part of `source` ("" for the package).
fn rels_part(source: &str) -> String {
    match source.rsplit_once('/') {
        Some((dir, file)) => format!("{dir}/_rels/{file}.rels"),
        None if source.is_empty() => "_rels/.rels".into(),
        None => format!("_rels/{source}.rels"),
    }
}

/// `name` as a target relative to `source`'s folder.
fn relative(source: &str, name: &str) -> String {
    let dir = source.rsplit_once('/').map_or("", |(d, _)| d);
    if dir.is_empty() {
        return name.into();
    }
    match name.strip_prefix(&format!("{dir}/")) {
        Some(rest) => rest.into(),
        None => format!("/{name}"),
    }
}

pub const PKG_RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
pub const OX: &str = "application/vnd.openxmlformats-officedocument";

//! Vars per role: layers merged at the TOML level, then deserialized
//! into the playbook's `V`.
//!
//! `V` is a struct of role sections, each field a role's own vars struct.
//! A value for field `f` of section `s` is the first of:
//!
//! 1. the vars file's `[s]` table, key `f`;
//! 2. the vars file's top-level key `f` (every section with a field of
//!    that name gets it);
//! 3. the playbook's defaults ([`crate::Playbook::defaults`]: a typed value,
//!    serialized to TOML of the same shape, for that playbook only);
//! 4. the section struct's own `Default` (`#[serde(default)]`).
//!
//! With an inventory (see [`crate::inventory`]) the first two places repeat
//! for each layer above the file, in priority order: the host's `vars`, its
//! groups' (the last listed first), the inventory's `[vars]`, then the vars
//! file; see [`load_layers`].
//!
//! Field names are learned by deserializing `V` from a probing
//! [`serde::Deserializer`] that records the `fields` list each struct hands
//! to `deserialize_struct`; no derive beyond serde's own. A section that is
//! not a struct (a plain scalar, a map) is a pass-through: the top-level key
//! of the section's own name feeds it directly. This is what lets a flat
//! `V` of scalar fields keep working.
//!
//! Errors name the file and the 1-based line of the offending key: an
//! unknown top-level key, an unknown key in a section, a wrong type. They
//! arise at load, before any host is traced.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::role::Playbook;

use serde::de::{
    self, DeserializeOwned, DeserializeSeed, Deserializer, EnumAccess, IntoDeserializer, MapAccess,
    SeqAccess, VariantAccess, Visitor,
};

/// Load `V` from the vars file at `path` (none: no file), with the
/// playbook's `defaults` (TOML text, `""` for none).
///
/// With no file and no defaults the result is `V::default()`; with either,
/// the layers run over an empty file, so a `V` whose sections all have
/// `#[serde(default)]` gets the same value both ways.
pub fn load_file<V: DeserializeOwned + Default>(
    path: Option<&Path>,
    defaults: &str,
) -> Result<V, String> {
    match path {
        None if defaults.trim().is_empty() => Ok(V::default()),
        None => load("", "", defaults),
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read vars file {}: {e}", p.display()))?;
            load(&p.display().to_string(), &text, defaults)
        }
    }
}

/// [`load_file`] with the defaults of `playbook` as layer 3. A default that
/// will not serialize is the error `playbook <name> defaults: ...`.
pub fn load_file_for<V: DeserializeOwned + Default>(
    playbook: &Playbook<V>,
    path: Option<&Path>,
) -> Result<V, String> {
    load_file(path, &playbook.defaults_toml()?)
}

/// [`load`] with the defaults of `playbook` as layer 3.
pub fn load_for<V: DeserializeOwned>(
    playbook: &Playbook<V>,
    source: &str,
    text: &str,
) -> Result<V, String> {
    load(source, text, &playbook.defaults_toml()?)
}

/// Load `V` from vars `text` (called `source` in errors) and `defaults`.
pub fn load<V: DeserializeOwned>(source: &str, text: &str, defaults: &str) -> Result<V, String> {
    let file = VarLayer::parse(source, text)?;
    load_layers(&[&file], defaults)
}

/// One place vars come from: a vars file, or the vars of an inventory's
/// host, group or top level (see [`crate::inventory`]). Keys keep their file
/// and line, so an error names where the value was written.
pub struct VarLayer(Doc);

impl VarLayer {
    /// Parse a whole vars file (`source` names it in errors).
    pub fn parse(source: &str, text: &str) -> Result<VarLayer, String> {
        Doc::parse(source, text, "cannot parse vars file").map(VarLayer)
    }

    /// The keys of one table inside the file `text`, with their lines.
    pub(crate) fn from_table(
        source: &str,
        text: &str,
        spanned: &toml::de::DeTable<'_>,
        values: &toml::Table,
    ) -> VarLayer {
        VarLayer(Doc::from_tables(source, text, spanned, values))
    }

    /// Unknown keys against the shape of `V`: the check [`load_layers`] runs.
    pub fn check<V: DeserializeOwned>(&self) -> Result<(), String> {
        self.0.check(&probe::<V>()?)
    }
}

/// [`load`] over several layers, the first the highest priority: for each
/// field the first layer with a value wins (within a layer a role's section
/// beats a top-level key), then `defaults`, then the role's `Default`.
pub fn load_layers<V: DeserializeOwned>(layers: &[&VarLayer], defaults: &str) -> Result<V, String> {
    let shape = probe::<V>()?;
    let source = layers
        .first()
        .map(|l| l.0.source.clone())
        .unwrap_or_else(|| "vars".into());
    let docs: Vec<&Doc> = layers.iter().map(|l| &l.0).collect();
    let defs = Doc::parse(
        "playbook defaults",
        defaults,
        "cannot parse playbook defaults",
    )?;
    for d in &docs {
        d.check(&shape)?;
    }
    defs.check(&shape)?;
    let (merged, origins) = merge(&shape, &docs, &defs);
    let map_err = |e: toml::de::Error| {
        let line = e.span().map(|s| merged[..s.start].matches('\n').count());
        match line.and_then(|l| origins.iter().find(|(n, _)| *n == l)) {
            Some((
                _,
                Origin {
                    source,
                    line: Some(l),
                    ..
                },
            )) => {
                format!("{source}:{l}: {}", e.message())
            }
            Some((
                _,
                Origin {
                    source,
                    key,
                    line: None,
                },
            )) => {
                format!("{source}: key `{key}`: {}", e.message())
            }
            None => format!("{source}: {}", e.message()),
        }
    };
    // The first parse keeps file:line on a type error. The second walks the
    // value so a `Secret` records its dotted field. A failure of the second
    // after the first succeeded is a bug in the path tracker, returned as-is.
    if let Err(e) = toml::from_str::<V>(&merged) {
        return Err(map_err(e));
    }
    let value: toml::Value = toml::from_str(&merged).map_err(|e| format!("{source}: {e}"))?;
    crate::secret::from_value(&value).map_err(|e| format!("{source}: {e}"))
}

/// Dotted paths of `Secret` fields in `paths` that were given a literal.
/// A reference is not listed. The value is not.
pub fn secret_literals<V: DeserializeOwned>(paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read vars file {}: {e}", path.display()))?;
        let (res, hits) =
            crate::secret::collect_literals(|| load::<V>(&path.display().to_string(), &text, ""));
        res?;
        for h in hits {
            found.push(format!("{}: {h}", path.display()));
        }
    }
    Ok(found)
}

// ---- documents with line numbers ----

struct Entry {
    key: String,
    line: usize,
    value: toml::Value,
    children: Vec<Entry>,
}

struct Doc {
    source: String,
    entries: Vec<Entry>,
}

pub(crate) fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

impl Doc {
    fn parse(source: &str, text: &str, what: &str) -> Result<Doc, String> {
        let fail = |e: &dyn std::fmt::Display| format!("{what} {source}: {e}");
        let spanned = toml::de::DeTable::parse(text).map_err(|e| fail(&e))?;
        let values: toml::Table = toml::from_str(text).map_err(|e| fail(&e))?;
        Ok(Doc::from_tables(source, text, spanned.get_ref(), &values))
    }

    /// The entries of one table and of the tables directly below it. `text`
    /// is the whole file the spans point into.
    fn from_tables(
        source: &str,
        text: &str,
        spanned: &toml::de::DeTable<'_>,
        values: &toml::Table,
    ) -> Doc {
        let mut entries = Vec::new();
        for (k, v) in spanned.iter() {
            let key = k.get_ref().to_string();
            let value = values
                .get(&key)
                .cloned()
                .unwrap_or(toml::Value::Boolean(false));
            let mut children = Vec::new();
            if let toml::de::DeValue::Table(t) = v.get_ref() {
                for (ck, _) in t.iter() {
                    let name = ck.get_ref().to_string();
                    let cv = value
                        .get(&name)
                        .cloned()
                        .unwrap_or(toml::Value::Boolean(false));
                    children.push(Entry {
                        key: name,
                        line: line_of(text, ck.span().start),
                        value: cv,
                        children: Vec::new(),
                    });
                }
            }
            entries.push(Entry {
                key,
                line: line_of(text, k.span().start),
                value,
                children,
            });
        }
        Doc {
            source: source.to_string(),
            entries,
        }
    }

    fn top(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    fn at(&self, line: usize, msg: String) -> String {
        if self.source == "playbook defaults" {
            format!("{} (line {line}): {msg}", self.source)
        } else {
            format!("{}:{line}: {msg}", self.source)
        }
    }

    /// Unknown keys, at top level and inside struct sections; a section
    /// name given a non-table value.
    fn check(&self, shape: &Shape) -> Result<(), String> {
        for e in &self.entries {
            let section = shape.sections.iter().find(|s| s.name == e.key);
            let is_field = shape.sections.iter().any(|s| s.has_field(&e.key));
            match section {
                None if !is_field => {
                    return Err(self.at(
                        e.line,
                        format!(
                            "unknown key `{}`: no role has a section or a field of that name",
                            e.key
                        ),
                    ));
                }
                Some(Section {
                    fields: Some(fields),
                    ..
                }) => {
                    if !e.value.is_table() {
                        if !is_field {
                            return Err(self.at(
                                e.line,
                                format!("wrong type for `{}`: expected a table", e.key),
                            ));
                        }
                        continue;
                    }
                    for c in &e.children {
                        if !fields.contains(&c.key.as_str()) {
                            return Err(self.at(
                                c.line,
                                format!(
                                    "unknown key `{}` in [{}]; fields: {}",
                                    c.key,
                                    e.key,
                                    fields.join(", ")
                                ),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

// ---- merge ----

struct Origin {
    source: String,
    key: String,
    line: Option<usize>,
}

#[derive(Default)]
struct Emitter {
    out: String,
    origins: Vec<(usize, Origin)>,
}

impl Emitter {
    fn line(&mut self, text: &str) {
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn value(&mut self, key: &str, e: &Entry, doc: &Doc) {
        let line = (doc.source != "playbook defaults").then_some(e.line);
        self.origins.push((
            self.out.matches('\n').count(),
            Origin {
                source: doc.source.clone(),
                key: e.key.clone(),
                line,
            },
        ));
        // A string that displays as a multiline TOML value (`probe=1\n`
        // becomes a `"""` string) must stay one line without the newline
        // becoming a space. Other values keep the one-line flattening.
        let rendered = e.value.to_string();
        let v = if rendered.contains('\n') {
            one_line_value(&e.value)
        } else {
            rendered
        };
        self.line(&format!("{} = {v}", quote(key)));
    }
}

fn in_section<'a>(d: &'a Doc, section: &str, field: &str) -> Option<&'a Entry> {
    d.top(section)
        .filter(|e| e.value.is_table())
        .and_then(|e| e.children.iter().find(|c| c.key == field))
}

/// A top-level key as a value for field `f`. A table named like a struct
/// section is that section, not a value for a field of the same name.
fn top_field<'a>(shape: &Shape, d: &'a Doc, f: &str) -> Option<&'a Entry> {
    d.top(f)
        .filter(|e| !(e.value.is_table() && shape.is_struct(f)))
}

fn merge(shape: &Shape, layers: &[&Doc], defs: &Doc) -> (String, Vec<(usize, Origin)>) {
    let mut em = Emitter::default();
    // Pass-through sections first: they are top-level keys.
    for s in shape.sections.iter().filter(|s| s.fields.is_none()) {
        if let Some((e, d)) = layers
            .iter()
            .copied()
            .chain([defs])
            .find_map(|d| d.top(s.name).map(|e| (e, d)))
        {
            em.value(s.name, e, d);
        }
    }
    for s in shape.sections.iter() {
        let Some(fields) = s.fields else { continue };
        em.line(&format!("[{}]", quote(s.name)));
        for f in fields {
            let hit = layers.iter().copied().chain([defs]).find_map(|d| {
                in_section(d, s.name, f)
                    .or_else(|| top_field(shape, d, f))
                    .map(|e| (e, d))
            });
            if let Some((e, d)) = hit {
                em.value(f, e, d);
            }
        }
    }
    (em.out, em.origins)
}

/// A TOML value on one line. A string is a basic string, so a newline in
/// the value is the escape `\n` and round-trips. Anything else keeps the
/// old flattening (a newline in the display becomes a space).
fn one_line_value(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => {
            let mut o = String::from("\"");
            for c in s.chars() {
                match c {
                    '\\' | '"' => {
                        o.push('\\');
                        o.push(c);
                    }
                    '\u{0008}' => o.push_str("\\b"),
                    '\t' => o.push_str("\\t"),
                    '\n' => o.push_str("\\n"),
                    '\u{000c}' => o.push_str("\\f"),
                    '\r' => o.push_str("\\r"),
                    c if c.is_control() => o.push_str(&format!("\\u{:04X}", u32::from(c))),
                    c => o.push(c),
                }
            }
            o.push('"');
            o
        }
        other => other.to_string().replace('\n', " "),
    }
}

fn quote(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k.to_string()
    } else {
        toml::Value::String(k.to_string()).to_string()
    }
}

// ---- the probing deserializer ----

struct Section {
    name: &'static str,
    /// `Some` when the section type is a struct.
    fields: Option<&'static [&'static str]>,
}

impl Section {
    fn has_field(&self, f: &str) -> bool {
        self.fields.is_some_and(|fs| fs.contains(&f))
    }
}

struct Shape {
    sections: Vec<Section>,
}

impl Shape {
    fn is_struct(&self, name: &str) -> bool {
        self.sections
            .iter()
            .any(|s| s.name == name && s.fields.is_some())
    }
}

#[derive(Default)]
struct Rec {
    sections: Vec<Section>,
}

fn probe<V: DeserializeOwned>() -> Result<Shape, String> {
    let rec = Rc::new(RefCell::new(Rec::default()));
    V::deserialize(Probe {
        rec: rec.clone(),
        depth: 0,
    })
    .map_err(|e: de::value::Error| format!("cannot learn the sections of the vars type: {e}"))?;
    let sections = std::mem::take(&mut rec.borrow_mut().sections);
    Ok(Shape { sections })
}

/// depth 0: the playbook's `V`; 1: a role section; 2+: anything below.
#[derive(Clone)]
struct Probe {
    rec: Rc<RefCell<Rec>>,
    depth: u8,
}

type E = de::value::Error;

struct FieldMap {
    fields: &'static [&'static str],
    i: usize,
    rec: Rc<RefCell<Rec>>,
    depth: u8,
}

impl<'de> MapAccess<'de> for FieldMap {
    type Error = E;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> Result<Option<K::Value>, E> {
        match self.fields.get(self.i) {
            Some(f) => seed.deserialize((*f).into_deserializer()).map(Some),
            None => Ok(None),
        }
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, E> {
        if self.depth == 1 {
            self.rec.borrow_mut().sections.push(Section {
                name: self.fields[self.i],
                fields: None,
            });
        }
        self.i += 1;
        seed.deserialize(Probe {
            rec: self.rec.clone(),
            depth: self.depth,
        })
    }
}

struct EmptySeq;
impl<'de> SeqAccess<'de> for EmptySeq {
    type Error = E;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, _: T) -> Result<Option<T::Value>, E> {
        Ok(None)
    }
}

struct LeafSeq(Probe, usize);
impl<'de> SeqAccess<'de> for LeafSeq {
    type Error = E;
    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, s: T) -> Result<Option<T::Value>, E> {
        if self.1 == 0 {
            return Ok(None);
        }
        self.1 -= 1;
        s.deserialize(self.0.clone()).map(Some)
    }
}

struct EmptyMap;
impl<'de> MapAccess<'de> for EmptyMap {
    type Error = E;
    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, _: K) -> Result<Option<K::Value>, E> {
        Ok(None)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, _: V) -> Result<V::Value, E> {
        Err(de::Error::custom("no value"))
    }
}

struct FirstVariant(&'static str);
impl<'de> EnumAccess<'de> for FirstVariant {
    type Error = E;
    type Variant = UnitOnly;
    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, UnitOnly), E> {
        Ok((seed.deserialize(self.0.into_deserializer())?, UnitOnly))
    }
}
struct UnitOnly;
impl<'de> VariantAccess<'de> for UnitOnly {
    type Error = E;
    fn unit_variant(self) -> Result<(), E> {
        Ok(())
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, _: T) -> Result<T::Value, E> {
        Err(de::Error::custom("probe: data-carrying enum"))
    }
    fn tuple_variant<V: Visitor<'de>>(self, _: usize, _: V) -> Result<V::Value, E> {
        Err(de::Error::custom("probe: data-carrying enum"))
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        _: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, E> {
        Err(de::Error::custom("probe: data-carrying enum"))
    }
}

macro_rules! leaf {
    ($($m:ident => $v:ident($($e:expr)?)),* $(,)?) => {$(
        fn $m<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, E> {
            visitor.$v($($e)?)
        }
    )*};
}

impl<'de> Deserializer<'de> for Probe {
    type Error = E;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, E> {
        // `Secret` (and any untagged value) deserializes through `deserialize_any`.
        // The probe discards the value; a flat field stays a pass-through section.
        visitor.visit_str("")
    }

    leaf! {
        deserialize_bool => visit_bool(false),
        deserialize_i8 => visit_i8(0),
        deserialize_i16 => visit_i16(0),
        deserialize_i32 => visit_i32(0),
        deserialize_i64 => visit_i64(0),
        deserialize_i128 => visit_i128(0),
        deserialize_u8 => visit_u8(0),
        deserialize_u16 => visit_u16(0),
        deserialize_u32 => visit_u32(0),
        deserialize_u64 => visit_u64(0),
        deserialize_u128 => visit_u128(0),
        deserialize_f32 => visit_f32(0.0),
        deserialize_f64 => visit_f64(0.0),
        deserialize_char => visit_char('a'),
        deserialize_str => visit_str(""),
        deserialize_string => visit_str(""),
        deserialize_bytes => visit_bytes(&[]),
        deserialize_byte_buf => visit_bytes(&[]),
        deserialize_unit => visit_unit(),
        deserialize_identifier => visit_str(""),
        deserialize_ignored_any => visit_unit(),
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, E> {
        // An optional section is probed through; below that, absent.
        if self.depth <= 1 {
            visitor.visit_some(self)
        } else {
            visitor.visit_none()
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, E> {
        visitor.visit_unit()
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, E> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, E> {
        visitor.visit_seq(EmptySeq)
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, E> {
        let mut leaf = self;
        leaf.depth = leaf.depth.max(2);
        visitor.visit_seq(LeafSeq(leaf, len))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, E> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, E> {
        visitor.visit_map(EmptyMap)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, E> {
        match self.depth {
            0 => visitor.visit_map(FieldMap {
                fields,
                i: 0,
                rec: self.rec,
                depth: 1,
            }),
            d => {
                if d == 1
                    && let Some(s) = self.rec.borrow_mut().sections.last_mut()
                {
                    s.fields = Some(fields);
                }
                visitor.visit_map(FieldMap {
                    fields,
                    i: 0,
                    rec: self.rec,
                    depth: 2,
                })
            }
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, E> {
        match variants.first() {
            Some(v) => visitor.visit_enum(FirstVariant(v)),
            None => Err(de::Error::custom("enum without variants")),
        }
    }
}

//! Vars per role: four layers merged at the TOML level, then deserialized
//! into the playbook's `V`.
//!
//! `V` is a struct of role sections, each field a role's own vars struct.
//! A value for field `f` of section `s` is the first of:
//!
//! 1. the vars file's `[s]` table, key `f`;
//! 2. the vars file's top-level key `f` (every section with a field of
//!    that name gets it);
//! 3. the playbook's defaults (a TOML text of the same shape);
//! 4. the section struct's own `Default` (`#[serde(default)]`).
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
use std::path::Path;
use std::rc::Rc;

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

/// Load `V` from vars `text` (called `source` in errors) and `defaults`.
pub fn load<V: DeserializeOwned>(source: &str, text: &str, defaults: &str) -> Result<V, String> {
    let shape = probe::<V>()?;
    let file = Doc::parse(source, text, "cannot parse vars file")?;
    let defs = Doc::parse(
        "playbook defaults",
        defaults,
        "cannot parse playbook defaults",
    )?;
    file.check(&shape)?;
    defs.check(&shape)?;
    let (merged, origins) = merge(&shape, &file, &defs);
    toml::from_str::<V>(&merged).map_err(|e| {
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
    })
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

fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

impl Doc {
    fn parse(source: &str, text: &str, what: &str) -> Result<Doc, String> {
        let fail = |e: &dyn std::fmt::Display| format!("{what} {source}: {e}");
        let spanned = toml::de::DeTable::parse(text).map_err(|e| fail(&e))?;
        let values: toml::Table = toml::from_str(text).map_err(|e| fail(&e))?;
        let mut entries = Vec::new();
        for (k, v) in spanned.get_ref().iter() {
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
        Ok(Doc {
            source: source.to_string(),
            entries,
        })
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
        let v = e.value.to_string().replace('\n', " ");
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

fn merge(shape: &Shape, file: &Doc, defs: &Doc) -> (String, Vec<(usize, Origin)>) {
    let mut em = Emitter::default();
    // Pass-through sections first: they are top-level keys.
    for s in shape.sections.iter().filter(|s| s.fields.is_none()) {
        if let Some((e, d)) = [file, defs]
            .into_iter()
            .find_map(|d| d.top(s.name).map(|e| (e, d)))
        {
            em.value(s.name, e, d);
        }
    }
    for s in shape.sections.iter() {
        let Some(fields) = s.fields else { continue };
        em.line(&format!("[{}]", quote(s.name)));
        for f in fields {
            let hit = [
                in_section(file, s.name, f).map(|e| (e, file)),
                top_field(shape, file, f).map(|e| (e, file)),
                in_section(defs, s.name, f).map(|e| (e, defs)),
                top_field(shape, defs, f).map(|e| (e, defs)),
            ]
            .into_iter()
            .flatten()
            .next();
            if let Some((e, d)) = hit {
                em.value(f, e, d);
            }
        }
    }
    (em.out, em.origins)
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

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, E> {
        Err(de::Error::custom(
            "the vars type uses deserialize_any, which cannot be probed",
        ))
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

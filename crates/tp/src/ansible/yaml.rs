//! A small owned YAML tree that keeps the source line of every node.
//!
//! This is the only module that touches the `saphyr` API; everything else
//! works on [`Node`], so the YAML crate can be swapped without ripple.

use saphyr::{LoadableYamlNode, MarkedYaml, Scalar, YamlData};

/// One YAML node with the 1-based line where it starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub line: usize,
    pub val: Val,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Seq(Vec<Node>),
    /// Mapping with keys stringified, in document order.
    Map(Vec<(String, Node)>),
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.val {
            Val::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_seq(&self) -> Option<&[Node]> {
        match &self.val {
            Val::Seq(s) => Some(s),
            _ => None,
        }
    }

    /// The scalar rendered as a string (strings as-is, numbers and bools in
    /// their YAML spelling); `None` for null, sequences and mappings.
    pub fn scalar(&self) -> Option<String> {
        match &self.val {
            Val::Bool(b) => Some(b.to_string()),
            Val::Int(i) => Some(i.to_string()),
            Val::Float(f) => Some(f.to_string()),
            Val::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Ansible truthiness for a literal (`yes`/`true`/...). Templated or
    /// non-boolean values are `None`.
    pub fn truthy(&self) -> Option<bool> {
        match &self.val {
            Val::Bool(b) => Some(*b),
            Val::Str(s) => match s.to_ascii_lowercase().as_str() {
                "yes" | "true" | "on" | "1" => Some(true),
                "no" | "false" | "off" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match &self.val {
            Val::Null => J::Null,
            Val::Bool(b) => J::Bool(*b),
            Val::Int(i) => J::from(*i),
            Val::Float(f) => serde_json::Number::from_f64(*f).map_or(J::Null, J::Number),
            Val::Str(s) => J::String(s.clone()),
            Val::Seq(s) => J::Array(s.iter().map(Node::to_json).collect()),
            Val::Map(m) => J::Object(m.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
        }
    }
}

/// Parse `source` (which may hold several documents) into [`Node`]s.
pub fn parse(source: &str) -> Result<Vec<Node>, String> {
    let docs = MarkedYaml::load_from_str(source).map_err(|e| e.to_string())?;
    Ok(docs.iter().map(convert).collect())
}

fn scalar_val(s: &Scalar<'_>) -> Val {
    match s {
        Scalar::Null => Val::Null,
        Scalar::Boolean(b) => Val::Bool(*b),
        Scalar::Integer(i) => Val::Int(*i),
        Scalar::FloatingPoint(f) => Val::Float(f.0),
        Scalar::String(s) => Val::Str(s.to_string()),
    }
}

fn convert(y: &MarkedYaml<'_>) -> Node {
    let line = y.span.start.line();
    let val = match &y.data {
        YamlData::Value(s) => scalar_val(s),
        YamlData::Representation(s, _, _) => Val::Str(s.to_string()),
        YamlData::Sequence(items) => Val::Seq(items.iter().map(convert).collect()),
        YamlData::Mapping(m) => Val::Map(
            m.iter()
                .map(|(k, v)| {
                    let key = match convert(k).val {
                        Val::Str(s) => s,
                        other => Node {
                            line: 0,
                            val: other,
                        }
                        .scalar()
                        .unwrap_or_default(),
                    };
                    (key, convert(v))
                })
                .collect(),
        ),
        YamlData::Tagged(_, inner) => return convert(inner),
        YamlData::Alias(_) | YamlData::BadValue => Val::Null,
    };
    Node { line, val }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_lines() {
        let docs = parse("---\n- name: a\n  apt: x\n\n- name: b\n  shell: y\n").unwrap();
        let items = docs[0].as_seq().unwrap();
        assert_eq!(items[0].line, 2);
        assert_eq!(items[1].line, 5);
        assert_eq!(items[1].get("shell").unwrap().scalar().unwrap(), "y");
    }
}

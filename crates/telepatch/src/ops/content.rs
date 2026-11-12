//! Bytes or an askama template held until render.
//!
//! A template is rendered when the op's script is built. `reveal` is off
//! for every reader (plan, `Debug`, the log): a `Secret` prints `<secret>`,
//! and if one was formatted the body is omitted and the path is marked
//! `(secret)`. The host script renders with `reveal` on, after prepare has
//! filled the secret cache.

use std::fmt;
use std::sync::OnceLock;

use crate::secret::{self, Secret};

/// Marker for [`FileContent`] implementations that are raw bytes.
pub struct BytesKind;

/// Marker for [`FileContent`] implementations that are askama templates.
pub struct TemplateKind;

/// What [`.content(..)`](crate::ops::file::FileHandle::content) accepts.
///
/// Bytes (`&str`, `String`, `&[u8]`, `Vec<u8>`) and an askama [`askama::Template`]
/// value (cloned into the trace, rendered per host). The two kinds are
/// different implementations of this trait so a byte type and a template
/// type do not overlap.
pub trait FileContent<Kind> {
    fn into_content(self) -> Content;
}

/// File or unit body: bytes, or a template rendered later.
#[derive(Clone)]
pub struct Content {
    inner: Inner,
}

#[derive(Clone)]
enum Inner {
    Bytes(Vec<u8>),
    Tpl(Box<dyn Held>),
}

enum Material {
    Bytes(Vec<u8>),
    /// A secret was formatted. The redacted script must not carry the body.
    Hidden,
    /// Reveal found a reference prepare did not resolve, or rendering failed.
    Broken,
}

struct Inspected {
    redacted: Result<String, ()>,
    secret: bool,
    secrets: Vec<Secret>,
}

trait Held: Send {
    fn inspect(&self) -> &Inspected;
    fn materialize(&self, reveal: bool) -> Material;
    fn clone_box(&self) -> Box<dyn Held>;
}

struct Wrap<T> {
    value: T,
    cached: OnceLock<Inspected>,
}

impl<T> Held for Wrap<T>
where
    T: askama::Template + Clone + Send + 'static,
{
    fn inspect(&self) -> &Inspected {
        self.cached.get_or_init(|| {
            let (text, out) = secret::with_render(false, true, || self.value.render());
            match text {
                Ok(redacted) => Inspected {
                    redacted: Ok(redacted),
                    secret: out.secret,
                    secrets: out.secrets,
                },
                Err(_) => Inspected {
                    redacted: Err(()),
                    secret: out.secret,
                    secrets: out.secrets,
                },
            }
        })
    }

    fn materialize(&self, reveal: bool) -> Material {
        let info = self.inspect();
        if !reveal {
            return match &info.redacted {
                Ok(text) if !info.secret => Material::Bytes(text.as_bytes().to_vec()),
                Ok(_) => Material::Hidden,
                Err(_) => Material::Broken,
            };
        }
        let (text, out) = secret::with_render(true, false, || self.value.render());
        match text {
            Ok(text) if !out.unresolved => Material::Bytes(text.into_bytes()),
            _ => Material::Broken,
        }
    }

    fn clone_box(&self) -> Box<dyn Held> {
        Box::new(Wrap {
            value: self.value.clone(),
            cached: OnceLock::new(),
        })
    }
}

impl Clone for Box<dyn Held> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

impl Content {
    fn materialize(&self, reveal: bool) -> Material {
        match &self.inner {
            Inner::Bytes(b) => Material::Bytes(b.clone()),
            Inner::Tpl(t) => t.materialize(reveal),
        }
    }

    /// Plain bytes, when this is not a template.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match &self.inner {
            Inner::Bytes(b) => Some(b),
            Inner::Tpl(_) => None,
        }
    }

    pub(crate) fn is_secret(&self) -> bool {
        match &self.inner {
            Inner::Bytes(_) => false,
            Inner::Tpl(t) => t.inspect().secret,
        }
    }

    /// References a redacted render formatted. Literals are included; the
    /// caller skips those that are already values.
    pub(crate) fn secret_refs(&self) -> Vec<Secret> {
        match &self.inner {
            Inner::Bytes(_) => Vec::new(),
            Inner::Tpl(t) => t.inspect().secrets.clone(),
        }
    }

    /// Bytes to send to the host (checksum or write). Empty when the
    /// template could not be revealed; the caller must not treat that as
    /// the wanted body.
    pub(crate) fn host_bytes(&self) -> Option<Vec<u8>> {
        match self.materialize(true) {
            Material::Bytes(b) => Some(b),
            Material::Hidden | Material::Broken => None,
        }
    }

    pub(crate) fn script_body(&self, reveal: bool) -> ScriptBody {
        match self.materialize(reveal) {
            Material::Bytes(b) => ScriptBody::Bytes(b),
            Material::Hidden => ScriptBody::Hidden,
            Material::Broken => ScriptBody::Broken,
        }
    }
}

/// How a body is placed into a shell script.
pub(crate) enum ScriptBody {
    Bytes(Vec<u8>),
    Hidden,
    Broken,
}

impl PartialEq for Content {
    fn eq(&self, other: &Self) -> bool {
        match (&self.inner, &other.inner) {
            (Inner::Bytes(a), Inner::Bytes(b)) => a == b,
            (Inner::Tpl(a), Inner::Tpl(b)) => {
                let ia = a.inspect();
                let ib = b.inspect();
                ia.redacted == ib.redacted && ia.secrets == ib.secrets
            }
            (Inner::Bytes(b), Inner::Tpl(t)) | (Inner::Tpl(t), Inner::Bytes(b)) => {
                let info = t.inspect();
                !info.secret && info.redacted.as_ref().ok().map(String::as_bytes) == Some(b)
            }
        }
    }
}

impl Eq for Content {}

impl fmt::Debug for Content {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.inner {
            Inner::Bytes(b) => fmt::Debug::fmt(b, f),
            Inner::Tpl(t) => {
                let info = t.inspect();
                match &info.redacted {
                    Ok(text) if !info.secret => fmt::Debug::fmt(text.as_bytes(), f),
                    _ => f.write_str("(secret)"),
                }
            }
        }
    }
}

fn from_bytes(bytes: Vec<u8>) -> Content {
    Content {
        inner: Inner::Bytes(bytes),
    }
}

impl FileContent<BytesKind> for Vec<u8> {
    fn into_content(self) -> Content {
        from_bytes(self)
    }
}
impl FileContent<BytesKind> for &Vec<u8> {
    fn into_content(self) -> Content {
        from_bytes(self.clone())
    }
}
impl FileContent<BytesKind> for &[u8] {
    fn into_content(self) -> Content {
        from_bytes(self.to_vec())
    }
}
impl<const N: usize> FileContent<BytesKind> for &[u8; N] {
    fn into_content(self) -> Content {
        from_bytes(self.to_vec())
    }
}
impl FileContent<BytesKind> for String {
    fn into_content(self) -> Content {
        from_bytes(self.into_bytes())
    }
}
impl FileContent<BytesKind> for &String {
    fn into_content(self) -> Content {
        from_bytes(self.as_bytes().to_vec())
    }
}
impl FileContent<BytesKind> for &str {
    fn into_content(self) -> Content {
        from_bytes(self.as_bytes().to_vec())
    }
}

impl<T> FileContent<TemplateKind> for T
where
    T: askama::Template + Clone + Send + 'static,
{
    fn into_content(self) -> Content {
        Content {
            inner: Inner::Tpl(Box::new(Wrap {
                value: self,
                cached: OnceLock::new(),
            })),
        }
    }
}

impl From<Vec<u8>> for Content {
    fn from(bytes: Vec<u8>) -> Content {
        bytes.into_content()
    }
}
impl From<&[u8]> for Content {
    fn from(b: &[u8]) -> Content {
        b.into_content()
    }
}
impl<const N: usize> From<&[u8; N]> for Content {
    fn from(b: &[u8; N]) -> Content {
        b.into_content()
    }
}
impl From<&Vec<u8>> for Content {
    fn from(b: &Vec<u8>) -> Content {
        b.into_content()
    }
}
impl From<&str> for Content {
    fn from(s: &str) -> Content {
        s.into_content()
    }
}
impl From<String> for Content {
    fn from(s: String) -> Content {
        s.into_content()
    }
}
impl From<&String> for Content {
    fn from(s: &String) -> Content {
        s.into_content()
    }
}

//! `inventory.toml`: hosts, groups, and vars for each, in one file.
//!
//! ```toml
//! [vars]                                  # every host
//! admin_users = ["bot"]
//!
//! [groups.builders]
//! hosts = ["bot@build-01", "bot@build-02"]
//! vars = { rust_build = { rustc_version = "1.94.0" } }
//!
//! [groups.gpu]
//! hosts = ["bot@gpu-01"]
//!
//! [hosts."bot@gpu-01"]
//! groups = ["builders"]
//! vars = { admin_users = ["bot", "ml"] }
//! ```
//!
//! A host's name is the destination `ssh` gets, verbatim, and its name in
//! the report: `user@host`, or `ssh://user@host:port` when it needs a port
//! (the same forms as `--host`).
//!
//! **Membership** is the union of the two ways to say it: a group's
//! `hosts` list and a host's `groups` list. A host named in either exists;
//! a group a host names must have a `[groups.*]` table. A host's groups are
//! kept in the order the `[groups.*]` tables appear in the file; hosts in
//! the order each is first named.
//!
//! **Vars** for a host, highest priority first ([`Inventory::vars_for`]):
//!
//! 1. the host's own `vars`;
//! 2. its groups' `vars`, the group listed last first (a later group wins);
//! 3. the inventory's `[vars]`, for every host;
//! 4. the vars file (`--vars`);
//! 5. the playbook's defaults, then each role's `Default`.
//!
//! Inside each of 1 to 4 a role's section beats a top-level key (see
//! [`crate::vars`]). Every error names the file and the 1-based line.
//!
//! `--limit` takes group and host names ([`Inventory::limit`]).

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use toml::de::{DeTable, DeValue};

use crate::vars::{VarLayer, line_of, load_layers};

/// A parsed inventory.
pub struct Inventory {
    vars: Option<VarLayer>,
    groups: Vec<Group>,
    hosts: Vec<InvHost>,
}

struct Group {
    name: String,
    vars: Option<VarLayer>,
}

/// One host of the inventory.
pub struct InvHost {
    /// The `ssh` destination, and the name in the report.
    pub name: String,
    /// The groups it is in, in the order the group tables are listed.
    pub groups: Vec<String>,
    vars: Option<VarLayer>,
}

// What parsing collects before membership is joined.
struct GroupDecl {
    pos: usize,
    name: String,
    hosts: Vec<(usize, String)>,
    vars: Option<VarLayer>,
}

struct HostDecl {
    pos: usize,
    name: String,
    groups: Vec<(usize, String)>,
    vars: Option<VarLayer>,
}

impl Inventory {
    /// Read and parse the inventory at `path`.
    pub fn load(path: &Path) -> Result<Inventory, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read inventory {}: {e}", path.display()))?;
        Inventory::parse(&path.display().to_string(), &text)
    }

    /// Parse inventory `text`, called `source` in errors.
    pub fn parse(source: &str, text: &str) -> Result<Inventory, String> {
        let fail = |e: &dyn std::fmt::Display| format!("cannot parse inventory {source}: {e}");
        let spanned = DeTable::parse(text).map_err(|e| fail(&e))?;
        let values: toml::Table = toml::from_str(text).map_err(|e| fail(&e))?;
        let p = Parser {
            source,
            text,
            values: &values,
        };

        let mut vars = None;
        let mut groups: Vec<GroupDecl> = Vec::new();
        let mut hosts: Vec<HostDecl> = Vec::new();
        for (k, v) in spanned.get_ref().iter() {
            match k.get_ref().as_ref() {
                "vars" => {
                    let t = p.table(v.get_ref(), v.span().start, "`vars`")?;
                    vars = Some(p.layer(t, &["vars"]));
                }
                "groups" => {
                    let t = p.table(v.get_ref(), v.span().start, "`groups`")?;
                    for (gk, gv) in t.iter() {
                        groups.push(p.group(gk.get_ref(), gk.span().start, gv.get_ref())?);
                    }
                }
                "hosts" => {
                    let t = p.table(v.get_ref(), v.span().start, "`hosts`")?;
                    for (hk, hv) in t.iter() {
                        hosts.push(p.host(hk.get_ref(), hk.span().start, hv.get_ref())?);
                    }
                }
                other => {
                    return Err(p.at(
                        k.span().start,
                        format!("unknown key `{other}`; expected vars, groups, hosts"),
                    ));
                }
            }
        }
        // file order, whatever order the TOML map iterates in
        groups.sort_by_key(|g| g.pos);
        hosts.sort_by_key(|h| h.pos);

        let known: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        for h in &hosts {
            for (pos, g) in &h.groups {
                if !known.contains(&g.as_str()) {
                    return Err(p.at(
                        *pos,
                        format!(
                            "host `{}` names group `{g}`, which is not defined; groups: {}",
                            h.name,
                            known.join(", ")
                        ),
                    ));
                }
            }
        }

        // hosts in the order each is first named
        let mut mentions: Vec<(usize, &str)> = hosts
            .iter()
            .map(|h| (h.pos, h.name.as_str()))
            .chain(
                groups
                    .iter()
                    .flat_map(|g| g.hosts.iter().map(|(pos, n)| (*pos, n.as_str()))),
            )
            .collect();
        mentions.sort_by_key(|(pos, _)| *pos);
        let mut names: Vec<&str> = Vec::new();
        for (_, n) in mentions {
            if !names.contains(&n) {
                names.push(n);
            }
        }

        let mut out_hosts = Vec::new();
        for name in names {
            let decl = hosts.iter().position(|h| h.name == name);
            let in_groups: Vec<String> = groups
                .iter()
                .filter(|g| {
                    g.hosts.iter().any(|(_, n)| n == name)
                        || decl.is_some_and(|i| hosts[i].groups.iter().any(|(_, x)| *x == g.name))
                })
                .map(|g| g.name.clone())
                .collect();
            out_hosts.push((name.to_string(), in_groups, decl));
        }
        let out_hosts = out_hosts
            .into_iter()
            .map(|(name, groups, decl)| InvHost {
                name,
                groups,
                vars: decl.and_then(|i| hosts[i].vars.take()),
            })
            .collect();
        Ok(Inventory {
            vars,
            groups: groups
                .into_iter()
                .map(|g| Group {
                    name: g.name,
                    vars: g.vars,
                })
                .collect(),
            hosts: out_hosts,
        })
    }

    /// Every host, in the order each is first named.
    pub fn hosts(&self) -> &[InvHost] {
        &self.hosts
    }

    /// Every group, in the order the group tables are listed.
    pub fn group_names(&self) -> Vec<&str> {
        self.groups.iter().map(|g| g.name.as_str()).collect()
    }

    /// The hosts `--limit` names: each name is a group (all its hosts) or a
    /// host; the result is the union, once each, in inventory order. A name
    /// that is neither, or a limit that selects no host, is an error that
    /// lists what exists.
    pub fn limit(&self, names: &[String]) -> Result<Vec<&InvHost>, String> {
        let mut picked = vec![false; self.hosts.len()];
        for n in names {
            let is_group = self.groups.iter().any(|g| g.name == *n);
            let is_host = self.hosts.iter().any(|h| h.name == *n);
            if !is_group && !is_host {
                return Err(format!(
                    "unknown --limit name `{n}`; groups: {}; hosts: {}",
                    self.group_names().join(", "),
                    self.hosts
                        .iter()
                        .map(|h| h.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            for (i, h) in self.hosts.iter().enumerate() {
                if h.name == *n || h.groups.contains(n) {
                    picked[i] = true;
                }
            }
        }
        let out: Vec<&InvHost> = self
            .hosts
            .iter()
            .zip(picked)
            .filter_map(|(h, p)| p.then_some(h))
            .collect();
        if out.is_empty() {
            return Err(format!(
                "--limit {} selects no hosts",
                names
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Ok(out)
    }

    /// Every `vars` of the file, with a name for where it is.
    fn layers(&self) -> Vec<(String, &VarLayer)> {
        let mut out = Vec::new();
        if let Some(l) = &self.vars {
            out.push(("[vars]".to_string(), l));
        }
        for g in &self.groups {
            if let Some(l) = &g.vars {
                out.push((format!("group {}", g.name), l));
            }
        }
        for h in &self.hosts {
            if let Some(l) = &h.vars {
                out.push((format!("host {}", h.name), l));
            }
        }
        out
    }

    /// Check every `vars` of the file, used or not, against the shape of
    /// `V`: an unknown key is an error naming the file and line.
    pub fn check_vars<V: DeserializeOwned>(&self) -> Result<(), String> {
        for (_, l) in self.layers() {
            l.check::<V>()?;
        }
        Ok(())
    }

    /// The vars for `host`: its own, its groups' (the last listed first),
    /// the inventory's, then `file` (the vars file) and `defaults` (the
    /// playbook's, as TOML text), then each role's `Default`.
    pub fn vars_for<V: DeserializeOwned>(
        &self,
        host: &InvHost,
        file: Option<&VarLayer>,
        defaults: &str,
    ) -> Result<V, String> {
        let mut layers: Vec<&VarLayer> = Vec::new();
        layers.extend(host.vars.as_ref());
        for g in host.groups.iter().rev() {
            layers.extend(
                self.groups
                    .iter()
                    .find(|x| x.name == *g)
                    .and_then(|x| x.vars.as_ref()),
            );
        }
        layers.extend(self.vars.as_ref());
        layers.extend(file);
        load_layers(&layers, defaults)
    }
}

/// `Secret` fields given a literal in the inventory files at `paths`: one
/// entry per field and place, `<path>: <dotted field> (in <place>)`. A
/// reference is not listed, nor is a value. The gate check for "a tracked
/// inventory file must not give a `Secret` a literal"
/// ([`crate::vars::secret_literals`] is the same for vars files).
pub fn secret_literals<V: DeserializeOwned>(paths: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for path in paths {
        let inv = Inventory::load(path)?;
        for (place, layer) in inv.layers() {
            let (res, hits) = crate::secret::collect_literals(|| load_layers::<V>(&[layer], ""));
            res?;
            for h in hits {
                found.push(format!("{}: {h} (in {place})", path.display()));
            }
        }
    }
    Ok(found)
}

struct Parser<'a> {
    source: &'a str,
    text: &'a str,
    values: &'a toml::Table,
}

impl Parser<'_> {
    fn at(&self, pos: usize, msg: String) -> String {
        format!("{}:{}: {msg}", self.source, line_of(self.text, pos))
    }

    fn table<'t, 'de>(
        &self,
        v: &'t DeValue<'de>,
        pos: usize,
        what: &str,
    ) -> Result<&'t DeTable<'de>, String> {
        match v {
            DeValue::Table(t) => Ok(t),
            _ => Err(self.at(pos, format!("{what} must be a table"))),
        }
    }

    /// A vars layer from the table at `path` in the file.
    fn layer(&self, t: &DeTable<'_>, path: &[&str]) -> VarLayer {
        let mut cur = Some(self.values);
        for p in path {
            cur = cur.and_then(|c| c.get(*p)).and_then(|v| v.as_table());
        }
        let empty = toml::Table::new();
        VarLayer::from_table(self.source, self.text, t, cur.unwrap_or(&empty))
    }

    fn strings(
        &self,
        v: &toml::Spanned<DeValue<'_>>,
        what: &str,
    ) -> Result<Vec<(usize, String)>, String> {
        let bad = |pos| self.at(pos, format!("{what} must be an array of strings"));
        let DeValue::Array(a) = v.get_ref() else {
            return Err(bad(v.span().start));
        };
        a.iter()
            .map(|e| match e.get_ref() {
                DeValue::String(s) => Ok((e.span().start, s.to_string())),
                _ => Err(bad(e.span().start)),
            })
            .collect()
    }

    fn group(&self, name: &str, pos: usize, v: &DeValue<'_>) -> Result<GroupDecl, String> {
        let t = self.table(v, pos, &format!("group `{name}`"))?;
        let mut g = GroupDecl {
            pos,
            name: name.to_string(),
            hosts: Vec::new(),
            vars: None,
        };
        for (k, v) in t.iter() {
            match k.get_ref().as_ref() {
                "hosts" => {
                    g.hosts = self.strings(v, &format!("`hosts` of group `{name}`"))?;
                    if let Some((p, _)) = g.hosts.iter().find(|(_, n)| n.is_empty()) {
                        return Err(self.at(*p, "a host name is empty".into()));
                    }
                }
                "vars" => {
                    let vt = self.table(
                        v.get_ref(),
                        v.span().start,
                        &format!("`vars` of group `{name}`"),
                    )?;
                    g.vars = Some(self.layer(vt, &["groups", name, "vars"]));
                }
                other => {
                    return Err(self.at(
                        k.span().start,
                        format!("unknown key `{other}` in [groups.{name}]; expected hosts, vars"),
                    ));
                }
            }
        }
        Ok(g)
    }

    fn host(&self, name: &str, pos: usize, v: &DeValue<'_>) -> Result<HostDecl, String> {
        if name.is_empty() {
            return Err(self.at(pos, "a host name is empty".into()));
        }
        let t = self.table(v, pos, &format!("host `{name}`"))?;
        let mut h = HostDecl {
            pos,
            name: name.to_string(),
            groups: Vec::new(),
            vars: None,
        };
        for (k, v) in t.iter() {
            match k.get_ref().as_ref() {
                "groups" => {
                    h.groups = self.strings(v, &format!("`groups` of host `{name}`"))?;
                }
                "vars" => {
                    let vt = self.table(
                        v.get_ref(),
                        v.span().start,
                        &format!("`vars` of host `{name}`"),
                    )?;
                    h.vars = Some(self.layer(vt, &["hosts", name, "vars"]));
                }
                other => {
                    return Err(self.at(
                        k.span().start,
                        format!(
                            "unknown key `{other}` in [hosts.\"{name}\"]; expected groups, vars"
                        ),
                    ));
                }
            }
        }
        Ok(h)
    }
}

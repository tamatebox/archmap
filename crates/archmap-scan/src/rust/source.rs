//! Per-file facts from `syn`: `mod` declarations, the names each module
//! defines, `use` declarations and `pub` symbols. Nothing here knows where a
//! file sits in its crate; [`super::tree`] places files in module trees.
//!
//! This is a structural scan. Function bodies are parsed by `syn` but only
//! visited for `use` declarations, which are recorded with local scope.

use std::collections::{BTreeMap, BTreeSet};

use archmap_core::{Scope, SymbolKind};
use quote::ToTokens;
use syn::ext::IdentExt;
use syn::punctuated::Punctuated;
use syn::visit::Visit;
use syn::{
    Attribute, Block, Ident, ImplItem, Item, ItemUse, Meta, Token, TraitItem, UseTree, Visibility,
};

/// What one file declares. `modules[0]` is the file's own module; the others
/// are the inline modules (`mod name { .. }`) inside it, in source order.
#[derive(Debug, Clone)]
pub(super) struct RustFile {
    pub modules: Vec<ModuleFacts>,
}

/// What one module declares, whether a file or an inline module.
#[derive(Debug, Clone, Default)]
pub(super) struct ModuleFacts {
    /// Whether its `pub` items are public interface: always for the file
    /// module, and for an inline module when it and every inline module
    /// around it are `pub`.
    pub public: bool,
    /// An inline module marked `#[cfg(test)]`.
    pub test: bool,
    /// Inline modules declared here: name -> index in [`RustFile::modules`].
    pub inline: BTreeMap<String, usize>,
    /// `mod name;` declarations, whose content is in another file.
    pub declared: Vec<ModDecl>,
    /// Names of the items defined here, whatever their visibility, so that a
    /// path can tell a definition from an import.
    pub items: BTreeSet<String>,
    /// `#[macro_export]` macros defined here, which live at the crate root.
    pub exported_macros: Vec<String>,
    pub uses: Vec<UseDecl>,
    pub symbols: Vec<SymbolDecl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModDecl {
    pub name: String,
    pub line: u32,
    /// Has a `#[path]` attribute (or a `#[cfg_attr]` with one) naming the
    /// file explicitly.
    pub path_attr: bool,
    /// Marked `#[cfg(test)]`.
    pub test: bool,
}

/// One name that a `use` or `extern crate` declaration brings into scope.
/// A declaration with a group (`use a::{b, c}`) yields one per leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UseDecl {
    /// The path as written, `crate`, `self` and `super` included. For a
    /// glob, the module whose names are imported.
    pub path: Vec<String>,
    /// The name bound in the module; `None` for a glob or `as _`.
    pub binds: Option<String>,
    pub glob: bool,
    /// Starts with `::`, which names an extern crate.
    pub leading_colon: bool,
    /// Visible outside the module (`pub`, `pub(crate)`, ...): a re-export.
    pub reexport: bool,
    pub line: u32,
    /// `Local` inside a function body, `Module` elsewhere.
    pub scope: Scope,
    /// Written in `#[cfg(test)]` code: the declaration itself, or the
    /// function or `impl` around it. Test modules are marked on the module.
    pub test: bool,
    /// `use` or `extern crate`, for evidence notes.
    pub note: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SymbolDecl {
    pub name: String,
    pub kind: SymbolKind,
    pub signature: Option<String>,
    pub line: u32,
}

pub(super) fn parse_file(text: &str) -> syn::Result<RustFile> {
    let file = syn::parse_file(text)?;
    let mut out = RustFile {
        modules: vec![ModuleFacts {
            public: true,
            ..ModuleFacts::default()
        }],
    };
    collect(&file.items, 0, &mut out);
    Ok(out)
}

fn collect(items: &[Item], module: usize, file: &mut RustFile) {
    let public = file.modules[module].public;
    for item in items {
        let test = cfg_test(attrs(item));
        let mut facts = Facts {
            module: &mut file.modules[module],
            public,
        };
        match item {
            Item::Use(u) => {
                let line = line_of(u.use_token.span);
                facts
                    .module
                    .uses
                    .extend(use_decls(u, line, Scope::Module, test));
            }
            Item::ExternCrate(e) => {
                let crate_name = name(&e.ident);
                let binds = match &e.rename {
                    Some((_, rename)) => name(rename),
                    None => crate_name.clone(),
                };
                facts.module.uses.push(UseDecl {
                    path: vec![crate_name],
                    binds: (binds != "_").then_some(binds),
                    glob: false,
                    leading_colon: false,
                    reexport: visible_outside(&e.vis),
                    line: line_of(e.extern_token.span),
                    scope: Scope::Module,
                    test,
                    note: "extern crate",
                });
            }
            Item::Fn(f) => {
                let (vis, sig) = (&f.vis, &f.sig);
                facts.define(&f.sig.ident, vis, SymbolKind::Function, || {
                    render(quote::quote!(#vis #sig))
                });
                local_uses(&f.block, facts.module, test);
            }
            Item::Struct(s) => {
                let (ident, g) = (&s.ident, &s.generics);
                facts.define(ident, &s.vis, SymbolKind::Struct, || {
                    render(quote::quote!(pub struct #ident #g))
                });
            }
            Item::Enum(e) => {
                let (ident, g) = (&e.ident, &e.generics);
                facts.define(ident, &e.vis, SymbolKind::Enum, || {
                    render(quote::quote!(pub enum #ident #g))
                });
            }
            Item::Trait(t) => {
                let (ident, g) = (&t.ident, &t.generics);
                facts.define(ident, &t.vis, SymbolKind::Trait, || {
                    render(quote::quote!(pub trait #ident #g))
                });
                for trait_item in &t.items {
                    if let TraitItem::Fn(method) = trait_item {
                        if let Some(block) = &method.default {
                            local_uses(block, facts.module, test || cfg_test(&method.attrs));
                        }
                    }
                }
            }
            Item::Type(t) => {
                let (ident, g) = (&t.ident, &t.generics);
                facts.define(ident, &t.vis, SymbolKind::TypeAlias, || {
                    render(quote::quote!(pub type #ident #g))
                });
            }
            Item::Const(c) => {
                let (ident, ty) = (&c.ident, &c.ty);
                facts.define(ident, &c.vis, SymbolKind::Constant, || {
                    render(quote::quote!(pub const #ident: #ty))
                });
            }
            Item::Static(s) => {
                let (ident, ty) = (&s.ident, &s.ty);
                facts.define(ident, &s.vis, SymbolKind::Constant, || {
                    render(quote::quote!(pub static #ident: #ty))
                });
            }
            Item::Union(u) => {
                facts.module.items.insert(name(&u.ident));
            }
            Item::TraitAlias(t) => {
                facts.module.items.insert(name(&t.ident));
            }
            Item::Macro(m) => {
                if let Some(ident) = &m.ident {
                    facts.module.items.insert(name(ident));
                    if m.attrs.iter().any(|a| a.path().is_ident("macro_export")) {
                        facts.module.exported_macros.push(name(ident));
                    }
                }
            }
            Item::Mod(m) => {
                let mod_name = name(&m.ident);
                if public && is_pub(&m.vis) {
                    facts.symbol(&mod_name, SymbolKind::Module, None, line_of(m.ident.span()));
                }
                match &m.content {
                    None => facts.module.declared.push(ModDecl {
                        name: mod_name,
                        line: line_of(m.ident.span()),
                        path_attr: m.attrs.iter().any(is_path_attr),
                        test,
                    }),
                    Some((_, items)) => {
                        let index = file.modules.len();
                        file.modules[module].inline.insert(mod_name, index);
                        file.modules.push(ModuleFacts {
                            public: public && is_pub(&m.vis),
                            test,
                            ..ModuleFacts::default()
                        });
                        collect(items, index, file);
                    }
                }
            }
            Item::Impl(imp) => {
                let self_ty = render(imp.self_ty.to_token_stream());
                for impl_item in &imp.items {
                    let ImplItem::Fn(method) = impl_item else {
                        continue;
                    };
                    if public && imp.trait_.is_none() && is_pub(&method.vis) {
                        let (vis, msig) = (&method.vis, &method.sig);
                        facts.symbol(
                            &format!("{self_ty}::{}", method.sig.ident),
                            SymbolKind::Function,
                            Some(render(quote::quote!(#vis #msig))),
                            line_of(method.sig.ident.span()),
                        );
                    }
                    local_uses(&method.block, facts.module, test || cfg_test(&method.attrs));
                }
            }
            _ => {}
        }
    }
}

/// The module being collected, and whether its `pub` items are symbols.
struct Facts<'a> {
    module: &'a mut ModuleFacts,
    public: bool,
}

impl Facts<'_> {
    /// Record an item's name, and a symbol when it is public interface.
    fn define(
        &mut self,
        ident: &Ident,
        vis: &Visibility,
        kind: SymbolKind,
        signature: impl FnOnce() -> String,
    ) {
        let item = name(ident);
        if self.public && is_pub(vis) {
            self.symbol(&item, kind, Some(signature()), line_of(ident.span()));
        }
        self.module.items.insert(item);
    }

    fn symbol(&mut self, name: &str, kind: SymbolKind, signature: Option<String>, line: u32) {
        self.module.symbols.push(SymbolDecl {
            name: name.to_owned(),
            kind,
            signature,
            line,
        });
    }
}

/// `use` declarations anywhere inside a function body. A module declared in
/// a body has a scope of its own and is left out.
fn local_uses(block: &Block, module: &mut ModuleFacts, test: bool) {
    struct Visitor<'a> {
        uses: &'a mut Vec<UseDecl>,
        test: bool,
    }
    impl<'ast> Visit<'ast> for Visitor<'_> {
        fn visit_item_use(&mut self, u: &'ast ItemUse) {
            let line = line_of(u.use_token.span);
            let test = self.test || cfg_test(&u.attrs);
            self.uses.extend(use_decls(u, line, Scope::Local, test));
        }
        fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {}
    }
    Visitor {
        uses: &mut module.uses,
        test,
    }
    .visit_block(block);
}

fn use_decls(u: &ItemUse, line: u32, scope: Scope, test: bool) -> Vec<UseDecl> {
    let mut leaves = Vec::new();
    use_leaves(&u.tree, &mut Vec::new(), &mut leaves);
    leaves
        .into_iter()
        .filter(|(path, _, _)| !path.is_empty())
        .map(|(path, binds, glob)| UseDecl {
            path,
            binds,
            glob,
            leading_colon: u.leading_colon.is_some(),
            // a `use` in a function body is visible only there
            reexport: scope == Scope::Module && visible_outside(&u.vis),
            line,
            scope,
            test,
            note: "use",
        })
        .collect()
}

/// The leaves of a use tree as (path, name bound, glob).
fn use_leaves(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, Option<String>, bool)>,
) {
    let path_to = |prefix: &[String], name: &str| {
        let mut path = prefix.to_vec();
        if name != "self" {
            path.push(name.to_owned());
        }
        path
    };
    match tree {
        UseTree::Path(p) => {
            prefix.push(name(&p.ident));
            use_leaves(&p.tree, prefix, out);
            prefix.pop();
        }
        UseTree::Name(n) => {
            let path = path_to(prefix, &name(&n.ident));
            let binds = path.last().cloned();
            out.push((path, binds, false));
        }
        UseTree::Rename(r) => {
            let rename = name(&r.rename);
            let path = path_to(prefix, &name(&r.ident));
            out.push((path, (rename != "_").then_some(rename), false));
        }
        UseTree::Glob(_) => out.push((prefix.clone(), None, true)),
        UseTree::Group(g) => {
            for tree in &g.items {
                use_leaves(tree, prefix, out);
            }
        }
    }
}

/// An identifier as a name: `r#type` is the module `type`.
fn name(ident: &Ident) -> String {
    ident.unraw().to_string()
}

fn line_of(span: proc_macro2::Span) -> u32 {
    span.start().line as u32
}

fn is_pub(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

/// Visible outside its module: any `pub` but `pub(self)`.
fn visible_outside(vis: &Visibility) -> bool {
    match vis {
        Visibility::Public(_) => true,
        Visibility::Restricted(r) => r.in_token.is_some() || !r.path.is_ident("self"),
        Visibility::Inherited => false,
    }
}

fn attrs(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => &[],
    }
}

/// Compiled only for tests: `#[cfg(test)]`, or `test` inside `all(..)`.
fn cfg_test(attrs: &[Attribute]) -> bool {
    fn requires_test(meta: &Meta) -> bool {
        match meta {
            Meta::Path(path) => path.is_ident("test"),
            Meta::List(list) if list.path.is_ident("all") => list
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .is_ok_and(|metas| metas.iter().any(requires_test)),
            _ => false,
        }
    }
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<Meta>().is_ok_and(|m| requires_test(&m))
    })
}

/// `#[path = ".."]`, or a `#[cfg_attr(.., path = "..")]`.
fn is_path_attr(attr: &Attribute) -> bool {
    if attr.path().is_ident("path") {
        return true;
    }
    attr.path().is_ident("cfg_attr")
        && attr
            .meta
            .require_list()
            .is_ok_and(|list| list.tokens.to_string().contains("path ="))
}

/// Turn a token stream into something a human would write.
///
/// `quote!` output puts spaces around every token; this trims the most
/// distracting ones. It is not a formatter.
fn render(tokens: proc_macro2::TokenStream) -> String {
    let s = tokens.to_string();
    let replacements = [
        (" (", "("),
        ("( ", "("),
        (" )", ")"),
        (" ,", ","),
        (" ;", ";"),
        (" :", ":"),
        (": :", "::"),
        ("< ", "<"),
        (" <", "<"),
        (" >", ">"),
        ("& '", "&'"),
        ("& mut", "&mut"),
        ("& self", "&self"),
        ("&mut  self", "&mut self"),
        (" ' ", "'"),
        ("'a  ", "'a "),
    ];
    let mut out = s;
    for (from, to) in replacements {
        out = out.replace(from, to);
    }
    // Collapse `& Path` produced by `& Path` -> `&Path`.
    out = out.replace("& ", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uses(text: &str) -> Vec<UseDecl> {
        parse_file(text).unwrap().modules.remove(0).uses
    }

    fn leaf(path: &[&str], binds: Option<&str>) -> (Vec<String>, Option<String>) {
        (
            path.iter().map(|s| s.to_string()).collect(),
            binds.map(str::to_owned),
        )
    }

    #[test]
    fn use_trees_become_one_declaration_per_leaf() {
        let decls = uses(
            "pub use crate::a::{b::C, d as E, f::*, self};\nuse super::g::{self as h, _x as _};\n",
        );
        let leaves: Vec<_> = decls
            .iter()
            .map(|d| (d.path.clone(), d.binds.clone()))
            .collect();
        assert_eq!(
            leaves,
            vec![
                leaf(&["crate", "a", "b", "C"], Some("C")),
                leaf(&["crate", "a", "d"], Some("E")),
                leaf(&["crate", "a", "f"], None),
                leaf(&["crate", "a"], Some("a")),
                leaf(&["super", "g"], Some("h")),
                leaf(&["super", "g", "_x"], None),
            ]
        );
        assert!(decls[2].glob);
        assert!(decls[..4].iter().all(|d| d.reexport && d.line == 1));
        assert!(decls[4..].iter().all(|d| !d.reexport && d.line == 2));
    }

    #[test]
    fn raw_identifiers_and_pub_self_are_read_as_the_compiler_reads_them() {
        let file = parse_file("mod r#type;\npub(self) use crate::r#type::r#Kind;\n").unwrap();
        let root = &file.modules[0];
        assert_eq!(root.declared[0].name, "type");
        assert_eq!(root.uses[0].path, vec!["crate", "type", "Kind"]);
        // `pub(self)` is private
        assert!(!root.uses[0].reexport);
    }

    #[test]
    fn uses_in_function_bodies_are_local() {
        let text = "\
fn f() {
    use crate::a::B;
    let _ = || {
        use crate::c::D;
    };
}
impl S {
    fn g(&self) {
        use crate::e::F;
    }
}
trait T {
    fn h() {
        use crate::g::H;
    }
}
";
        let decls = uses(text);
        let found: Vec<(u32, Scope)> = decls.iter().map(|d| (d.line, d.scope)).collect();
        assert_eq!(
            found,
            vec![
                (2, Scope::Local),
                (4, Scope::Local),
                (9, Scope::Local),
                (14, Scope::Local)
            ]
        );
        assert!(decls.iter().all(|d| !d.reexport && !d.test));
    }

    #[test]
    fn test_code_is_marked() {
        let text = "\
#[cfg(test)]
use crate::a::A;
#[cfg(all(test, unix))]
fn helper() {
    use crate::b::B;
}
#[cfg(not(test))]
use crate::c::C;
#[cfg(any(test, feature = \"x\"))]
use crate::d::D;
#[cfg(test)]
mod tests {
    use super::*;
}
#[cfg(test)]
mod fixtures;
";
        let file = parse_file(text).unwrap();
        let root = &file.modules[0];
        let test: Vec<(&str, bool)> = root
            .uses
            .iter()
            .map(|d| (d.path[1].as_str(), d.test))
            .collect();
        // only what is compiled for tests alone
        assert_eq!(
            test,
            vec![("a", true), ("b", true), ("c", false), ("d", false)]
        );
        assert!(file.modules[root.inline["tests"]].test);
        assert!(root.declared[0].test);
    }

    #[test]
    fn modules_items_and_symbols_are_recorded_per_module() {
        let text = "\
pub mod a;
#[path = \"other.rs\"]
mod b;
pub mod c {
    pub fn visible() {}
    mod d {
        pub fn hidden() {}
        mod e;
    }
}
mod f {
    pub mod g {
        pub fn also_hidden() {}
    }
}
fn private() {}
pub struct S;
macro_rules! m { () => {} }
#[macro_export]
macro_rules! exported { () => {} }
#[cfg_attr(unix, path = \"unix.rs\")]
mod platform;
";
        let file = parse_file(text).unwrap();
        let root = &file.modules[0];
        let declared: Vec<(&str, u32, bool)> = root
            .declared
            .iter()
            .map(|d| (d.name.as_str(), d.line, d.path_attr))
            .collect();
        assert_eq!(
            declared,
            vec![("a", 1, false), ("b", 3, true), ("platform", 22, true)]
        );
        assert_eq!(
            root.items,
            ["S", "exported", "m", "private"].map(String::from).into()
        );
        assert_eq!(root.exported_macros, vec!["exported"]);
        let symbols: Vec<&str> = root.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(symbols, vec!["a", "c", "S"]);

        let c = &file.modules[root.inline["c"]];
        assert!(c.public);
        assert_eq!(c.symbols[0].name, "visible");
        let d = &file.modules[c.inline["d"]];
        assert!(!d.public && d.symbols.is_empty());
        assert_eq!(d.declared[0].name, "e");
        let g = &file.modules[file.modules[root.inline["f"]].inline["g"]];
        assert!(!g.public && g.symbols.is_empty());
    }

    #[test]
    fn render_produces_readable_signatures() {
        let f: syn::ItemFn = syn::parse_quote! {
            pub fn scan(root: &Path, options: &ScanOptions) -> Result<Vec<u8>, Error> { todo!() }
        };
        let (vis, fsig) = (&f.vis, &f.sig);
        let sig = render(quote::quote!(#vis #fsig));
        assert_eq!(
            sig,
            "pub fn scan(root: &Path, options: &ScanOptions) -> Result<Vec<u8>, Error>"
        );
    }
}

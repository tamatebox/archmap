//! Source pass: `pub` items and `use` statements from `src/**/*.rs`.
//!
//! This is a structural scan. Function bodies are parsed by `syn` but never
//! inspected; only item-level facts are recorded.

use std::path::Path;

use archmap_core::{Edge, EdgeKind, Evidence, Symbol, SymbolId, SymbolKind};
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::{Item, UseTree, Visibility};

use super::{owning_package, ResolvedPackage};
use crate::analyzer::AnalyzerOutput;
use crate::context::display_path;
use crate::RepoContext;

pub(super) fn source_pass(
    ctx: &RepoContext,
    packages: &[ResolvedPackage],
    output: &mut AnalyzerOutput,
) {
    for rel in ctx.files_with_extension("rs") {
        let Some(pkg) = owning_package(packages, rel) else {
            continue;
        };
        let Some(module_path) = module_path(&pkg.dir, rel) else {
            continue; // not under src/
        };

        let text = match ctx.read_to_string(rel) {
            Ok(text) => text,
            Err(err) => {
                output
                    .warnings
                    .push(format!("{}: {err}", display_path(rel)));
                continue;
            }
        };
        let file = match syn::parse_file(&text) {
            Ok(file) => file,
            Err(err) => {
                output
                    .warnings
                    .push(format!("{}: parse error: {err}", display_path(rel)));
                continue;
            }
        };

        let mut visitor = FileVisitor {
            pkg,
            file: display_path(rel),
            output,
        };
        visitor.visit_items(&file.items, &module_path);
    }
}

/// Module path for a file relative to its package: `src/lib.rs` -> `[]`,
/// `src/a/mod.rs` -> `[a]`, `src/a/b.rs` -> `[a, b]`. Files outside `src/`
/// yield `None`.
fn module_path(pkg_dir: &Path, file: &Path) -> Option<Vec<String>> {
    let rel = file.strip_prefix(pkg_dir).ok()?;
    let mut parts = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    if parts.next()? != "src" {
        return None;
    }
    let mut parts: Vec<String> = parts.collect();
    let last = parts.pop()?;
    let stem = last.strip_suffix(".rs")?;
    match stem {
        "lib" | "main" if parts.is_empty() => {}
        "mod" => {}
        other => parts.push(other.to_owned()),
    }
    Some(parts)
}

struct FileVisitor<'a> {
    pkg: &'a ResolvedPackage,
    file: String,
    output: &'a mut AnalyzerOutput,
}

impl FileVisitor<'_> {
    fn visit_items(&mut self, items: &[Item], module_path: &[String]) {
        for item in items {
            match item {
                Item::Use(item_use) => {
                    self.record_use(&item_use.tree, item_use.span().start().line)
                }
                Item::ExternCrate(ext) => {
                    let name = ext.ident.to_string();
                    self.record_import(&name, ext.span().start().line, "extern crate");
                }
                Item::Fn(f) if is_pub(&f.vis) => {
                    let (vis, sig) = (&f.vis, &f.sig);
                    let sig = render(quote::quote!(#vis #sig));
                    self.record_symbol(
                        module_path,
                        &f.sig.ident.to_string(),
                        SymbolKind::Function,
                        Some(sig),
                        f.sig.ident.span(),
                    );
                }
                Item::Struct(s) if is_pub(&s.vis) => {
                    let (ident, g) = (&s.ident, &s.generics);
                    self.record_symbol(
                        module_path,
                        &s.ident.to_string(),
                        SymbolKind::Struct,
                        Some(render(quote::quote!(pub struct #ident #g))),
                        s.ident.span(),
                    );
                }
                Item::Enum(e) if is_pub(&e.vis) => {
                    let (ident, g) = (&e.ident, &e.generics);
                    self.record_symbol(
                        module_path,
                        &e.ident.to_string(),
                        SymbolKind::Enum,
                        Some(render(quote::quote!(pub enum #ident #g))),
                        e.ident.span(),
                    );
                }
                Item::Trait(t) if is_pub(&t.vis) => {
                    let (ident, g) = (&t.ident, &t.generics);
                    self.record_symbol(
                        module_path,
                        &t.ident.to_string(),
                        SymbolKind::Trait,
                        Some(render(quote::quote!(pub trait #ident #g))),
                        t.ident.span(),
                    );
                }
                Item::Type(t) if is_pub(&t.vis) => {
                    let (ident, g) = (&t.ident, &t.generics);
                    self.record_symbol(
                        module_path,
                        &t.ident.to_string(),
                        SymbolKind::TypeAlias,
                        Some(render(quote::quote!(pub type #ident #g))),
                        t.ident.span(),
                    );
                }
                Item::Const(c) if is_pub(&c.vis) => {
                    let (ident, ty) = (&c.ident, &c.ty);
                    self.record_symbol(
                        module_path,
                        &c.ident.to_string(),
                        SymbolKind::Constant,
                        Some(render(quote::quote!(pub const #ident: #ty))),
                        c.ident.span(),
                    );
                }
                Item::Static(s) if is_pub(&s.vis) => {
                    let (ident, ty) = (&s.ident, &s.ty);
                    self.record_symbol(
                        module_path,
                        &s.ident.to_string(),
                        SymbolKind::Constant,
                        Some(render(quote::quote!(pub static #ident: #ty))),
                        s.ident.span(),
                    );
                }
                Item::Mod(m) if is_pub(&m.vis) => {
                    let name = m.ident.to_string();
                    self.record_symbol(
                        module_path,
                        &name,
                        SymbolKind::Module,
                        None,
                        m.ident.span(),
                    );
                    if let Some((_, items)) = &m.content {
                        let mut nested = module_path.to_vec();
                        nested.push(name);
                        self.visit_items(items, &nested);
                    }
                }
                Item::Mod(m) => {
                    // Private inline module: its `pub` items are not part of
                    // the package interface, but its `use` statements are
                    // still dependency facts.
                    if let Some((_, items)) = &m.content {
                        self.visit_uses_only(items);
                    }
                }
                Item::Impl(imp) if imp.trait_.is_none() => {
                    let self_ty = render(imp.self_ty.to_token_stream());
                    for impl_item in &imp.items {
                        if let syn::ImplItem::Fn(method) = impl_item {
                            if is_pub(&method.vis) {
                                let (vis, msig) = (&method.vis, &method.sig);
                                let sig = render(quote::quote!(#vis #msig));
                                let name = format!("{self_ty}::{}", method.sig.ident);
                                self.record_symbol(
                                    module_path,
                                    &name,
                                    SymbolKind::Function,
                                    Some(sig),
                                    method.sig.ident.span(),
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn visit_uses_only(&mut self, items: &[Item]) {
        for item in items {
            match item {
                Item::Use(item_use) => {
                    self.record_use(&item_use.tree, item_use.span().start().line)
                }
                Item::Mod(m) => {
                    if let Some((_, items)) = &m.content {
                        self.visit_uses_only(items);
                    }
                }
                _ => {}
            }
        }
    }

    fn record_use(&mut self, tree: &UseTree, line: usize) {
        match tree {
            UseTree::Path(p) => self.record_import(&p.ident.to_string(), line, "use"),
            UseTree::Name(n) => self.record_import(&n.ident.to_string(), line, "use"),
            UseTree::Rename(r) => self.record_import(&r.ident.to_string(), line, "use"),
            UseTree::Group(g) => {
                for t in &g.items {
                    self.record_use(t, line);
                }
            }
            UseTree::Glob(_) => {}
        }
    }

    fn record_import(&mut self, first_segment: &str, line: usize, note: &str) {
        if matches!(
            first_segment,
            "crate" | "self" | "super" | "std" | "core" | "alloc"
        ) {
            return;
        }
        let Some(target) = self.pkg.import_targets.get(first_segment) else {
            return; // local module or unknown crate
        };
        if *target == self.pkg.id {
            return;
        }
        self.output.fragment.push_edge(
            Edge::new(self.pkg.id.clone(), target.clone(), EdgeKind::Import).with_evidence(
                Evidence::new(&self.file)
                    .at_line(line as u32)
                    .with_note(note),
            ),
        );
    }

    fn record_symbol(
        &mut self,
        module_path: &[String],
        name: &str,
        kind: SymbolKind,
        signature: Option<String>,
        span: proc_macro2::Span,
    ) {
        let mut id = self.pkg.name.clone();
        for m in module_path {
            id.push_str("::");
            id.push_str(m);
        }
        id.push_str("::");
        id.push_str(name);

        self.output.fragment.push_symbol(Symbol {
            id: SymbolId::new(id),
            name: name.to_owned(),
            kind,
            component: self.pkg.id.clone(),
            signature,
            evidence: vec![Evidence::new(&self.file).at_line(span.start().line as u32)],
        });
    }
}

fn is_pub(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
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

    #[test]
    fn module_path_follows_rust_conventions() {
        let dir = Path::new("crates/a");
        let mp = |f: &str| module_path(dir, Path::new(f));
        assert_eq!(mp("crates/a/src/lib.rs"), Some(vec![]));
        assert_eq!(mp("crates/a/src/main.rs"), Some(vec![]));
        assert_eq!(mp("crates/a/src/x.rs"), Some(vec!["x".into()]));
        assert_eq!(mp("crates/a/src/x/mod.rs"), Some(vec!["x".into()]));
        assert_eq!(
            mp("crates/a/src/x/y.rs"),
            Some(vec!["x".into(), "y".into()])
        );
        assert_eq!(
            mp("crates/a/src/bin/tool.rs"),
            Some(vec!["bin".into(), "tool".into()])
        );
        assert_eq!(mp("crates/a/tests/it.rs"), None);
        assert_eq!(mp("crates/a/build.rs"), None);
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

//! Per-file facts from `syn`: `mod` declarations, the names each module
//! defines, `use` declarations, module paths written in code, and `pub`
//! symbols. Nothing here knows where a file sits in its crate;
//! [`super::tree`] places files in module trees.
//!
//! This is a structural scan. Function bodies are parsed by `syn` but only
//! visited for `use` declarations and module paths, both recorded with local
//! scope, and, for a module with a glob `use`, for the names the glob may
//! bring in, apart from those a binding hides; which item a path names,
//! calls and data flow are not recorded.

use std::collections::{BTreeMap, BTreeSet};

use archmap_core::{Scope, SymbolKind};
use quote::ToTokens;
use syn::ext::IdentExt;
use syn::punctuated::Punctuated;
use syn::visit::Visit;
use syn::{
    Attribute, Block, Ident, ImplItem, Item, ItemUse, Meta, Path, Token, TraitItem, UseTree,
    Visibility,
};

/// Macros whose arguments are no code of the calling crate, however much
/// they look like expressions: tokens to print (`stringify!`) or to emit
/// into another crate (`quote!`). They are never read, only recorded.
pub(super) const NOT_CODE: &[&str] = &[
    "stringify",
    "concat_idents",
    "quote",
    "quote_spanned",
    "parse_quote",
    "parse_quote_spanned",
];

/// Primitive types, whose associated items (`u32::MAX`) are no module paths.
const PRIMITIVES: &[&str] = &[
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64",
    "i128", "isize", "f32", "f64",
];

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
    /// An inline module visible outside the module that declares it (`pub`,
    /// `pub(crate)`, ..), so that paths from elsewhere can name it.
    pub visible: bool,
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
    /// Paths in code that may name a module, outside `use` declarations,
    /// those in the arguments of macro calls included.
    pub paths: Vec<PathRef>,
    /// The other names code writes first in a path, which a glob `use` of
    /// the module may bring in: kept only when the module has one.
    pub names: BTreeSet<NameRef>,
    /// Macro calls whose arguments are neither expressions nor items, so
    /// the paths in them are not read.
    pub unread_macros: Vec<MacroCall>,
    pub symbols: Vec<SymbolDecl>,
}

/// A macro call whose arguments are not read (`json!({ .. })`, a DSL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MacroCall {
    /// The macro's name, as written last in its path (`json`).
    pub name: String,
    /// The names in the `a::b` paths its arguments write, which a target
    /// of that name may be.
    pub names: BTreeSet<String>,
    pub line: u32,
    pub scope: Scope,
    /// In `#[cfg(test)]` or `#[test]` code.
    pub test: bool,
}

/// Visit the arguments of a macro call as comma-separated expressions, as
/// an expression and a pattern (`matches!(x, Some(_))`), as the elements of
/// an array (`vec![x; n]`), or as items; `false` when they are none of
/// these.
pub(super) fn visit_arguments<V>(tokens: &proc_macro2::TokenStream, paths: &mut V) -> bool
where
    V: for<'ast> Visit<'ast>,
{
    use syn::parse::{ParseStream, Parser};
    if let Ok(list) = Punctuated::<syn::Expr, Token![,]>::parse_terminated.parse2(tokens.clone()) {
        list.iter().for_each(|e| paths.visit_expr(e));
        return true;
    }
    let matched = |input: ParseStream| {
        let expr: syn::Expr = input.parse()?;
        input.parse::<Token![,]>()?;
        let pattern = syn::Pat::parse_multi_with_leading_vert(input)?;
        let guard = match input.parse::<Option<Token![if]>>()? {
            Some(_) => Some(input.parse::<syn::Expr>()?),
            None => None,
        };
        input.parse::<Option<Token![,]>>()?;
        Ok((expr, pattern, guard))
    };
    if let Ok((expr, pattern, guard)) = matched.parse2(tokens.clone()) {
        paths.visit_expr(&expr);
        paths.visit_pat(&pattern);
        guard.iter().for_each(|g| paths.visit_expr(g));
        return true;
    }
    if let Ok(array) = syn::parse2::<syn::Expr>(quote::quote!([#tokens])) {
        paths.visit_expr(&array);
        return true;
    }
    if let Ok(file) = syn::parse2::<syn::File>(tokens.clone()) {
        file.items.iter().for_each(|i| paths.visit_item(i));
        return true;
    }
    false
}

/// The names in the `a::b` paths of `tokens`, groups included.
fn path_names(tokens: proc_macro2::TokenStream, names: &mut BTreeSet<String>) {
    use proc_macro2::{Spacing, TokenTree};
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let colons = |i: usize| {
        matches!((trees.get(i), trees.get(i + 1)),
            (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b)))
                if a.as_char() == ':' && a.spacing() == Spacing::Joint && b.as_char() == ':')
    };
    for (i, tree) in trees.iter().enumerate() {
        match tree {
            TokenTree::Group(group) => path_names(group.stream(), names),
            // a name before or after `::`
            TokenTree::Ident(ident) if colons(i + 1) || (i >= 2 && colons(i - 2)) => {
                names.insert(ident.to_string());
            }
            _ => {}
        }
    }
}

/// A path written in code that may name a module: two or more segments,
/// starting with `crate`, `self`, `super`, `::` or a lowercase name
/// (`crate::graph::build(..)`, `child::run()`, `serde_json::to_string`).
/// Paths starting with a type (`Self::`, `String::new`) are left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PathRef {
    pub segments: Vec<String>,
    pub leading_colon: bool,
    /// Written in an expression (`crate::parse(..)`), whose last name is a
    /// value, which a module of the same name does not hide; elsewhere it is
    /// a type or a module.
    pub value: bool,
    pub line: u32,
    /// `Local` inside a function body, `Module` elsewhere (signatures, types).
    pub scope: Scope,
    /// In `#[cfg(test)]` or `#[test]` code.
    pub test: bool,
}

/// A name code writes alone (`pay(1)`, `Receipt`) or first in a path that
/// names no module (`Receipt::new`), where no binding of a parameter or a
/// pattern and no item of a block of that name hides it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct NameRef {
    pub name: String,
    /// Written alone in an expression or a pattern: a value.
    pub value: bool,
    /// The name of a macro called (`settle!(..)`), which only a macro
    /// answers, never a module or a function of that name.
    pub macro_call: bool,
    /// In `#[cfg(test)]` or `#[test]` code.
    pub test: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModDecl {
    pub name: String,
    pub line: u32,
    /// Visible outside the declaring module (`pub`, `pub(crate)`, ..).
    pub visible: bool,
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
    /// For a method: the self type of its inherent `impl`.
    pub owner: Option<SelfType>,
    /// Compiled only for tests by an attribute of its own (`#[cfg(test)]` on
    /// the item, or on a method or its `impl`); a test module marks what it
    /// holds through the module.
    pub test: bool,
}

/// The self type of an inherent `impl` as written, without generics:
/// `Wrapper` for `impl<T> Wrapper<T>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SelfType {
    pub segments: Vec<String>,
    pub leading_colon: bool,
    /// The line of the `impl`.
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
        if !matches!(item, Item::Use(_) | Item::ExternCrate(_) | Item::Mod(_)) {
            let facts = &mut file.modules[module];
            Paths {
                out: &mut facts.paths,
                unread: &mut facts.unread_macros,
                names: &mut facts.names,
                scope: Scope::Module,
                test,
                value: false,
                frames: Vec::new(),
                binding: None,
            }
            .visit_item(item);
        }
        let mut facts = Facts {
            module: &mut file.modules[module],
            public,
            test,
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
                        visible: visible_outside(&m.vis),
                        path_attr: m.attrs.iter().any(is_path_attr),
                        test,
                    }),
                    Some((_, items)) => {
                        let index = file.modules.len();
                        file.modules[module].inline.insert(mod_name, index);
                        file.modules.push(ModuleFacts {
                            public: public && is_pub(&m.vis),
                            visible: visible_outside(&m.vis),
                            test,
                            ..ModuleFacts::default()
                        });
                        collect(items, index, file);
                    }
                }
            }
            Item::Impl(imp) => {
                let self_ty = render(imp.self_ty.to_token_stream());
                let owner = self_type(&imp.self_ty, line_of(imp.impl_token.span));
                for impl_item in &imp.items {
                    let ImplItem::Fn(method) = impl_item else {
                        continue;
                    };
                    facts.test = test || cfg_test(&method.attrs);
                    if public && imp.trait_.is_none() && is_pub(&method.vis) {
                        let (vis, msig) = (&method.vis, &method.sig);
                        facts.symbol(
                            &format!("{self_ty}::{}", method.sig.ident),
                            SymbolKind::Function,
                            Some(render(quote::quote!(#vis #msig))),
                            line_of(method.sig.ident.span()),
                        );
                        if let Some(symbol) = facts.module.symbols.last_mut() {
                            symbol.owner = owner.clone();
                        }
                    }
                    local_uses(&method.block, facts.module, test || cfg_test(&method.attrs));
                }
            }
            _ => {}
        }
    }
    // only a glob brings names in
    let facts = &mut file.modules[module];
    if !facts
        .uses
        .iter()
        .any(|u| u.glob && u.scope == Scope::Module)
    {
        facts.names.clear();
    }
}

/// The module being collected, whether its `pub` items are symbols, and
/// whether the item at hand is compiled only for tests.
struct Facts<'a> {
    module: &'a mut ModuleFacts,
    public: bool,
    test: bool,
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
            owner: None,
            test: self.test,
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

/// Module paths in an item: in signatures and types at module scope, in
/// function bodies at local scope, in the arguments of macro calls that are
/// code. `use` declarations, inline modules, visibility restrictions and
/// attributes other than `#[derive(..)]` are left out; a macro call whose
/// arguments are no code is recorded as not read.
struct Paths<'a> {
    out: &'a mut Vec<PathRef>,
    unread: &'a mut Vec<MacroCall>,
    names: &'a mut BTreeSet<NameRef>,
    scope: Scope,
    test: bool,
    /// The next path is an expression's, whose last name is a value.
    value: bool,
    /// What the scopes around the walk bind, innermost last.
    frames: Vec<Bound>,
    /// The names the pattern being read binds, while one is read.
    binding: Option<BTreeSet<String>>,
}

/// What one scope of code binds: the bindings of parameters and patterns,
/// which hide a name written alone in an expression, and a block's items,
/// which hide any name.
struct Bound {
    locals: BTreeSet<String>,
    items: BTreeSet<String>,
}

impl Paths<'_> {
    /// Read a pattern: the paths it names, and the names it binds.
    fn pattern(&mut self, pat: &syn::Pat) -> BTreeSet<String> {
        let outer = self.binding.replace(BTreeSet::new());
        self.visit_pat(pat);
        std::mem::replace(&mut self.binding, outer).unwrap_or_default()
    }

    /// Walk in a scope that binds `locals`.
    fn bound(&mut self, locals: BTreeSet<String>, walk: impl FnOnce(&mut Self)) {
        self.frames.push(Bound {
            locals,
            items: BTreeSet::new(),
        });
        walk(self);
        self.frames.pop();
    }

    /// A function: its parameters are bound in its body.
    fn function(&mut self, sig: &syn::Signature, block: Option<&Block>) {
        let outer = self.binding.replace(BTreeSet::new());
        self.visit_signature(sig);
        let params = std::mem::replace(&mut self.binding, outer).unwrap_or_default();
        if let Some(block) = block {
            self.bound(params, |p| p.visit_block(block));
        }
    }

    /// Keep the first name of `path` among the names a glob may bring in,
    /// unless a scope around hides it.
    fn name(&mut self, path: &Path, value: bool) {
        let Some(first) = path.segments.first() else {
            return;
        };
        let first = name(&first.ident);
        let alone = value && path.segments.len() == 1;
        let keyword = matches!(first.as_str(), "crate" | "self" | "super" | "Self");
        let hidden = self
            .frames
            .iter()
            .any(|f| f.items.contains(&first) || (alone && f.locals.contains(&first)));
        if keyword || hidden || PRIMITIVES.contains(&first.as_str()) {
            return;
        }
        self.names.insert(NameRef {
            name: first,
            value: alone,
            macro_call: false,
            test: self.test,
        });
    }

    /// Keep the name of a macro called by its name alone among the names a
    /// glob may bring in, unless an item of a block around hides it.
    fn macro_name(&mut self, called: &syn::Ident) {
        let called = name(called);
        if self.frames.iter().any(|f| f.items.contains(&called)) {
            return;
        }
        self.names.insert(NameRef {
            name: called,
            value: false,
            macro_call: true,
            test: self.test,
        });
    }
}

impl<'ast> Visit<'ast> for Paths<'_> {
    /// A macro call's arguments, which `syn` keeps as tokens, read as the
    /// expressions or items most macros take (`vec![..]`, `write!(..)`,
    /// `assert_eq!(..)`, `thread_local! { .. }`); any other form is recorded
    /// as not read.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        match mac.path.get_ident() {
            Some(called) => self.macro_name(called),
            None => syn::visit::visit_macro(self, mac),
        }
        let last = mac.path.segments.last();
        let code = last.is_none_or(|s| !NOT_CODE.contains(&s.ident.to_string().as_str()));
        if !(code && visit_arguments(&mac.tokens, self)) {
            let mut names = BTreeSet::new();
            path_names(mac.tokens.clone(), &mut names);
            self.unread.push(MacroCall {
                name: last.map(|s| s.ident.to_string()).unwrap_or_default(),
                names,
                line: last.map_or(0, |s| line_of(s.ident.span())),
                scope: self.scope,
                test: self.test,
            });
        }
    }

    /// A `macro_rules!` definition: its body is patterns, not code.
    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if item.ident.is_none() {
            syn::visit::visit_item_macro(self, item);
        }
    }

    /// An expression's path (a path pattern's too) names a value last,
    /// unless a qualified self type (`<T as Trait>::m`) comes first.
    fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
        let outer = std::mem::replace(&mut self.value, p.qself.is_none());
        syn::visit::visit_expr_path(self, p);
        self.value = outer;
    }

    fn visit_path(&mut self, path: &'ast Path) {
        // the paths in its generic arguments are types
        let value = std::mem::take(&mut self.value);
        if let Some(segments) = module_path(path) {
            self.out.push(PathRef {
                segments,
                leading_colon: path.leading_colon.is_some(),
                value,
                line: line_of(path.segments[0].ident.span()),
                scope: self.scope,
                test: self.test,
            });
        } else if path.leading_colon.is_none() {
            self.name(path, value);
        }
        // generic arguments hold paths of their own
        syn::visit::visit_path(self, path);
    }

    fn visit_block(&mut self, block: &'ast Block) {
        let outer = std::mem::replace(&mut self.scope, Scope::Local);
        let items = block
            .stmts
            .iter()
            .filter_map(|s| match s {
                syn::Stmt::Item(item) => item_name(item),
                _ => None,
            })
            .collect();
        self.frames.push(Bound {
            locals: BTreeSet::new(),
            items,
        });
        syn::visit::visit_block(self, block);
        self.frames.pop();
        self.scope = outer;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        let outer = self.test;
        self.test |= cfg_test(&f.attrs);
        f.attrs.iter().for_each(|a| self.visit_attribute(a));
        self.function(&f.sig, Some(&f.block));
        self.test = outer;
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let outer = self.test;
        self.test |= cfg_test(&f.attrs);
        f.attrs.iter().for_each(|a| self.visit_attribute(a));
        self.function(&f.sig, Some(&f.block));
        self.test = outer;
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        f.attrs.iter().for_each(|a| self.visit_attribute(a));
        self.function(&f.sig, f.default.as_ref());
    }

    // what a binding hides: after a `let`, in a closure's body, a match
    // arm, a `for` body, and the rest of the condition and the branch of an
    // `if let` and a `while let`
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let Some(init) = &local.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        let names = self.pattern(&local.pat);
        if let Some(frame) = self.frames.last_mut() {
            frame.locals.extend(names);
        }
    }

    fn visit_expr_closure(&mut self, c: &'ast syn::ExprClosure) {
        let mut params = BTreeSet::new();
        for input in &c.inputs {
            params.extend(self.pattern(input));
        }
        self.visit_return_type(&c.output);
        self.bound(params, |p| p.visit_expr(&c.body));
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        let names = self.pattern(&arm.pat);
        self.bound(names, |p| {
            if let Some((_, guard)) = &arm.guard {
                p.visit_expr(guard);
            }
            p.visit_expr(&arm.body);
        });
    }

    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        self.visit_expr(&f.expr);
        let names = self.pattern(&f.pat);
        self.bound(names, |p| p.visit_block(&f.body));
    }

    fn visit_expr_if(&mut self, e: &'ast syn::ExprIf) {
        self.bound(BTreeSet::new(), |p| {
            p.visit_expr(&e.cond);
            p.visit_block(&e.then_branch);
        });
        if let Some((_, other)) = &e.else_branch {
            self.visit_expr(other);
        }
    }

    fn visit_expr_while(&mut self, e: &'ast syn::ExprWhile) {
        self.bound(BTreeSet::new(), |p| {
            p.visit_expr(&e.cond);
            p.visit_block(&e.body);
        });
    }

    fn visit_expr_let(&mut self, l: &'ast syn::ExprLet) {
        self.visit_expr(&l.expr);
        let names = self.pattern(&l.pat);
        if let Some(frame) = self.frames.last_mut() {
            frame.locals.extend(names);
        }
    }

    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        if let Some(names) = &mut self.binding {
            names.insert(name(&p.ident));
        }
        syn::visit::visit_pat_ident(self, p);
    }

    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        if !attr.path().is_ident("derive") {
            return;
        }
        let Ok(paths) = attr.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)
        else {
            return;
        };
        for path in &paths {
            self.visit_path(path);
        }
    }

    fn visit_item_use(&mut self, _: &'ast ItemUse) {}
    fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {}
    fn visit_vis_restricted(&mut self, _: &'ast syn::VisRestricted) {}
}

/// The name an item of a block binds, which hides a name a glob brings in.
fn item_name(item: &Item) -> Option<String> {
    let ident = match item {
        Item::Const(i) => &i.ident,
        Item::Enum(i) => &i.ident,
        Item::Fn(i) => &i.sig.ident,
        Item::Mod(i) => &i.ident,
        Item::Static(i) => &i.ident,
        Item::Struct(i) => &i.ident,
        Item::Trait(i) => &i.ident,
        Item::Type(i) => &i.ident,
        Item::Union(i) => &i.ident,
        Item::Macro(i) => i.ident.as_ref()?,
        _ => return None,
    };
    Some(name(ident))
}

/// The segments of `path` when it may name a module: see [`PathRef`].
fn module_path(path: &Path) -> Option<Vec<String>> {
    if path.segments.len() < 2 {
        return None;
    }
    let first = name(&path.segments[0].ident);
    let lowercase = first.starts_with(|c: char| c.is_lowercase() || c == '_');
    if path.leading_colon.is_none() && (!lowercase || PRIMITIVES.contains(&first.as_str())) {
        return None;
    }
    Some(path.segments.iter().map(|s| name(&s.ident)).collect())
}

pub(super) fn use_decls(u: &ItemUse, line: u32, scope: Scope, test: bool) -> Vec<UseDecl> {
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
pub(super) fn name(ident: &Ident) -> String {
    ident.unraw().to_string()
}

/// The path of an `impl`'s self type, for a plain path type.
fn self_type(ty: &syn::Type, line: u32) -> Option<SelfType> {
    let syn::Type::Path(p) = ty else {
        return None;
    };
    if p.qself.is_some() {
        return None;
    }
    Some(SelfType {
        segments: p.path.segments.iter().map(|s| name(&s.ident)).collect(),
        leading_colon: p.path.leading_colon.is_some(),
        line,
    })
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

/// Compiled only for tests: `#[test]`, `#[cfg(test)]`, or `test` inside
/// `all(..)`.
pub(super) fn cfg_test(attrs: &[Attribute]) -> bool {
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
        a.path().is_ident("test")
            || (a.path().is_ident("cfg") && a.parse_args::<Meta>().is_ok_and(|m| requires_test(&m)))
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
    fn methods_keep_the_path_of_their_type() {
        let file = parse_file(
            "impl<T> Wrapper<T> {\n    pub fn new() {}\n}\nimpl crate::model::Edge {\n    pub fn kind() {}\n}\n",
        )
        .unwrap();
        let owners: Vec<(Vec<&str>, bool, u32)> = file.modules[0]
            .symbols
            .iter()
            .filter_map(|s| s.owner.as_ref())
            .map(|o| {
                (
                    o.segments.iter().map(String::as_str).collect(),
                    o.leading_colon,
                    o.line,
                )
            })
            .collect();
        assert_eq!(
            owners,
            [
                (vec!["Wrapper"], false, 1),
                (vec!["crate", "model", "Edge"], false, 4)
            ]
        );
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
    fn macro_arguments_are_read_as_code_when_they_are() {
        let text = "\
macro_rules! ignored {
    ($x:expr) => { crate::not::read };
}
fn calls(out: &mut String) {
    let all = vec![Box::new(rust::Analyzer)];
    write!(out, \"{}\", crate::text::shell(1));
    let ok = matches!(kind, model::Kind::A | model::Kind::B);
    let many = vec![shape::unit(); 3];
    json!({ \"a\": config::value() });
    let name = stringify!(crate::printed::only);
}
thread_local! {
    static CELL: std::cell::Cell<u32> = std::cell::Cell::new(cache::start());
}
";
        let file = parse_file(text).unwrap();
        let root = &file.modules[0];
        let paths: BTreeSet<String> = root.paths.iter().map(|p| p.segments.join("::")).collect();
        for read in [
            "rust::Analyzer",
            "crate::text::shell",
            "model::Kind::A",
            "model::Kind::B",
            "shape::unit",
            "cache::start",
        ] {
            assert!(paths.contains(read), "{read} in {paths:?}");
        }
        // a definition's body is patterns; a DSL's arguments are not read
        assert!(!paths
            .iter()
            .any(|p| p.contains("not::read") || p.contains("config") || p.contains("printed")));
        let unread: Vec<(&str, Vec<&str>)> = root
            .unread_macros
            .iter()
            .map(|m| {
                (
                    m.name.as_str(),
                    m.names.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        // tokens to print are no code, though they read as an expression
        assert_eq!(
            unread,
            [
                ("json", vec!["config", "value"]),
                ("stringify", vec!["crate", "only", "printed"])
            ]
        );
    }

    #[test]
    fn test_symbols_are_marked() {
        let text = "\
#[cfg(test)]
pub fn fixture() {}
pub fn production() {}
pub struct Wallet;
impl Wallet {
    #[cfg(test)]
    pub fn sample() -> Self {
        Wallet
    }
    pub fn open(&self) {}
}
#[cfg(test)]
impl Wallet {
    pub fn empty() -> Self {
        Wallet
    }
}
#[cfg(any(test, feature = \"x\"))]
pub fn maybe() {}
#[cfg(test)]
pub mod support {
    pub fn helper() {}
}
";
        let file = parse_file(text).unwrap();
        let marks = |m: usize| -> Vec<(&str, bool)> {
            file.modules[m]
                .symbols
                .iter()
                .map(|s| (s.name.as_str(), s.test))
                .collect()
        };
        assert_eq!(
            marks(0),
            vec![
                ("fixture", true),
                ("production", false),
                ("Wallet", false),
                ("Wallet::sample", true),
                ("Wallet::open", false),
                // the whole `impl` only for tests
                ("Wallet::empty", true),
                // compiled outside tests too
                ("maybe", false),
                ("support", true),
            ]
        );
        // what a test module holds is test code through the module
        assert_eq!(marks(1), vec![("helper", false)]);
        assert!(file.modules[1].test);
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
#[test]
fn case() {
    use crate::e::E;
    crate::f::g();
}
fn production() {
    crate::h::i();
}
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
            vec![
                ("a", true),
                ("b", true),
                ("c", false),
                ("d", false),
                ("e", true)
            ]
        );
        let paths: Vec<(&str, bool)> = root
            .paths
            .iter()
            .map(|p| (p.segments[1].as_str(), p.test))
            .collect();
        assert_eq!(paths, vec![("f", true), ("h", false)]);
        assert!(file.modules[root.inline["tests"]].test);
        assert!(root.declared[0].test);
    }

    #[test]
    fn module_paths_in_code_are_recorded_with_their_scope() {
        let text = "\
#[derive(Debug, serde::Serialize)]
pub struct S {
    field: crate::a::A,
    other: Vec<super::b::B>,
}
pub fn f(x: &dyn crate::c::C) -> self::d::D {
    crate::e::run();
    let _ = String::new();
    let _ = u32::MAX;
    let _ = Option::<u8>::None;
    crate::m::shout!();
    format!(\"{}\", crate::hidden::X);
    child::go(::other::f());
}
pub(in crate::vis) fn g() {}
use crate::u::U;
mod inline {
    fn h() { crate::i::j(); }
}
";
        let file = parse_file(text).unwrap();
        let found: Vec<(String, u32, Scope)> = file.modules[0]
            .paths
            .iter()
            .map(|p| (p.segments.join("::"), p.line, p.scope))
            .collect();
        let row = |path: &str, line, scope| (path.to_owned(), line, scope);
        assert_eq!(
            found,
            vec![
                row("serde::Serialize", 1, Scope::Module),
                row("crate::a::A", 3, Scope::Module),
                row("super::b::B", 4, Scope::Module),
                row("crate::c::C", 6, Scope::Module),
                row("self::d::D", 6, Scope::Module),
                row("crate::e::run", 7, Scope::Local),
                row("crate::m::shout", 11, Scope::Local),
                // inside `format!(..)`, whose arguments are expressions
                row("crate::hidden::X", 12, Scope::Local),
                row("child::go", 13, Scope::Local),
                row("other::f", 13, Scope::Local),
            ]
        );
        assert!(file.modules[0].paths[9].leading_colon);
        // an inline module keeps its own paths
        let inline = &file.modules[file.modules[0].inline["inline"]];
        assert_eq!(inline.paths[0].segments, vec!["crate", "i", "j"]);
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
    fn a_module_with_a_glob_keeps_the_names_its_code_writes_that_nothing_binds() {
        let file = parse_file(
            "\
use shop::*;
pub fn f(given: u32) -> Receipt {
    let paid = pay(given);
    let refund = 1;
    let _ = refund;
    let total = |fee: u32| fee + paid;
    match total(1) {
        bill => {
            let _ = bill;
        }
    }
    {
        fn local() {}
        local();
    }
    Receipt::new(paid)
}
",
        )
        .unwrap();
        let names: Vec<(&str, bool)> = file.modules[0]
            .names
            .iter()
            .map(|n| (n.name.as_str(), n.value))
            .collect();
        // a parameter, a `let`, a closure's parameter, a match arm's binding
        // and a block's item hide a name; a type and a path's first name
        // are no value
        assert_eq!(names, [("Receipt", false), ("pay", true)]);
        // without a glob there is nothing to bring the names in
        let file = parse_file("pub fn f() -> u32 {\n    pay(1)\n}\n").unwrap();
        assert!(file.modules[0].names.is_empty());
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

//! Import and export elision, at the end of a module.
//!
//! tsc drops an import binding the module only uses as a type, and the whole
//! import when that leaves it binding nothing; it drops `export { T }` of a
//! name that is only a type. Both matter at run time, not just for tidiness:
//! `import { Props } from "./types"` would ask `./types` for an export that
//! does not exist once its `interface Props` is erased, and linking fails.
//!
//! A type never reaches the syntax tree, so "used as a value" is "referred to
//! by the tree". The parser counts references to import bindings as it reads
//! them (`Parser::ts_note_ref`), which settles every module whose imports
//! come before the code that uses them; one with an import after other
//! statements has its references counted from the finished tree instead.

use super::name_key;
use crate::parse::ast::*;
use crate::parse::parser::Parser;
use crate::parse::visit::{self, Node};

impl<'s> Parser<'s> {
    /// Elide `body`'s type-only imports and exports. Runs before the module's
    /// `export { x }` names are checked against its bindings, so those of
    /// type-only names are taken out of that check first.
    pub(crate) fn ts_finish_program(&mut self, body: &mut [Stmt]) {
        if self.goal != Goal::Module {
            return;
        }
        // `export { T }` where `T` is only a type here.
        if !self.ts_state.type_names.is_empty() {
            let types = std::mem::take(&mut self.ts_state.type_names);
            let type_only = |p: &Self, n: &Name| types.contains(n) && !p.scopes.module_binds(n);
            let pending = std::mem::take(&mut self.pending_export_locals);
            self.pending_export_locals = pending
                .into_iter()
                .filter(|(n, _)| !type_only(self, n))
                .collect();
            for s in body.iter_mut() {
                let Stmt::Export(e) = s else { continue };
                let ExportDecl::Named {
                    specifiers,
                    source: None,
                    ..
                } = &mut **e
                else {
                    continue;
                };
                if specifiers.is_empty() {
                    continue;
                }
                specifiers.retain(
                    |sp| !matches!(&sp.local, ModuleExportName::Ident(n) if type_only(self, n)),
                );
                if specifiers.is_empty() {
                    *s = Stmt::Empty;
                }
            }
        }
        if self.ts_state.imports.is_empty() {
            return;
        }
        let mut used = std::mem::take(&mut self.ts_state.imports);
        if self.ts_state.late_import {
            count_references(body, &mut used);
        }
        for s in body.iter_mut() {
            let Stmt::Import(d) = s else { continue };
            if d.specifiers.is_empty() {
                continue;
            }
            d.specifiers.retain(|sp| {
                let local = match sp {
                    ImportSpecifier::Default(n) | ImportSpecifier::Namespace(n) => n,
                    ImportSpecifier::Named { local, .. } => local,
                };
                used.get(&name_key(local)).copied().unwrap_or(true)
            });
            if d.specifiers.is_empty() {
                *s = Stmt::Empty;
            }
        }
    }
}

/// Mark each import binding of `used` the tree refers to.
fn count_references(body: &[Stmt], used: &mut rustc_hash::FxHashMap<usize, bool>) {
    let mut mark = |n: &Name| {
        if let Some(u) = used.get_mut(&name_key(n)) {
            *u = true;
        }
    };
    for s in body {
        if let Stmt::Export(e) = s {
            if let ExportDecl::Named {
                specifiers,
                source: None,
                ..
            } = &**e
            {
                for sp in specifiers {
                    if let ModuleExportName::Ident(n) = &sp.local {
                        mark(n);
                    }
                }
            }
        }
    }
    visit::stmts(body, &mut |node| {
        match node {
            Node::Expr(Expr::Ident(n)) => mark(n),
            Node::Target(Target::Ident { name, .. }) => mark(name),
            _ => {}
        }
        visit::Flow::Go
    });
}

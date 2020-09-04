use crate::{error::TypescriptError, wasm::WasmBindgenImport};
use std::{
    collections::BTreeMap,
    fmt::{Debug, Formatter, Result as FmtResult},
};
use swc_ecma_ast::{
    ClassDecl, Decl, ExportAll, ExportDefaultDecl, ExportDefaultExpr, FnDecl, ImportDecl, Module,
    ModuleDecl, ModuleItem, NamedExport, Pat, Stmt, TsEnumDecl, TsExportAssignment,
    TsImportEqualsDecl, TsInterfaceDecl, TsModuleDecl, TsModuleName, TsNamespaceBody,
    TsNamespaceDecl, TsNamespaceExportDecl, TsTypeAliasDecl, VarDecl,
};

use super::{class::Class, enums::Enum, func::Func, interface::Interface, name::Name, ty::Ty};

pub struct TsModule {
    variables: BTreeMap<Name, Ty>,
    aliases: BTreeMap<Name, Ty>,
    enums: BTreeMap<Name, Enum>,
    classes: BTreeMap<Name, Class>,
    interfaces: BTreeMap<Name, Interface>,
    functions: BTreeMap<Name, Func>,
    children: BTreeMap<Name, TsModule>,
}

impl Debug for TsModule {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_map()
            .entries(&self.variables)
            .entries(&self.enums)
            .entries(&self.classes)
            .entries(&self.interfaces)
            .entries(&self.functions)
            .entries(&self.children)
            .finish()
    }
}

impl From<Module> for TsModule {
    fn from(module: Module) -> Self {
        let mut new = TsModule::blank();
        new.add_module_contents(module.body);
        new
    }
}

impl TsModule {
    fn blank() -> Self {
        Self {
            aliases: Default::default(),
            children: Default::default(),
            classes: Default::default(),
            interfaces: Default::default(),
            enums: Default::default(),
            functions: Default::default(),
            variables: Default::default(),
        }
    }

    fn add_module_contents(&mut self, contents: Vec<ModuleItem>) {
        for item in contents {
            match item {
                ModuleItem::ModuleDecl(decl) => match decl {
                    ModuleDecl::Import(import) => self.add_import(import),
                    ModuleDecl::ExportDecl(export) => self.add_decl(export.decl),
                    ModuleDecl::ExportNamed(named) => self.add_named_export(named),
                    ModuleDecl::ExportDefaultDecl(default_decl) => {
                        self.add_default_export(default_decl)
                    }
                    ModuleDecl::ExportDefaultExpr(expr) => self.add_default_export_expr(expr),
                    ModuleDecl::ExportAll(export_all) => self.add_export_all(export_all),

                    ModuleDecl::TsImportEquals(ts_import) => self.add_import_equals(ts_import),
                    ModuleDecl::TsExportAssignment(ts_export) => self.add_export_assign(ts_export),
                    ModuleDecl::TsNamespaceExport(ts_ns_export) => {
                        self.add_namespace_export(ts_ns_export)
                    }
                },
                ModuleItem::Stmt(stmt) => self.add_stmt(stmt),
            }
        }
    }

    fn add_import(&mut self, _import: ImportDecl) {
        todo!("support imports");
    }

    pub fn add_named_export(&mut self, _named: NamedExport) {
        todo!("support re-exports");
    }

    pub fn add_default_export(&mut self, _default_decl: ExportDefaultDecl) {
        todo!("export default decl");
    }

    pub fn add_default_export_expr(&mut self, _default_expr: ExportDefaultExpr) {
        todo!("export default expr");
    }

    pub fn add_export_all(&mut self, _export_all: ExportAll) {
        todo!("export all");
    }

    pub fn add_import_equals(&mut self, _ts_import: TsImportEqualsDecl) {
        todo!("ts import");
    }

    pub fn add_export_assign(&mut self, _ts_export: TsExportAssignment) {
        todo!("export assignment");
    }

    pub fn add_namespace_export(&mut self, _ts_ns_export: TsNamespaceExportDecl) {
        todo!("typescript namespace export");
    }

    fn add_stmt(&mut self, stmt: Stmt) {
        match stmt {
            Stmt::Decl(decl) => self.add_decl(decl),
            _ => {
                todo!("non-decl statements");
            }
        }
    }

    fn add_decl(&mut self, decl: Decl) {
        match decl {
            Decl::Class(class) => self.add_class(class),
            Decl::TsInterface(interface) => self.add_interface(interface),
            Decl::TsTypeAlias(alias) => self.add_alias(alias),
            Decl::TsEnum(ts_enum) => self.add_enum(ts_enum),
            Decl::TsModule(TsModuleDecl { id, body, .. }) => self.add_ts_module(id, body),
            Decl::Fn(fun) => self.add_function(fun),
            Decl::Var(var) => self.add_var(var),
        }
    }

    fn add_var(&mut self, var: VarDecl) {
        for decl in var.decls {
            let (name, ty) = match decl.name {
                Pat::Ident(n) => (
                    Name::from(n.sym.to_string()),
                    n.type_ann.map(Ty::from).unwrap_or_else(Ty::any),
                ),
                other => {
                    todo!("i guess implement support for assignments to {:?}", other);
                }
            };

            if let Some(init) = decl.init {
                todo!("i guess support initializing {:?} {:?} {:?}", &var.kind, name, init);
            }

            self.variables.insert(name, ty);
        }
    }

    fn add_function(&mut self, fun: FnDecl) {
        let name = Name::from(fun.ident.sym.to_string());
        self.functions.insert(name, fun.function.into());
    }

    fn add_class(&mut self, class: ClassDecl) {
        self.classes.insert(class.ident.sym.to_string().into(), class.class.into());
    }

    fn add_interface(&mut self, interface: TsInterfaceDecl) {
        self.interfaces.insert(interface.id.sym.to_string().into(), interface.body.into());
    }

    fn add_alias(&mut self, alias: TsTypeAliasDecl) {
        self.aliases.insert(alias.id.sym.to_string().into(), From::from(*alias.type_ann));
    }

    fn add_enum(&mut self, decl: TsEnumDecl) {
        self.enums.insert(decl.id.sym.to_string().into(), decl.members.into());
    }

    fn add_ts_module(&mut self, id: TsModuleName, body: Option<TsNamespaceBody>) {
        let new_child = self.children.entry(module_name(&id)).or_insert_with(TsModule::blank);

        match body {
            Some(TsNamespaceBody::TsModuleBlock(block)) => {
                new_child.add_module_contents(block.body);
            }
            Some(TsNamespaceBody::TsNamespaceDecl(TsNamespaceDecl { id, body, .. })) => {
                new_child.add_ts_module(TsModuleName::Ident(id), Some(*body));
            }
            None => (),
        }
    }

    pub fn import_with_wasm_bindgen(&self) -> Result<WasmBindgenImport, TypescriptError> {
        todo!("{:?}", self)
    }
}

fn module_name(id: &TsModuleName) -> Name {
    match id {
        TsModuleName::Ident(i) => &i.sym,
        TsModuleName::Str(s) => &s.value,
    }
    .to_string()
    .into()
}